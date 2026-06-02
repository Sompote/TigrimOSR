use std::sync::Arc;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::server::data::*;
use crate::server::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use crate::server::services::toolbox::{call_with_tools, SubAgentConfig, ToolUpdate};

// ---------------------------------------------------------------------------
// Global cancel flags for running web chat sessions
// ---------------------------------------------------------------------------

static CANCEL_FLAGS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>> =
    std::sync::OnceLock::new();

fn cancel_flags() -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>> {
    CANCEL_FLAGS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

// ---------------------------------------------------------------------------
// Log directories
// ---------------------------------------------------------------------------

fn activity_log_dir() -> std::path::PathBuf {
    crate::server::data::data_dir().join("activity_logs")
}

fn chat_log_dir() -> std::path::PathBuf {
    crate::server::data::data_dir().join("chat_logs")
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/bulk", get(get_all_sessions).put(put_all_sessions))
        .route(
            "/sessions/{id}",
            get(get_session).delete(delete_session).patch(rename_session),
        )
        .route("/sessions/{id}/messages", post(send_message))
        .route(
            "/sessions/{id}/messages/{index}/feedback",
            post(message_feedback),
        )
        .route("/sessions/{id}/activity", get(get_activity_log))
        .route("/sessions/{id}/chatlog", get(get_chat_log))
        .route("/active-tasks", get(get_active_tasks))
        .route("/sessions/{id}/kill", post(kill_session))
}

// ---------------------------------------------------------------------------
// GET /sessions/bulk - get all sessions with messages (for remote sync)
// ---------------------------------------------------------------------------

async fn get_all_sessions() -> Json<Vec<ChatSession>> {
    Json(get_chat_history().await)
}

// ---------------------------------------------------------------------------
// PUT /sessions/bulk - save all sessions (for remote sync)
// ---------------------------------------------------------------------------

async fn put_all_sessions(Json(sessions): Json<Vec<ChatSession>>) -> impl IntoResponse {
    save_chat_history(&sessions).await;
    StatusCode::OK
}

// ---------------------------------------------------------------------------
// GET /sessions - list all chat sessions
// ---------------------------------------------------------------------------

async fn list_sessions() -> impl IntoResponse {
    let sessions = get_chat_history().await;
    let summaries: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "createdAt": s.created_at,
                "updatedAt": s.updated_at,
                "messageCount": s.messages.len(),
            })
        })
        .collect();
    Json(json!(summaries))
}

// ---------------------------------------------------------------------------
// GET /sessions/:id - get a single session
// ---------------------------------------------------------------------------

