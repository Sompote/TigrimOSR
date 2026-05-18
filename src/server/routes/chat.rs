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
use std::sync::atomic::AtomicBool;
use crate::server::services::toolbox::{call_with_tools, SubAgentConfig, ToolUpdate};

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
    let session = sessions.iter_mut().find(|s| s.id == id);
    let session = match session {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Session not found"})),
            )
                .into_response();
        }
    };

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
    let sandbox_dir = settings.sandbox_dir.clone();

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
        _ => !config_file.is_empty(),
    };
    let effective_mode = match request_mode {
        "single" => settings.sub_agent_mode.clone().unwrap_or_else(|| "auto".to_string()),
        m => m.to_string(),
    };

    let sub_agent = SubAgentConfig {
        enabled: sub_agent_enabled,
        mode: effective_mode,
        session_id: id.clone(),
        agent_id: "web".to_string(),
        agent_ids: vec![],
        agent_role: String::new(),
        config_file,
        api_key: api_key.clone(),
        api_url: api_url.clone(),
        model: model.clone(),
        depth: 0,
        cancel_flag: Arc::new(AtomicBool::new(false)),
    };

    // Clear activity log before starting
    let log_dir = activity_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{}.log", id));
    let _ = std::fs::write(&log_path, "");

    // Call the AI tool loop — write progress to activity log
    let session_id_for_log = id.clone();
    let result = call_with_tools(
        &api_key,
        &api_url,
        &model,
        llm_messages,
        None,
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
            }
        },
        sub_agent,
    ).await;

    let assistant_content = result.content;

    // Save assistant response
    let mut sessions3 = get_chat_history().await;
    if let Some(s) = sessions3.iter_mut().find(|s| s.id == id) {
        s.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: assistant_content.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            files: None,
            feedback: None,
        });
        s.updated_at = chrono::Utc::now().to_rfc3339();
        save_chat_history(&sessions3).await;
    }

    (
        StatusCode::OK,
        Json(json!({
            "content": assistant_content,
            "files": result.files,
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
