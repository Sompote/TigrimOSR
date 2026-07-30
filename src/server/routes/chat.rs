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
use crate::server::services::toolbox::{call_with_tools, SubAgentConfig, ToolUpdate};
use crate::server::AppState;
use std::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// Global cancel flags for running web chat sessions
// ---------------------------------------------------------------------------

static CANCEL_FLAGS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>,
> = std::sync::OnceLock::new();

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
        .route(
            "/sessions/bulk",
            get(get_all_sessions).put(put_all_sessions),
        )
        .route(
            "/sessions/{id}",
            get(get_session)
                .delete(delete_session)
                .patch(rename_session),
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
        .route("/agent-configs", get(list_agent_configs))
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
                let file_path = crate::server::data::data_dir()
                    .join("agents")
                    .join(&filename);
                let system_name = if let Ok(content) = tokio::fs::read_to_string(&file_path).await {
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
    let project_id = body
        .get("projectId")
        .or_else(|| body.get("project_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let now = chrono::Utc::now().to_rfc3339();
    let session = ChatSession {
        id: Uuid::new_v4().to_string(),
        title,
        messages: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
        skill_candidate: None,
        skill_feedback: None,
        project_id,
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

async fn rename_session(Path(id): Path<String>, Json(body): Json<Value>) -> impl IntoResponse {
    let mut sessions = get_chat_history().await;
    let session = sessions.iter_mut().find(|s| s.id == id);
    match session {
        Some(s) => {
            if let Some(title) = body.get("title").and_then(|v| v.as_str()) {
                s.title = title.to_string();
            }
            let updated = s.clone();
            save_chat_history(&sessions).await;
            (
                StatusCode::OK,
                Json(serde_json::to_value(&updated).unwrap_or(json!({}))),
            )
                .into_response()
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
    let clear = body.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);

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

async fn send_message(Path(id): Path<String>, Json(body): Json<Value>) -> impl IntoResponse {
    let req = AgentRunRequest {
        session_id: id.clone(),
        message: body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        session_title: None,
        agent_mode: body
            .get("agent_mode")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        agent_loop_profile: body
            .get("agent_loop_profile")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        graph_profile: body
            .get("graph_profile")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        config_file: body
            .get("config_file")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        project_id: body
            .get("projectId")
            .or_else(|| body.get("project_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    };
    match start_agent_run(req, None, None).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "status": "processing", "session_id": id })),
        )
            .into_response(),
        Err(err) => (StatusCode::OK, Json(json!({ "content": err }))).into_response(),
    }
}

/// Per-run parameters for a headless agent run — mirrors the fields the web
/// chat endpoint accepts in its JSON body so other channels (Telegram/LINE
/// bots) can drive the identical pipeline.
pub struct AgentRunRequest {
    pub session_id: String,
    pub message: String,
    /// Title used when the session is auto-created.
    pub session_title: Option<String>,
    pub agent_mode: Option<String>,
    pub agent_loop_profile: Option<String>,
    /// Graph-mode profile filename override (data/graph/*.yaml).
    pub graph_profile: Option<String>,
    pub config_file: Option<String>,
    pub project_id: Option<String>,
}

/// Run one chat turn as a workflow DAG instead of the normal sub-agent loop.
///
/// Returns the same shape as `call_with_tools` so everything downstream —
/// history, output files, the completion footer — is untouched.
///
/// Shared with the desktop UI (`src/ui/chat.rs`) so both front-ends dispatch
/// the same patterns through the same executor — a pattern reachable from one
/// UI but not the other is the bug this is guarding against.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_workflow_turn(
    pattern: &str,
    width: usize,
    llm_messages: &[serde_json::Value],
    api_key: &str,
    api_url: &str,
    model: &str,
    sandbox_dir: &str,
    session_id: &str,
    model_pool: Vec<crate::server::data::ModelPoolEntry>,
) -> crate::server::services::toolbox::ToolLoopResult {
    use crate::server::services::toolbox::{append_session_progress, ToolLoopResult};
    use crate::server::services::workflow::{self, WorkflowContext};

    // The task is the latest user turn; earlier turns are already summarised
    // into the session, and a DAG node takes a single prompt.
    let task = llm_messages
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string();

    let profile = match workflow::build_pattern(pattern, width) {
        Ok(p) => p,
        Err(e) => {
            return ToolLoopResult {
                content: format!("⚠️ Could not build the '{pattern}' workflow: {e}"),
                tool_results: Vec::new(),
                files: Vec::new(),
            }
        }
    };

    let plan = profile
        .levels()
        .map(|ls| {
            ls.iter()
                .enumerate()
                .map(|(i, l)| {
                    let names: Vec<&str> =
                        l.iter().map(|&n| profile.nodes[n].name.as_str()).collect();
                    format!("  {}. {}", i + 1, names.join(", "))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    append_session_progress(
        session_id,
        &format!(
            "⬡ **{}** — {} nodes\n{}\n",
            profile.name,
            profile.nodes.len(),
            plan
        ),
    );

    let sid = session_id.to_string();
    let sink: workflow::ProgressSink =
        std::sync::Arc::new(move |line: String| append_session_progress(&sid, &line));

    let ctx = WorkflowContext {
        api_key: api_key.to_string(),
        api_url: api_url.to_string(),
        model: model.to_string(),
        sandbox_dir: sandbox_dir.to_string(),
        session_id: session_id.to_string(),
        model_pool,
    };

    match workflow::run_with_agent_loop(&profile, &task, &ctx, Some(sink)).await {
        Ok(run) => {
            // A partial run is still worth returning — the failed nodes are
            // named so the answer is not silently short.
            let failed: Vec<&str> = run
                .outcomes
                .iter()
                .filter(|o| !o.ok)
                .map(|o| o.name.as_str())
                .collect();
            let mut content = run.final_output;
            if !failed.is_empty() {
                content.push_str(&format!(
                    "\n\n---\n⚠️ {} node(s) failed and were excluded: {}",
                    failed.len(),
                    failed.join(", ")
                ));
            }
            ToolLoopResult {
                content,
                tool_results: Vec::new(),
                files: Vec::new(),
            }
        }
        Err(e) => ToolLoopResult {
            content: format!("⚠️ The '{pattern}' workflow could not run: {e}"),
            tool_results: Vec::new(),
            files: Vec::new(),
        },
    }
}

/// Run one agent turn against a chat session, headless. Extracted from the
/// send_message handler: session persistence, cancel-flag registration,
/// pre-flight, system prompt, and activity/chat logs all behave exactly as
/// they do for web chat.
///
/// `extra_on_update` is invoked with every ToolUpdate in addition to the
/// built-in log writer. `done_tx` fires with the final assistant text after
/// it has been persisted. Err(text) means the run could not start; the text
/// has already been persisted as an assistant message.
pub async fn start_agent_run(
    req: AgentRunRequest,
    extra_on_update: Option<Arc<dyn Fn(ToolUpdate) + Send + Sync>>,
    done_tx: Option<tokio::sync::oneshot::Sender<String>>,
) -> Result<(), String> {
    let id = req.session_id.clone();
    let mut sessions = get_chat_history().await;

    // Auto-create session if it doesn't exist (e.g. desktop remote mode sends with local ID)
    if !sessions.iter().any(|s| s.id == id) {
        let now = chrono::Utc::now().to_rfc3339();
        sessions.push(ChatSession {
            id: id.clone(),
            title: req
                .session_title
                .clone()
                .unwrap_or_else(|| "Remote Chat".to_string()),
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

    let message = req.message.clone();

    // Active project: request wins, else fall back to the session's stored
    // project. Persist it so later messages in this session stay project-scoped.
    let project_id = req
        .project_id
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| session.project_id.clone());
    session.project_id = project_id.clone();

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
    let mut api_url = if raw_url == "claude-code" {
        raw_url
    } else if raw_url.ends_with("/chat/completions") {
        raw_url
    } else {
        format!("{}/chat/completions", raw_url.trim_end_matches('/'))
    };
    let mut api_key = api_key;
    let mut model = model;
    let mut sandbox_dir = crate::server::data::get_sandbox_dir_sync();

    // Project run-context: working folder → sandbox, memory/instructions → prompt,
    // agent override → config file, assigned skills → installed-skills filter.
    let project_ctx = match &project_id {
        Some(pid) => crate::server::routes::projects::load_project_run_context(pid).await,
        None => None,
    };
    if let Some(ref ctx) = project_ctx {
        if let Some(ref folder) = ctx.sandbox_dir {
            let _ = std::fs::create_dir_all(folder);
            sandbox_dir = folder.clone();
        }
    }

    // Agent-loop profile: per-request body > project override > settings.
    // (Resolved before the graph gate so the profile's `graph:` knobs can
    // turn the gate on/off; a graph worker.agent_loop_profile override is
    // applied after gate resolution below.)
    let mut loop_profile = {
        use crate::server::services::agent_loop;
        let request_profile = req
            .agent_loop_profile
            .as_deref()
            .filter(|s| !s.trim().is_empty());
        match request_profile {
            Some(name) => agent_loop::load_profile(name),
            None => agent_loop::resolve_active_profile(
                settings.agent_loop_profile.as_deref(),
                project_ctx
                    .as_ref()
                    .and_then(|c| c.agent_loop_profile.as_deref()),
            ),
        }
        .map(Arc::new)
    };

    // Graph gate activation — DEFAULT OFF. On when:
    //  1. the "graph" mode is explicitly selected (request or settings), or
    //  2. the agent-loop profile says `graph.enabled: true`, or
    //  3. the global graphEnabled settings toggle is on
    // (profile `graph.enabled: false` overrides the settings toggle).
    // In explicit graph mode the loop runs in the graph profile's worker.mode;
    // toggle-activated gates keep the user's chosen mode untouched.
    let initial_request_mode = req.agent_mode.as_deref().unwrap_or("single");
    let settings_graph = settings.sub_agent_mode.as_deref() == Some("graph");
    let mode_graph =
        initial_request_mode == "graph" || (initial_request_mode == "single" && settings_graph);
    let profile_graph_knobs = loop_profile.as_deref().and_then(|p| p.graph.clone());
    let gate_on = mode_graph
        || profile_graph_knobs
            .as_ref()
            .and_then(|g| g.enabled)
            .unwrap_or_else(|| settings.graph_enabled.unwrap_or(false));
    let graph_profile_arc: Option<Arc<crate::server::services::graph::GraphProfile>> = if gate_on {
        use crate::server::services::graph;
        graph::ensure_default_profile();
        // Graph profile file: loop-profile knobs > request > project > settings.
        let knobs_profile = profile_graph_knobs
            .as_ref()
            .and_then(|g| g.profile.as_deref())
            .filter(|s| !s.trim().is_empty());
        let resolved = graph::resolve_active_profile(
            settings.graph_profile.as_deref(),
            project_ctx
                .as_ref()
                .and_then(|c| c.graph_profile.as_deref()),
            knobs_profile.or(req.graph_profile.as_deref()),
        )
        .unwrap_or_else(graph::default_profile);
        tracing::info!(
            "[chat] graph gate ON ({}): profile '{}', worker mode '{}', {} judge(s)",
            if mode_graph {
                "graph mode"
            } else {
                "toggle/profile"
            },
            resolved.name,
            resolved.worker_mode(),
            resolved.judges.len()
        );
        Some(Arc::new(resolved))
    } else {
        None
    };

    // Explicit graph mode: the graph profile's worker may name its own
    // agent-loop profile; it applies unless the request pinned one.
    if mode_graph {
        if let Some(worker_profile) = graph_profile_arc
            .as_deref()
            .and_then(|p| p.worker.as_ref())
            .and_then(|w| w.agent_loop_profile.as_deref())
            .filter(|s| !s.trim().is_empty())
        {
            if req
                .agent_loop_profile
                .as_deref()
                .map(|s| s.trim().is_empty())
                .unwrap_or(true)
            {
                if let Some(p) = crate::server::services::agent_loop::load_profile(worker_profile) {
                    loop_profile = Some(Arc::new(p));
                }
            }
        }
    }
    if let Some(m) = loop_profile.as_deref().and_then(|p| p.model.as_ref()) {
        if !m.model.trim().is_empty() {
            model = m.model.trim().to_string();
        }
        if !m.api_key.trim().is_empty() {
            api_key = m.api_key.trim().to_string();
        }
        if !m.api_url.trim().is_empty() {
            let raw = m.api_url.trim().to_string();
            api_url = if crate::server::services::cli_models::is_local_cli_url(&raw)
                || raw.ends_with("/chat/completions")
            {
                raw
            } else {
                format!("{}/chat/completions", raw.trim_end_matches('/'))
            };
        }
    }

    // Local CLI backends authenticate through their own login, so demanding an
    // API key here would make them unusable from chat.
    if api_key.is_empty() && !crate::server::services::cli_models::is_local_cli_url(&api_url) {
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
        return Err(err);
    }

    // Build message history for LLM
    let sessions_snap = get_chat_history().await;
    let session_snap = sessions_snap.iter().find(|s| s.id == id);
    let llm_messages: Vec<Value> = session_snap
        .map(|s| {
            s.messages
                .iter()
                .map(|m| json!({ "role": m.role, "content": m.content }))
                .collect()
        })
        .unwrap_or_default();

    // config_file: prefer request, fall back to server settings
    let config_file = req
        .config_file
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            project_ctx
                .as_ref()
                .and_then(|c| c.agent_config_file.clone())
        })
        .or_else(|| settings.sub_agent_config_file.clone())
        .unwrap_or_default();
    // Per-request agent_mode overrides settings: "single" disables sub-agents,
    // "auto"/"manual"/"fully_auto" enable them (requires config file).
    // An explicit "graph" request is rewritten to the profile's worker mode —
    // the gate itself rides on SubAgentConfig.graph_profile below.
    let request_mode = req.agent_mode.as_deref().unwrap_or("single");

    // A workflow pattern ("tournament", "fanout_and_synthesize", ...) runs as a
    // DAG of agent nodes rather than the normal sub-agent loop. Captured here
    // and dispatched at the run site; like "graph", it must not leak into
    // SubAgentConfig.mode, which only understands the swarm modes.
    let workflow_pattern: Option<String> = crate::server::services::workflow::pattern_catalog()
        .iter()
        .find(|(id, _)| *id == request_mode)
        .map(|(id, _)| id.to_string());

    let request_mode: &str = if request_mode == "graph" {
        graph_profile_arc
            .as_deref()
            .map(|p| p.worker_mode())
            .unwrap_or("single")
    } else if workflow_pattern.is_some() {
        // Each node runs as its own single agent; the topology does the work.
        "single"
    } else {
        request_mode
    };
    let sub_agent_enabled = match request_mode {
        "single" => false,
        "fully_auto" | "auto_swarm" | "router" => true, // these create their own config (router triages)
        "auto" | "manual" => {
            if !config_file.is_empty() {
                true
            } else {
                // No config file → fall back to fully_auto behavior
                tracing::info!(
                    "[chat] mode='{}' but no config file, falling back to fully_auto",
                    request_mode
                );
                true
            }
        }
        _ => !config_file.is_empty(),
    };
    tracing::info!(
        "[chat] request_mode={}, config_file='{}', sub_agent_enabled={}",
        request_mode,
        config_file,
        sub_agent_enabled
    );
    let effective_mode = match request_mode {
        // "graph" must never leak into SubAgentConfig.mode — substitute the
        // profile's worker mode (the gate rides on graph_profile instead).
        "single" => match settings.sub_agent_mode.as_deref() {
            Some("graph") => graph_profile_arc
                .as_deref()
                .map(|p| p.worker_mode().to_string())
                .unwrap_or_else(|| "auto".to_string()),
            Some(m) => m.to_string(),
            None => "auto".to_string(),
        },
        // auto/manual without config file → use fully_auto
        "auto" | "manual" if config_file.is_empty() => "fully_auto".to_string(),
        m => m.to_string(),
    };

    // Router is self-contained — ignore any pre-set YAML team; it triages and
    // builds its own LLM-assigned team via create_architecture.
    let config_file = if effective_mode == "router" {
        String::new()
    } else {
        config_file
    };

    // Load agent IDs from YAML config (same as native UI)
    let agent_ids = if sub_agent_enabled && !config_file.is_empty() {
        crate::server::services::toolbox::load_agent_yaml(&config_file)
            .map(|(_, ids)| ids)
            .unwrap_or_default()
    } else {
        vec![]
    };

    let sub_agent_model = settings
        .sub_agent_model
        .clone()
        .unwrap_or_else(|| model.clone());

    // Router mode: heterogeneous model pool + tier from settings (empty otherwise).
    let (router_pool, router_tier) = if effective_mode == "router" {
        let pool = settings
            .model_pool
            .clone()
            .unwrap_or_default()
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect::<Vec<_>>();
        let tier = settings
            .router_tier
            .clone()
            .unwrap_or_else(|| "fast".into());
        (pool, tier)
    } else {
        (Vec::new(), String::new())
    };

    let mut sub_agent = SubAgentConfig {
        enabled: sub_agent_enabled,
        effort: settings.reasoning_effort.clone().unwrap_or_default(),
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
        model_pool: router_pool,
        router_tier,
        loop_profile: loop_profile.clone(),
        graph_profile: graph_profile_arc.clone(),
    };

    // Register cancel flag so kill endpoint can abort this session
    {
        let mut flags = cancel_flags().lock().unwrap();
        flags.insert(id.clone(), sub_agent.cancel_flag.clone());
    }

    // Clear activity log and initialize chat log BEFORE pre-flight, so
    // architecture-creation failures are visible to the web client.
    {
        let log_dir = activity_log_dir();
        let _ = std::fs::create_dir_all(&log_dir);
        let log_path = log_dir.join(format!("{}.log", id));
        let _ = std::fs::write(&log_path, "");
        let cl_dir = chat_log_dir();
        let _ = std::fs::create_dir_all(&cl_dir);
        let cl_path = cl_dir.join(format!("{}.log", id));
        let _ = std::fs::write(
            &cl_path,
            format!(
                "[{}] === Web Chat Session ===\n",
                chrono::Utc::now().format("%H:%M:%S"),
            ),
        );
    }
    let append_chat_log = |id: &str, line: &str| {
        use std::io::Write;
        let cl_path = chat_log_dir().join(format!("{}.log", id));
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&cl_path) {
            let _ = f.write_all(line.as_bytes());
        }
    };

    if sub_agent_enabled && request_mode == "manual" && sub_agent.config_file.is_empty() {
        append_chat_log(&id, "⚠️ Manual mode requested but no agent config file is set on this server — falling back to fully-auto (an agent team will be generated).\n");
    }

    // Fully Auto pre-flight: create architecture + boot realtime session (same as native UI)
    if sub_agent_enabled && effective_mode == "fully_auto" {
        use crate::server::services::toolbox::{
            force_create_architecture, get_session_architecture, start_realtime_session,
        };

        let user_msg = llm_messages
            .last()
            .and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_string();

        // Step 1: Get or create architecture
        let config_file = match get_session_architecture(&id).await {
            Some(existing) => Some(existing),
            None => {
                let (ok, cf, _msg) =
                    force_create_architecture(&user_msg, &sub_agent, &sandbox_dir).await;
                if ok {
                    cf
                } else {
                    None
                }
            }
        };

        if let Some(ref cf) = config_file {
            sub_agent.config_file = cf.clone();

            // Load agent IDs from the created YAML
            if let Some((_, ids)) = crate::server::services::toolbox::load_agent_yaml(cf) {
                sub_agent.agent_ids = ids;
            }

            // Step 2: Boot realtime session
            let booted = start_realtime_session(
                &sub_agent.session_id,
                cf,
                &sub_agent.api_key,
                &sub_agent.api_url,
                &sub_agent.model,
                &sandbox_dir,
            )
            .await;
            append_chat_log(
                &id,
                &format!(
                    "🤖 Agent team '{}' ({} agents) — realtime session {}\n",
                    cf,
                    sub_agent.agent_ids.len(),
                    if booted { "LIVE" } else { "FAILED to boot" }
                ),
            );
        } else {
            append_chat_log(&id, "❌ Failed to create agent architecture — continuing WITHOUT sub-agents (single-agent mode).\n");
        }
    }

    // For manual/auto modes with a config file, boot realtime session
    if sub_agent_enabled && effective_mode != "fully_auto" && !sub_agent.config_file.is_empty() {
        use crate::server::services::toolbox::start_realtime_session;
        let booted = start_realtime_session(
            &sub_agent.session_id,
            &sub_agent.config_file,
            &sub_agent.api_key,
            &sub_agent.api_url,
            &sub_agent.model,
            &sandbox_dir,
        )
        .await;
        append_chat_log(
            &id,
            &format!(
                "🤖 Agent config '{}' ({} agents) — realtime session {}\n",
                sub_agent.config_file,
                sub_agent.agent_ids.len(),
                if booted {
                    "LIVE"
                } else {
                    "FAILED to boot (check the YAML file exists on this server)"
                }
            ),
        );
    }

    // Build system prompt — same as native UI (identity, soul, skills, tools)
    let system_prompt = {
        let tool_list = if sub_agent_enabled {
            match effective_mode.as_str() {
                "auto" => "create_architecture, send_task, wait_result, check_agents, web_search, fetch_url, run_python, run_shell, read_file, write_file, list_files, list_skills, load_skill",
                "fully_auto" => "create_architecture, send_task, wait_result, check_agents, run_python, write_file",
                "router" => "create_architecture, send_task, wait_result, check_agents, web_search, fetch_url, run_python, run_shell, read_file, write_file, list_files, list_skills, load_skill",
                "manual" => "send_task, wait_result, check_agents, run_python, write_file, read_file, list_files",
                _ => "web_search, fetch_url, run_python, run_shell, read_file, write_file, list_files, list_skills, load_skill, spawn_subagent",
            }
        } else {
            "web_search, fetch_url, run_python, run_shell, read_file, write_file, list_files, list_skills, load_skill, spawn_subagent"
        };

        let agents_list = sub_agent.agent_ids.join(", ");

        // Build detailed agent roster and extract orchestration metadata from YAML
        let (yaml_orch_mode, agent_roster, orchestrator_id) =
            if sub_agent_enabled && !sub_agent.config_file.is_empty() {
                if let Some((yaml, _)) =
                    crate::server::services::toolbox::load_agent_yaml(&sub_agent.config_file)
                {
                    let orch_mode = yaml
                        .get("system")
                        .and_then(|s| s.get("orchestration_mode"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let agents_arr = yaml
                        .get("agents")
                        .and_then(|a| a.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let mut orch_id = String::new();
                    let roster: Vec<String> = agents_arr
                        .iter()
                        .filter(|a| a.get("role").and_then(|r| r.as_str()) != Some("human"))
                        .map(|a| {
                            let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                            let name = a.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                            let role = a.get("role").and_then(|v| v.as_str()).unwrap_or("worker");
                            if role == "orchestrator" {
                                orch_id = id.to_string();
                            }
                            let resp = a
                                .get("responsibilities")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str())
                                        .collect::<Vec<_>>()
                                        .join("; ")
                                })
                                .unwrap_or_default();
                            if resp.is_empty() {
                                format!("  - {} (name: \"{}\", role: {})", id, name, role)
                            } else {
                                format!(
                                    "  - {} (name: \"{}\", role: {}, tasks: {})",
                                    id, name, role, resp
                                )
                            }
                        })
                        .collect();
                    (orch_mode, roster.join("\n"), orch_id)
                } else {
                    (String::new(), String::new(), String::new())
                }
            } else {
                (String::new(), String::new(), String::new())
            };
        let is_pipeline = yaml_orch_mode == "pipeline";

        let sub_agent_prompt = if sub_agent_enabled && !agents_list.is_empty() {
            let routing_rule = if !orchestrator_id.is_empty()
                && matches!(yaml_orch_mode.as_str(), "hierarchical" | "hybrid")
            {
                format!(
                    "\n\n🔴 ROUTING RULE: This is a {} architecture. You MUST send ALL tasks to the orchestrator '{}'. \
Do NOT send tasks directly to worker agents — the orchestrator will delegate to them.",
                    yaml_orch_mode, orchestrator_id
                )
            } else {
                String::new()
            };

            match effective_mode.as_str() {
                "fully_auto" => {
                    if is_pipeline {
                        format!(
                            "\n\n═══ FULLY AUTO MODE (PIPELINE) ═══\n\
An agent pipeline is LIVE. You are the COORDINATOR — your primary job is to delegate.\n\n\
Agent Roster:\n{}\n\n\
RULES (MANDATORY — violating these is a system error):\n\
1. Send the task to the FIRST agent only — the pipeline flows automatically.\n\
2. Use wait_result on the LAST agent to get the final output.\n\
3. You may use your own tools (web_search, run_python, etc.) to supplement agent work if needed.\n\
4. After collecting results, write a clear synthesis as your final answer.\n\n\
Workflow: send_task({{\"to\": \"<first_agent>\", \"task\": \"<detailed task>\"}}) → wait_result({{\"from\": \"<last_agent>\"}}) → synthesize.",
                            agent_roster
                        )
                    } else {
                        format!(
                            "\n\n═══ FULLY AUTO MODE ═══\n\
An agent team is LIVE. You are the COORDINATOR — your primary job is to delegate.\n\n\
Agent Roster:\n{}\n\n\
RULES (MANDATORY — violating these is a system error):\n\
1. You MUST delegate work to agents via send_task / wait_result.\n\
2. Give each agent a DETAILED task description — include all context they need.\n\
3. You may also use your own tools (web_search, run_python, etc.) to supplement agent work.\n\
4. After collecting ALL results, write a comprehensive synthesis as your final answer.{}\n\n\
Workflow: send_task({{\"to\": \"<agentId>\", \"task\": \"<detailed task>\"}}) → wait_result({{\"from\": \"<agentId>\"}}) → synthesize all results.",
                            agent_roster, routing_rule
                        )
                    }
                }
                "manual" => {
                    if is_pipeline {
                        format!(
                            "\n\n═══ MANUAL AGENT MODE (PIPELINE) ═══\n\
All agents are LIVE in a sequential pipeline. You are the COORDINATOR.\n\n\
Agent Roster:\n{}\n\n\
RULES (MANDATORY):\n\
1. Send the task to the FIRST agent — the pipeline forwards automatically.\n\
2. Use wait_result on the LAST agent for the final output.\n\
3. Give a DETAILED task description with full context.\n\
4. You may use your own tools to supplement agent work if needed.\n\n\
Workflow: send_task({{\"to\": \"<first_agent>\", \"task\": \"<detailed task>\"}}) → wait_result({{\"from\": \"<last_agent>\"}}) → synthesize.",
                            agent_roster
                        )
                    } else {
                        format!(
                            "\n\n═══ MANUAL AGENT MODE ═══\n\
All agents are LIVE. You are the COORDINATOR — your primary job is to delegate.\n\n\
Agent Roster:\n{}\n\n\
RULES (MANDATORY — violating these is a system error):\n\
1. You MUST delegate work to agents via send_task / wait_result.\n\
2. Give each agent a DETAILED task description — include all context they need.\n\
3. Always delegate, even for seemingly simple tasks — the agents are specialists.\n\
4. You may also use your own tools to supplement agent work.\n\
5. After collecting ALL results, write a comprehensive synthesis.{}\n\n\
Workflow: send_task({{\"to\": \"<agentId>\", \"task\": \"<detailed task>\"}}) → wait_result({{\"from\": \"<agentId>\"}}) → synthesize all results.",
                            agent_roster, routing_rule
                        )
                    }
                }
                "router" => {
                    format!(
                        "\n\n═══ ROUTER MODE — TEAM LIVE ═══\n\
A specialist team is running. You are the ORCHESTRATOR.\n\n\
Agent Roster:\n{}\n\n\
DISPATCH IN PARALLEL: fire send_task to ALL relevant agents in the SAME step (multiple send_task calls at once) so they work asynchronously; THEN wait_result for each; THEN combine/synthesize. Do not wait for one agent before sending the next.{}\n\
You may still answer trivial follow-ups directly without delegating.\n\n\
Workflow: send_task({{\"to\": \"<agentId>\", \"task\": \"<detailed task>\"}}) (×N at once) → wait_result({{\"from\": \"<agentId>\"}}) (×N) → synthesize.",
                        agent_roster, routing_rule
                    )
                }
                "auto" | _ => {
                    format!(
                        "\n\n═══ MULTI-AGENT SYSTEM ACTIVE ═══\n\
You have specialist sub-agents. You are the COORDINATOR — prefer delegation over doing work yourself.\n\n\
Agent Roster:\n{}\n\n\
RULES (MANDATORY):\n\
1. For ANY research, analysis, data gathering, writing, or complex task — you MUST delegate to the appropriate agent.\n\
2. Give each agent a DETAILED task description with full context.\n\
3. You may also use your own tools (web_search, run_python, etc.) to supplement agent work.\n\
4. After collecting results, write a comprehensive synthesis.{}\n\n\
Workflow: send_task({{\"to\": \"<agentId>\", \"task\": \"<detailed task>\"}}) → wait_result({{\"from\": \"<agentId>\"}}) → synthesize.",
                        agent_roster, routing_rule
                    )
                }
            }
        } else if sub_agent_enabled && effective_mode == "router" {
            "\n\n═══ ROUTER MODE ═══\n\
You are the ORCHESTRATOR. Your DEFAULT is to delegate to a specialist team — TRIAGE EVERY REQUEST and only solo the smallest tasks:\n\
1. TRIVIAL chat (greeting, quick fact, clarifying question): answer in text, no tools.\n\
2. TINY single-step task (one quick lookup, one short calculation, read/write a single file): DO IT YOURSELF with your own tools (run_python, web_search, fetch_url, run_shell, read_file, write_file, list_files, load_skill).\n\
3. EVERYTHING ELSE — any task that touches multiple sources, produces a real deliverable, needs analysis or cross-checking, or could plausibly split into 2+ sub-tasks: call create_architecture to design and boot a specialist team, then orchestrate it. This is the common case.\n\
When you build a team, use architectureType 'flat', split into INDEPENDENT sub-tasks, fire send_task to ALL workers at once, then wait_result on each, then synthesize.\n\
WHEN IN DOUBT, BUILD A TEAM. Only solo when the task is unmistakably trivial or a single tool call.".to_string()
        } else if sub_agent_enabled {
            "\n\n═══ FULLY AUTO MODE ═══\n\
An agent team is being created for this task. You are the COORDINATOR.\n\
1. Call create_architecture to design and boot a team if no agents are available yet.\n\
2. Then use send_task/wait_result to delegate work to agents.\n\
3. You may also use your own tools to supplement agent work."
                .to_string()
        } else {
            String::new()
        };

        let research_instruction = if sub_agent_enabled && effective_mode == "router" {
            "Triage first, but default to delegating: solo only trivial chat or a single tool call; for anything multi-step or multi-source, build a team via create_architecture + send_task/wait_result."
        } else if sub_agent_enabled {
            "Delegate ALL tasks to agents via send_task/wait_result."
        } else {
            "For research tasks, gather info from multiple sources using web_search/fetch_url, then synthesize results"
        };

        // Profile system-prompt override: replace_base swaps only the hardcoded
        // base text (orchestration instructions, project/skills/SOUL blocks
        // still apply); otherwise the profile text is appended after the base.
        let profile_prompt = loop_profile
            .as_deref()
            .and_then(|p| p.system_prompt.as_ref())
            .filter(|sp| !sp.text.trim().is_empty());
        let mut base = if let Some(sp) = profile_prompt.filter(|sp| sp.replace_base) {
            format!(
                "{}\n\nYour working directory is the sandbox folder '{}'. All file operations use this directory as the root.{}",
                sp.text.trim(),
                sandbox_dir,
                sub_agent_prompt
            )
        } else {
            format!(
            "You are AndrewOS, an AI assistant with tools for search, code execution, files, and skills.\n\
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
            )
        };
        if let Some(sp) = profile_prompt.filter(|sp| !sp.replace_base) {
            base.push_str(&format!(
                "\n\n=== USER INSTRUCTIONS (agent-loop profile) ===\n{}",
                sp.text.trim()
            ));
        }

        // Graph mode heads-up: the worker responds better to panel rejections
        // when it knows a review gate exists.
        if graph_profile_arc.is_some() {
            base.push_str(
                "\n\nGRAPH MODE: your final answer will be reviewed by a judge panel against \
                configured rules BEFORE it reaches the user. If the panel rejects it you will \
                receive a structured YAML verdict — follow its `revise` instructions and return \
                a complete corrected answer.",
            );
        }

        // Inject active project context (name/description/memory/custom instructions).
        if let Some(ref ctx) = project_ctx {
            if !ctx.system_block.is_empty() {
                base.push_str(&format!("\n\n=== ACTIVE PROJECT ===\n{}", ctx.system_block));
            }
        }

        // Append installed skills — filtered to the project's assigned skills
        // if any, further narrowed by the agent-loop profile's skill filter.
        let project_skills = project_ctx
            .as_ref()
            .map(|c| c.skills.as_slice())
            .filter(|s| !s.is_empty());
        let profile_skill_filter = loop_profile.as_deref().and_then(|p| p.skills.as_ref());
        let skills_block = match profile_skill_filter.map(|f| f.mode.as_str()) {
            Some("none") => String::new(),
            Some("selected") => {
                let selected = &profile_skill_filter.unwrap().list;
                let effective: Vec<String> = match project_skills {
                    Some(ps) => selected
                        .iter()
                        .filter(|s| ps.iter().any(|p| p == *s))
                        .cloned()
                        .collect(),
                    None => selected.clone(),
                };
                if effective.is_empty() {
                    String::new()
                } else {
                    crate::server::services::toolbox::build_enabled_skills_block_pub(Some(
                        &effective,
                    ))
                    .await
                }
            }
            _ => {
                crate::server::services::toolbox::build_enabled_skills_block_pub(project_skills)
                    .await
            }
        };
        if !skills_block.is_empty() {
            base.push_str(&skills_block);
        }

        // Inject Soul & Identity from data dir
        let data_dir = crate::server::data::data_dir();
        if let Ok(soul) = std::fs::read_to_string(data_dir.join("SOUL.md")) {
            if !soul.trim().is_empty() {
                base.push_str(&format!(
                    "\n\n=== SOUL.md (Internal Cognition & Behavioral Prior) ===\n{}",
                    soul
                ));
            }
        }
        if let Ok(identity) = std::fs::read_to_string(data_dir.join("IDENTITY.md")) {
            if !identity.trim().is_empty() {
                base.push_str(&format!(
                    "\n\n=== IDENTITY.md (External Presentation) ===\n{}",
                    identity
                ));
            }
        }

        Some(base)
    };

    // Register in native UI's active tasks so local Tasks view sees web chats
    {
        let title = {
            let sessions_snap = get_chat_history().await;
            sessions_snap
                .iter()
                .find(|s| s.id == id)
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

    // Logs were initialized before the pre-flight (so pre-flight lines are kept)
    let cl_path = chat_log_dir().join(format!("{}.log", id));

    // Spawn AI work in a fully detached task — returns immediately so the
    // HTTP response is not tied to the long-running AI loop. The client polls
    // for completion via GET /sessions/:id (checks last message role).
    let session_id_for_log = id.clone();
    let session_id_bg = id.clone();
    let chat_log_path_for_cb = cl_path.clone();
    let cl_path_bg = cl_path.clone();

    // Router: run the ORCHESTRATOR on the user-chosen pool model (worker agents
    // keep their own per-agent models). Empty/unset → main model.
    let (api_key, api_url, model) = match (sub_agent_enabled && effective_mode == "router")
        .then(|| {
            settings
                .router_orchestrator_model
                .as_ref()
                .filter(|s| !s.is_empty())
        })
        .flatten()
    {
        Some(want) => {
            match settings
                .model_pool
                .as_ref()
                .and_then(|pool| pool.iter().find(|e| &e.model == want))
            {
                Some(e) => {
                    let u = if e.api_url.trim().is_empty() {
                        api_url.clone()
                    } else if e.api_url == "claude-code" || e.api_url.ends_with("/chat/completions")
                    {
                        e.api_url.clone()
                    } else {
                        format!("{}/chat/completions", e.api_url.trim_end_matches('/'))
                    };
                    let k = if e.api_key.trim().is_empty() {
                        api_key.clone()
                    } else {
                        e.api_key.clone()
                    };
                    (k, u, e.model.clone())
                }
                None => (api_key, api_url, want.clone()),
            }
        }
        None => (api_key, api_url, model),
    };

    // Workflow-mode inputs, resolved before the task takes ownership of them.
    // The roster lets a pattern run judges on a different model from workers.
    let workflow_model_pool = settings.model_pool.clone().unwrap_or_default();
    // Reuses the existing swarm agent-count knob rather than adding a second
    // one that means the same thing. build_pattern clamps it to a sane range.
    let settings_agent_count: Option<usize> = settings
        .extra
        .get("autoAgentCount")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .map(|n| n as usize);

    tokio::spawn(async move {
        let extra_cb = extra_on_update;
        let mut done_tx = done_tx;
        let result = if let Some(pattern) = workflow_pattern {
            // Width sizes the parallel stage (workers, verifiers, attempts).
            let width = settings_agent_count.unwrap_or(3);
            run_workflow_turn(
                &pattern,
                width,
                &llm_messages,
                &api_key,
                &api_url,
                &model,
                &sandbox_dir,
                &session_id_for_log,
                workflow_model_pool,
            )
            .await
        } else {
            call_with_tools(
                &api_key,
                &api_url,
                &model,
                llm_messages,
                system_prompt,
                &sandbox_dir,
                move |update: ToolUpdate| {
                    use crate::server::services::toolbox::append_session_progress;
                    if let Some(cb) = &extra_cb {
                        cb(update.clone());
                    }
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
                                // Log a summary of text-only responses for diagnostics
                                let preview: String = text.chars().take(200).collect();
                                if !preview.is_empty() {
                                    format!("💬 Response: {}\n", preview)
                                } else {
                                    String::new()
                                }
                            }
                        }
                        ToolUpdate::Error(err) => format!("❌ {}\n", err),
                        ToolUpdate::ApprovalRequired { name, .. } => {
                            format!("⚠️ Approval needed for **{}**\n", name)
                        }
                    };
                    if !line.is_empty() {
                        append_session_progress(&session_id_for_log, &line);
                        use std::io::Write;
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .append(true)
                            .open(&chat_log_path_for_cb)
                        {
                            let _ = f.write_all(line.as_bytes());
                        }
                    }
                },
                sub_agent,
            )
            .await
        };

        // Remove from native UI's active tasks and cancel flags
        {
            let mut chats = crate::ui::tasks_view::active_chats().lock().unwrap();
            chats.retain(|c| c.session_id != session_id_bg);
        }
        {
            let mut flags = cancel_flags().lock().unwrap();
            flags.remove(&session_id_bg);
        }

        // Clean up activity log so finished tasks don't appear in active-tasks
        {
            let log_path = activity_log_dir().join(format!("{}.log", session_id_bg));
            let _ = tokio::fs::remove_file(&log_path).await;
        }

        // Append completion footer to chat log
        {
            let footer = format!(
                "[{}] === Response complete ===\n\n",
                chrono::Utc::now().format("%H:%M:%S"),
            );
            use tokio::io::AsyncWriteExt;
            if let Ok(mut f) = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&cl_path_bg)
                .await
            {
                let _ = f.write_all(footer.as_bytes()).await;
            }
        }

        // Never persist a blank assistant turn — the web client polls for the
        // assistant message, and an empty one looks like the job never finished.
        let assistant_content = if result.content.trim().is_empty() {
            "⚠️ The run finished without a final answer (the model returned empty content). \
            Check the activity log for partial results and try again."
                .to_string()
        } else {
            result.content
        };
        let output_files = result.files.clone();

        // Save assistant response to session history
        let mut sessions3 = get_chat_history().await;
        if let Some(s) = sessions3.iter_mut().find(|s| s.id == session_id_bg) {
            s.messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: assistant_content.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                files: if output_files.is_empty() {
                    None
                } else {
                    Some(output_files.clone())
                },
                feedback: None,
            });
            s.updated_at = chrono::Utc::now().to_rfc3339();
            save_chat_history(&sessions3).await;
        }

        tracing::info!(
            "[chat] Session {} completed ({} chars)",
            session_id_bg,
            assistant_content.len()
        );

        if let Some(tx) = done_tx.take() {
            let _ = tx.send(assistant_content);
        }
    });

    // Return immediately — callers poll the session (or await done_tx)
    Ok(())
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
// GET /agent-configs — list available YAML agent config files
// ---------------------------------------------------------------------------

async fn list_agent_configs() -> impl IntoResponse {
    let dir = crate::server::data::data_dir().join("agents");
    let mut files = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".yaml") || name.ends_with(".yml") {
                files.push(name);
            }
        }
    }
    files.sort();
    Json(json!({"ok": true, "files": files}))
}

// ---------------------------------------------------------------------------
// GET /active-tasks — list sessions that are currently processing (recent activity)
// ---------------------------------------------------------------------------

async fn get_active_tasks() -> impl IntoResponse {
    let log_dir = activity_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let mut active = Vec::new();

    // Primary source: cancel_flags map tracks sessions with running AI tasks
    let running_ids: Vec<String> = {
        let flags = cancel_flags().lock().unwrap();
        flags.keys().cloned().collect()
    };

    let sessions = get_chat_history().await;

    for session_id in &running_ids {
        let title = sessions
            .iter()
            .find(|s| &s.id == session_id)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| session_id.clone());

        let log_path = log_dir.join(format!("{}.log", session_id));
        let content = tokio::fs::read_to_string(&log_path)
            .await
            .unwrap_or_default();

        active.push(json!({
            "session_id": session_id,
            "title": title,
            "activity": content,
            "age_secs": 0,
        }));
    }

    // Fallback: also check activity logs modified recently (catches edge cases)
    if let Ok(mut entries) = tokio::fs::read_dir(&log_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".log") {
                continue;
            }
            let session_id = name.trim_end_matches(".log").to_string();
            if running_ids.contains(&session_id) {
                continue;
            } // already added

            if let Ok(meta) = entry.metadata().await {
                if let Ok(modified) = meta.modified() {
                    let age = modified.elapsed().unwrap_or_default().as_secs();
                    if age < 60 && meta.len() > 0 {
                        let content = tokio::fs::read_to_string(entry.path())
                            .await
                            .unwrap_or_default();
                        let title = sessions
                            .iter()
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
    let killed = kill_session_by_id(&id).await;
    Json(json!({ "ok": killed, "session_id": id }))
}

/// Cancel a running chat session: sets the cancel flag, shuts down any
/// realtime sub-agent session, and SIGKILLs the session's process trees.
/// Extracted from the kill handler so bot channels can /stop a run.
pub async fn kill_session_by_id(id: &str) -> bool {
    let killed = {
        let flags = cancel_flags().lock().unwrap();
        if let Some(flag) = flags.get(id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    };

    // Also abort any realtime sub-agent session
    crate::server::services::toolbox::shutdown_realtime_session(id).await;

    // SIGKILL every OS process tree this session spawned (python/shell/CLI
    // agents and their descendants). Setting the cancel flag alone only stops
    // the loop at round boundaries — already-running children would otherwise
    // keep executing with full filesystem/network access after the kill.
    let reaped = crate::server::services::proc_registry::kill_session(id);

    // Also push to native UI's killed list
    if killed {
        let killed_ids = crate::ui::tasks_view::killed_chat_ids();
        let mut ids = killed_ids.lock().unwrap();
        if !ids.iter().any(|k| k == id) {
            ids.push(id.to_string());
        }
    }

    // NOTE: Do NOT remove the cancel flag here — the spawned task reads it
    // to know it should stop. The spawned task removes it when it finishes.
    // Clean up the activity log so the task disappears from active-tasks immediately.
    let log_path = activity_log_dir().join(format!("{}.log", id));
    let _ = tokio::fs::remove_file(&log_path).await;

    tracing::info!(
        "[kill] Session {} killed={} reaped_processes={}",
        id,
        killed,
        reaped
    );

    killed
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