async fn get_session(Path(id): Path<String>) -> impl IntoResponse {
    let sessions = get_chat_history().await;
    let session = sessions.iter().find(|s| s.id == id);
    match session {
        Some(s) => {
            let mut val = serde_json::to_value(s).unwrap_or(json!({}));

            // Include auto-created architecture info if present
            let auto_arch_filename = get_auto_created_architecture(&id);
            if let Some(filename) = auto_arch_filename {
                let file_path =
                    crate::server::data::data_dir().join("agents").join(&filename);
                let system_name = if let Ok(content) =
                    tokio::fs::read_to_string(&file_path).await
                {
                    // Try to parse YAML and extract system.name
                    serde_yaml::from_str::<Value>(&content)
                        .ok()
                        .and_then(|parsed| {
                            parsed
                                .get("system")
                                .and_then(|sys| sys.get("name"))
                                .and_then(|n| n.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_else(|| {
                            filename
                                .trim_end_matches(".yml")
                                .trim_end_matches(".yaml")
                                .to_string()
                        })
                } else {
                    filename
                        .trim_end_matches(".yml")
                        .trim_end_matches(".yaml")
                        .to_string()
                };

                if let Some(obj) = val.as_object_mut() {
                    obj.insert(
                        "autoCreatedArch".to_string(),
                        json!({
                            "filename": filename,
                            "systemName": system_name,
                        }),
                    );
                }
            }

            (StatusCode::OK, Json(val)).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Session not found"})),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /sessions - create a new session
// ---------------------------------------------------------------------------

async fn create_session(Json(body): Json<Value>) -> impl IntoResponse {
    let mut sessions = get_chat_history().await;
    let title = body
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("New Chat")
        .to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let session = ChatSession {
        id: Uuid::new_v4().to_string(),
        title,
        messages: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
        skill_candidate: None,
        skill_feedback: None,
        project_id: None,
    };
    sessions.push(session.clone());
    save_chat_history(&sessions).await;
    Json(serde_json::to_value(&session).unwrap_or(json!({})))
}

// ---------------------------------------------------------------------------
// DELETE /sessions/:id - delete a session
// ---------------------------------------------------------------------------

async fn delete_session(Path(id): Path<String>) -> impl IntoResponse {
    let sessions = get_chat_history().await;
    let filtered: Vec<ChatSession> = sessions.into_iter().filter(|s| s.id != id).collect();
    save_chat_history(&filtered).await;
    // Clean up agent history folder for this session
    delete_agent_history(&id).await;
    Json(json!({"success": true}))
}

// ---------------------------------------------------------------------------
// PATCH /sessions/:id - rename a session
// ---------------------------------------------------------------------------

async fn rename_session(
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut sessions = get_chat_history().await;
    let session = sessions.iter_mut().find(|s| s.id == id);
    match session {
        Some(s) => {
            if let Some(title) = body.get("title").and_then(|v| v.as_str()) {
                s.title = title.to_string();
            }
            let updated = s.clone();
            save_chat_history(&sessions).await;
            (StatusCode::OK, Json(serde_json::to_value(&updated).unwrap_or(json!({})))).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Session not found"})),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /sessions/:id/messages/:index/feedback - save feedback on a message
// ---------------------------------------------------------------------------

async fn message_feedback(
    Path((session_id, index_str)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let index: usize = match index_str.parse() {
        Ok(i) => i,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "Invalid message index"})),
            )
                .into_response();
        }
    };

    let mut sessions = get_chat_history().await;
    let session = sessions.iter_mut().find(|s| s.id == session_id);
    let session = match session {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"ok": false, "error": "Session not found"})),
            )
                .into_response();
        }
    };

    if index >= session.messages.len() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Invalid message index"})),
        )
            .into_response();
    }

    let rating = body
        .get("rating")
        .and_then(|v| v.as_str())
        .filter(|r| *r == "up" || *r == "down")
        .map(|s| s.to_string());
    let comment = body
        .get("comment")
        .and_then(|v| v.as_str())
        .map(|s| s.chars().take(4000).collect::<String>());
    let clear = body
        .get("clear")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if rating.is_none() && comment.is_none() && !clear {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Provide rating, comment, or clear=true"})),
        )
            .into_response();
    }

    let msg = &mut session.messages[index];
    if clear {
        msg.feedback = None;
    } else {
        let existing = msg.feedback.take().unwrap_or(ChatMessageFeedback {
            rating: None,
            comment: None,
            submitted_at: None,
        });
        msg.feedback = Some(ChatMessageFeedback {
            rating: rating.or(existing.rating),
            comment: comment.or(existing.comment),
            submitted_at: Some(chrono::Utc::now().to_rfc3339()),
        });
    }

    session.updated_at = chrono::Utc::now().to_rfc3339();
    let feedback = msg.feedback.clone();
    save_chat_history(&sessions).await;

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "feedback": feedback,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// POST /sessions/:id/messages - send a message
// ---------------------------------------------------------------------------

async fn send_message(
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut sessions = get_chat_history().await;

    // Auto-create session if it doesn't exist (e.g. desktop remote mode sends with local ID)
    if !sessions.iter().any(|s| s.id == id) {
        let now = chrono::Utc::now().to_rfc3339();
        sessions.push(ChatSession {
            id: id.clone(),
            title: "Remote Chat".to_string(),
            messages: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            skill_candidate: None,
            skill_feedback: None,
            project_id: None,
        });
        save_chat_history(&sessions).await;
    }

    let session = sessions.iter_mut().find(|s| s.id == id).unwrap();

    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let now = chrono::Utc::now().to_rfc3339();

    // Save user message
    session.messages.push(ChatMessage {
        role: "user".to_string(),
        content: message.clone(),
        timestamp: now.clone(),
        files: None,
        feedback: None,
    });

    // Update title from first message
    if session.messages.len() == 1 {
        session.title = message.chars().take(60).collect();
    }

    session.updated_at = chrono::Utc::now().to_rfc3339();
    save_chat_history(&sessions).await;

    // Load settings for API credentials
    let settings = get_settings().await;
    let api_key = settings.tiger_bot_api_key.clone();
    let model = if settings.tiger_bot_model.is_empty() {
        "deepseek-chat".to_string()
    } else {
        settings.tiger_bot_model.clone()
    };
    let raw_url = settings
        .tiger_bot_api_url
        .clone()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "https://api.deepseek.com/v1".to_string());
    let api_url = if raw_url == "claude-code" {
        raw_url
    } else if raw_url.ends_with("/chat/completions") {
        raw_url
    } else {
        format!("{}/chat/completions", raw_url.trim_end_matches('/'))
    };
    let sandbox_dir = crate::server::data::get_sandbox_dir_sync();

    if api_key.is_empty() {
        let err = "API key not configured. Set it in Settings > AI Configuration.".to_string();
        // Save error as assistant message
        let mut sessions2 = get_chat_history().await;
        if let Some(s) = sessions2.iter_mut().find(|s| s.id == id) {
            s.messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: err.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                files: None,
                feedback: None,
            });
            s.updated_at = chrono::Utc::now().to_rfc3339();
            save_chat_history(&sessions2).await;
        }
        return (StatusCode::OK, Json(json!({ "content": err }))).into_response();
    }

    // Build message history for LLM
    let sessions_snap = get_chat_history().await;
    let session_snap = sessions_snap.iter().find(|s| s.id == id);
    let llm_messages: Vec<Value> = session_snap
        .map(|s| {
            s.messages.iter().map(|m| {
                json!({ "role": m.role, "content": m.content })
            }).collect()
        })
        .unwrap_or_default();

    let config_file = settings.sub_agent_config_file.clone().unwrap_or_default();
    // Per-request agent_mode overrides settings: "single" disables sub-agents,
    // "auto"/"manual"/"fully_auto" enable them (requires config file).
    let request_mode = body.get("agent_mode").and_then(|v| v.as_str()).unwrap_or("single");
    let sub_agent_enabled = match request_mode {
        "single" => false,
        "fully_auto" | "auto_swarm" => true, // these create their own config
        _ => !config_file.is_empty(),
    };
    tracing::info!("[chat] request_mode={}, config_file='{}', sub_agent_enabled={}", request_mode, config_file, sub_agent_enabled);
    let effective_mode = match request_mode {
        "single" => settings.sub_agent_mode.clone().unwrap_or_else(|| "auto".to_string()),
        m => m.to_string(),
    };

    // Load agent IDs from YAML config (same as native UI)
    let agent_ids = if sub_agent_enabled && !config_file.is_empty() {
        crate::server::services::toolbox::load_agent_yaml(&config_file)
            .map(|(_, ids)| ids)
            .unwrap_or_default()
    } else {
        vec![]
    };

    let sub_agent_model = settings.sub_agent_model.clone()
        .unwrap_or_else(|| model.clone());

    let mut sub_agent = SubAgentConfig {
        enabled: sub_agent_enabled,
        mode: effective_mode.clone(),
        session_id: id.clone(),
        agent_id: "main".to_string(),
        agent_ids,
        agent_role: "orchestrator".to_string(),
        config_file,
        api_key: api_key.clone(),
        api_url: api_url.clone(),
        model: sub_agent_model,
        depth: 0,
        cancel_flag: Arc::new(AtomicBool::new(false)),
    };

    // Register cancel flag so kill endpoint can abort this session
    {
        let mut flags = cancel_flags().lock().unwrap();
        flags.insert(id.clone(), sub_agent.cancel_flag.clone());
    }

    // Fully Auto pre-flight: create architecture + boot realtime session (same as native UI)
    if sub_agent_enabled && effective_mode == "fully_auto" {
        use crate::server::services::toolbox::{
            get_session_architecture, force_create_architecture, start_realtime_session,
        };

        let user_msg = llm_messages.last()
            .and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_string();

        // Step 1: Get or create architecture
        let config_file = match get_session_architecture(&id).await {
            Some(existing) => Some(existing),
            None => {
                let (ok, cf, _msg) = force_create_architecture(&user_msg, &sub_agent, &sandbox_dir).await;
                if ok { cf } else { None }
            }
        };

        if let Some(ref cf) = config_file {
            sub_agent.config_file = cf.clone();

            // Load agent IDs from the created YAML
            if let Some((_, ids)) = crate::server::services::toolbox::load_agent_yaml(cf) {
                sub_agent.agent_ids = ids;
            }

            // Step 2: Boot realtime session
            start_realtime_session(
                &sub_agent.session_id,
                cf,
                &sub_agent.api_key,
                &sub_agent.api_url,
                &sub_agent.model,
                &sandbox_dir,
            ).await;
        }
    }

    // For manual/auto modes with a config file, boot realtime session
    if sub_agent_enabled && effective_mode != "fully_auto" && !sub_agent.config_file.is_empty() {
        use crate::server::services::toolbox::start_realtime_session;
        start_realtime_session(
            &sub_agent.session_id,
            &sub_agent.config_file,
            &sub_agent.api_key,
            &sub_agent.api_url,
            &sub_agent.model,
            &sandbox_dir,
        ).await;
    }

    // Build system prompt — same as native UI (identity, soul, skills, tools)
    let system_prompt = {
        let tool_list = if sub_agent_enabled {
            match effective_mode.as_str() {
                "auto" => "create_architecture, send_task, wait_result, check_agents, web_search, fetch_url, run_python, run_shell, read_file, write_file, list_files, list_skills, load_skill",
                "fully_auto" => "create_architecture, send_task, wait_result, check_agents, run_python, write_file",
                "manual" => "send_task, wait_result, check_agents, run_python, write_file, read_file, list_files",
                _ => "web_search, fetch_url, run_python, run_shell, read_file, write_file, list_files, list_skills, load_skill, spawn_subagent",
            }
        } else {
            "web_search, fetch_url, run_python, run_shell, read_file, write_file, list_files, list_skills, load_skill, spawn_subagent"
        };

        let sub_agent_prompt = if sub_agent_enabled {
            "\n- You are the orchestrator. Delegate complex multi-step tasks to sub-agents using send_task."
        } else {
            ""
        };

        let research_instruction = "For research tasks, gather info from multiple sources using web_search/fetch_url, then synthesize results";

        let mut base = format!(
            "You are TigrimOS, an AI assistant with tools for search, code execution, files, and skills.\n\
Rules:\n\
- Always use tools to produce real results — never just describe what you would do.\n\
- If a tool call fails, analyze the error, fix it, and retry. Try a different approach after two failures.\n\
- Do not call the same tool with identical arguments repeatedly.\n\
- Before writing code, check if an installed skill matches the task. If so, call load_skill first and use its implementation.\n\
- For web search, prefer installed search skills (e.g. web-search, duckduckgo-search) via load_skill + run_python over the built-in web_search tool. If results are limited, follow up with fetch_url.\n\
- {}\n\
- Your working directory is the sandbox folder '{}'. All file operations use this directory as the root.\n\
- When a user asks about files, use list_files first to see what's available.\n\
- Use run_python for data analysis, charts, and calculations.\n\
- Use run_shell for system commands.\n\
- When you generate files (charts, images, data), always describe the results in your response. Explain what each figure shows and summarize key findings. Never respond with just a greeting after completing tool work.\n\
You have access to these tools: {}.{}",
            research_instruction, sandbox_dir, tool_list, sub_agent_prompt
        );

        // Append installed skills
        let skills_block = crate::server::services::toolbox::build_enabled_skills_block_pub(None).await;
        if !skills_block.is_empty() {
            base.push_str(&skills_block);
        }

        // Inject Soul & Identity from data dir
        let data_dir = crate::server::data::data_dir();
        if let Ok(soul) = std::fs::read_to_string(data_dir.join("SOUL.md")) {
            if !soul.trim().is_empty() {
                base.push_str(&format!("\n\n=== SOUL.md (Internal Cognition & Behavioral Prior) ===\n{}", soul));
            }
        }
        if let Ok(identity) = std::fs::read_to_string(data_dir.join("IDENTITY.md")) {
            if !identity.trim().is_empty() {
                base.push_str(&format!("\n\n=== IDENTITY.md (External Presentation) ===\n{}", identity));
            }
        }

        Some(base)
    };

    // Register in native UI's active tasks so local Tasks view sees web chats
    {
        let title = {
            let sessions_snap = get_chat_history().await;
            sessions_snap.iter().find(|s| s.id == id)
                .map(|s| s.title.clone())
                .unwrap_or_else(|| "Web Chat".to_string())
        };
        let mut chats = crate::ui::tasks_view::active_chats().lock().unwrap();
        chats.retain(|c| c.session_id != id);
        chats.push(crate::ui::tasks_view::ActiveChatSession {
            session_id: id.clone(),
            title,
            started_at: chrono::Utc::now(),
            agent_count: 0,
            tool_calls: 0,
        });
    }

    // Clear activity log before starting
    let log_dir = activity_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{}.log", id));
    let _ = std::fs::write(&log_path, "");

    // Initialize chat log before starting (so it's not empty during the run)
    let cl_dir = chat_log_dir();
    let _ = std::fs::create_dir_all(&cl_dir);
    let cl_path = cl_dir.join(format!("{}.log", id));
    let _ = std::fs::write(&cl_path, format!(
        "[{}] === Web Chat Session ===\n",
        chrono::Utc::now().format("%H:%M:%S"),
    ));

    // Call the AI tool loop — write progress to activity log + chat log
    let session_id_for_log = id.clone();
    let chat_log_path_for_cb = cl_path.clone();
    let result = call_with_tools(
        &api_key,
        &api_url,
        &model,
        llm_messages,
        system_prompt,
        &sandbox_dir,
        move |update: ToolUpdate| {
            use crate::server::services::toolbox::append_session_progress;
            let line = match &update {
                ToolUpdate::ToolCall { name, args } => {
                    let preview: String = args.to_string().chars().take(120).collect();
                    format!("🔧 Calling **{}** — {}\n", name, preview)
                }
                ToolUpdate::ToolResult { name, result } => {
                    let preview: String = result.to_string().chars().take(200).collect();
                    format!("✅ **{}** done — {}\n", name, preview)
                }
                ToolUpdate::TextChunk(text) => {
                    if text.starts_with("[reasoning]") {
                        format!("💭 Reasoning...\n")
                    } else {
                        String::new() // final text, skip
                    }
                }
                ToolUpdate::Error(err) => format!("❌ {}\n", err),
                ToolUpdate::ApprovalRequired { name, .. } => {
                    format!("⚠️ Approval needed for **{}**\n", name)
                }
            };
            if !line.is_empty() {
                append_session_progress(&session_id_for_log, &line);
                // Also write to chat log in real-time
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .append(true).open(&chat_log_path_for_cb)
                {
                    let _ = f.write_all(line.as_bytes());
                }
            }
        },
        sub_agent,
    ).await;

    // Remove from native UI's active tasks and cancel flags
    {
        let mut chats = crate::ui::tasks_view::active_chats().lock().unwrap();
        chats.retain(|c| c.session_id != id);
    }
    {
        let mut flags = cancel_flags().lock().unwrap();
        flags.remove(&id);
    }

    // Append completion footer to chat log (content was written in real-time)
    {
        let footer = format!(
            "[{}] === Response complete ===\n\n",
            chrono::Utc::now().format("%H:%M:%S"),
        );
        use tokio::io::AsyncWriteExt;
        if let Ok(mut f) = tokio::fs::OpenOptions::new()
            .append(true).open(&cl_path).await
        {
            let _ = f.write_all(footer.as_bytes()).await;
        }
    }

    let assistant_content = result.content;
    let output_files = result.files.clone();

    // Save assistant response
    let mut sessions3 = get_chat_history().await;
    if let Some(s) = sessions3.iter_mut().find(|s| s.id == id) {
        s.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: assistant_content.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            files: if output_files.is_empty() { None } else { Some(output_files.clone()) },
            feedback: None,
        });
        s.updated_at = chrono::Utc::now().to_rfc3339();
        save_chat_history(&sessions3).await;
    }

    (
        StatusCode::OK,
        Json(json!({
            "content": assistant_content,
            "files": output_files,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /sessions/:id/activity - get activity log for a session
// ---------------------------------------------------------------------------

async fn get_activity_log(Path(id): Path<String>) -> impl IntoResponse {
    let log_path = activity_log_dir().join(format!("{}.log", id));
    let content = tokio::fs::read_to_string(&log_path)
        .await
        .unwrap_or_default();
    Json(json!({"ok": true, "content": content}))
}

// ---------------------------------------------------------------------------
// GET /active-tasks — list sessions that are currently processing (recent activity)
// ---------------------------------------------------------------------------

async fn get_active_tasks() -> impl IntoResponse {
    let log_dir = activity_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let mut active = Vec::new();

    if let Ok(mut entries) = tokio::fs::read_dir(&log_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".log") { continue; }
            let session_id = name.trim_end_matches(".log").to_string();

            if let Ok(meta) = entry.metadata().await {
                if let Ok(modified) = meta.modified() {
                    let age = modified.elapsed().unwrap_or_default().as_secs();
                    // Consider active if modified in last 120s and file is non-empty
                    if age < 120 && meta.len() > 0 {
                        let content = tokio::fs::read_to_string(entry.path())
                            .await
                            .unwrap_or_default();
                        // Look up session title
                        let sessions = get_chat_history().await;
                        let title = sessions.iter()
                            .find(|s| s.id == session_id)
                            .map(|s| s.title.clone())
                            .unwrap_or_else(|| session_id.clone());
                        active.push(json!({
                            "session_id": session_id,
                            "title": title,
                            "activity": content,
                            "age_secs": age,
                        }));
                    }
                }
            }
        }
    }

    // Sort by most recent first
    active.sort_by(|a, b| {
        a["age_secs"].as_u64().unwrap_or(999).cmp(&b["age_secs"].as_u64().unwrap_or(999))
    });

    Json(json!({ "tasks": active }))
}

// ---------------------------------------------------------------------------
// GET /sessions/:id/chatlog - get chat log for a session
// ---------------------------------------------------------------------------

async fn get_chat_log(Path(id): Path<String>) -> impl IntoResponse {
    let log_path = chat_log_dir().join(format!("{}.log", id));
    let content = tokio::fs::read_to_string(&log_path)
        .await
        .unwrap_or_default();
    Json(json!({"ok": true, "content": content}))
}

// ---------------------------------------------------------------------------
// POST /sessions/:id/kill — cancel a running chat session
// ---------------------------------------------------------------------------

async fn kill_session(Path(id): Path<String>) -> impl IntoResponse {
    let killed = {
        let flags = cancel_flags().lock().unwrap();
        if let Some(flag) = flags.get(&id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    };

    // Also push to native UI's killed list
    if killed {
        let killed_ids = crate::ui::tasks_view::killed_chat_ids();
        let mut ids = killed_ids.lock().unwrap();
        if !ids.contains(&id) {
            ids.push(id.clone());
        }
    }

    Json(json!({ "ok": killed, "session_id": id }))
}

// ---------------------------------------------------------------------------
// Helper: check if a session has an auto-created architecture file
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Helper: check if a session has an auto-created architecture file
// ---------------------------------------------------------------------------

fn get_auto_created_architecture(session_id: &str) -> Option<String> {
    // Look for a YAML file in data/agents that matches the session ID
    let agents_dir = crate::server::data::data_dir().join("agents");
    let pattern = format!("{}.yml", session_id);
    let pattern_yaml = format!("{}.yaml", session_id);

    if agents_dir.join(&pattern).exists() {
        Some(pattern)
    } else if agents_dir.join(&pattern_yaml).exists() {
        Some(pattern_yaml)
    } else {
        None
    }
}
