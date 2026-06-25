use std::collections::HashMap;
use std::sync::OnceLock;

use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::broadcast;
use tracing::info;
use uuid::Uuid;

use crate::server::services::toolbox::{
    call_with_tools, load_agent_yaml, shutdown_realtime_session,
    start_realtime_session, SubAgentConfig, ToolUpdate,
};
use crate::server::data::{get_settings};

// ---------------------------------------------------------------------------
// Remote task state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct RemoteTaskEntry {
    id: String,
    task: String,
    status: String,           // "pending", "running", "completed", "failed", "killed"
    progress: Vec<Value>,
    result: Option<String>,
    session_id: String,
    created_at: String,
    completed_at: Option<String>,
    progress_seq: u64,
    owner_token: String,      // token that created this task (for session isolation)
}

static REMOTE_TASKS: OnceLock<TokioMutex<HashMap<String, RemoteTaskEntry>>> = OnceLock::new();

fn remote_tasks() -> &'static TokioMutex<HashMap<String, RemoteTaskEntry>> {
    REMOTE_TASKS.get_or_init(|| TokioMutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Broadcast channel for instant UI updates (no polling needed)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TaskEvent {
    /// A task's progress or status changed — contains the task ID
    Updated(String),
}

static TASK_TX: OnceLock<broadcast::Sender<TaskEvent>> = OnceLock::new();

fn task_tx() -> &'static broadcast::Sender<TaskEvent> {
    TASK_TX.get_or_init(|| broadcast::channel(64).0)
}

/// Subscribe to real-time task events. UI calls this once and receives
/// instant notifications instead of polling every 5 seconds.
pub fn subscribe_task_events() -> broadcast::Receiver<TaskEvent> {
    task_tx().subscribe()
}

fn notify_task_updated(task_id: &str) {
    let _ = task_tx().send(TaskEvent::Updated(task_id.to_string()));
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

// ---------------------------------------------------------------------------
// Token extraction for session isolation
// ---------------------------------------------------------------------------

/// Extract the authenticated token from request headers (Bearer) or query string.
/// Returns an empty string when no token is present (local-only / no-auth mode).
fn extract_caller_token(headers: &HeaderMap) -> String {
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let t = auth.trim_start_matches("Bearer ").to_string();
        if !t.is_empty() {
            return t;
        }
    }
    String::new()
}

/// Check whether a caller token is allowed to access a task.
/// - Empty owner or empty caller ⇒ local-only mode, always allowed.
/// - Otherwise tokens must match exactly.
fn token_owns_task(caller_token: &str, owner_token: &str) -> bool {
    caller_token.is_empty() || owner_token.is_empty() || caller_token == owner_token
}

// ---------------------------------------------------------------------------
// Request/response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SubmitTaskBody {
    task: String,
    config_file: Option<String>,
    mode: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /remote/task — submit a task for execution.
/// Access is governed by the auth middleware — this endpoint also serves the
/// local web UI's Agents page, so it must not require remote_enabled.
async fn submit_task(headers: HeaderMap, Json(body): Json<SubmitTaskBody>) -> impl IntoResponse {
    let settings = get_settings().await;

    let caller_token = extract_caller_token(&headers);
    let task_id = Uuid::new_v4().to_string();
    let session_id = format!("remote_{}", &task_id[..8]);

    let mode = body.mode.clone().unwrap_or_else(|| "auto".to_string());
    // Resolve the agent team: explicit in the request, else the server's
    // configured sub-agent system (so older clients still get the team).
    // "single" mode means: no team, run a plain single agent.
    let config_file = if mode == "single" {
        None
    } else {
        body.config_file.clone().filter(|f| !f.is_empty()).or_else(|| {
            if settings.sub_agent_enabled == Some(true) {
                settings.sub_agent_config_file.clone().filter(|f| !f.is_empty())
            } else {
                None
            }
        })
    };

    let entry = RemoteTaskEntry {
        id: task_id.clone(),
        task: body.task.clone(),
        status: "pending".to_string(),
        progress: vec![],
        result: None,
        session_id: session_id.clone(),
        created_at: now_iso(),
        completed_at: None,
        progress_seq: 0,
        owner_token: caller_token,
    };

    remote_tasks()
        .lock()
        .await
        .insert(task_id.clone(), entry);

    // Spawn the task processor
    let task = body.task.clone();
    let tid = task_id.clone();
    let sid = session_id.clone();
    let cf = config_file.clone();
    let task_mode = mode.clone();

    tokio::spawn(async move {
        process_remote_task(tid, sid, task, cf, task_mode).await;
    });

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "taskId": task_id,
            "sessionId": session_id,
        })),
    )
}

/// GET /remote/tasks — list remote tasks owned by the caller
async fn list_tasks(headers: HeaderMap) -> impl IntoResponse {
    let caller_token = extract_caller_token(&headers);
    let tasks = remote_tasks().lock().await;
    let mut list: Vec<Value> = tasks
        .values()
        .filter(|t| token_owns_task(&caller_token, &t.owner_token))
        .map(|t| {
            json!({
                "id": t.id,
                "task": crate::util::truncate_utf8_ellipsis(&t.task, 200),
                "status": t.status,
                "sessionId": t.session_id,
                "createdAt": t.created_at,
                "completedAt": t.completed_at,
                "progressCount": t.progress.len(),
                "result": t.result.as_deref().map(|r| crate::util::truncate_utf8_ellipsis(r, 2000)),
                "lastProgress": t.progress.last().and_then(|p| p["message"].as_str()).unwrap_or(""),
            })
        })
        .collect();

    // Sort by created_at descending
    list.sort_by(|a, b| {
        b["createdAt"]
            .as_str()
            .cmp(&a["createdAt"].as_str())
    });

    Json(json!({ "ok": true, "tasks": list }))
}

/// GET /remote/task/:id — poll for task progress/result
async fn get_task(headers: HeaderMap, Path(id): Path<String>) -> impl IntoResponse {
    let caller_token = extract_caller_token(&headers);
    let tasks = remote_tasks().lock().await;
    match tasks.get(&id) {
        Some(entry) if token_owns_task(&caller_token, &entry.owner_token) => {
            let resp = json!({
                "ok": true,
                "id": entry.id,
                "status": entry.status,
                "progress": entry.progress,
                "result": entry.result,
                "sessionId": entry.session_id,
                "createdAt": entry.created_at,
                "completedAt": entry.completed_at,
                "progressSeq": entry.progress_seq,
            });
            (StatusCode::OK, Json(resp))
        }
        Some(_) => (
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "Access denied" })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "Task not found" })),
        ),
    }
}

/// POST /remote/task/:id/kill — kill a running task
async fn kill_task(headers: HeaderMap, Path(id): Path<String>) -> impl IntoResponse {
    let caller_token = extract_caller_token(&headers);
    let mut tasks = remote_tasks().lock().await;
    match tasks.get_mut(&id) {
        Some(entry) if !token_owns_task(&caller_token, &entry.owner_token) => (
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "Access denied" })),
        ),
        Some(entry) if entry.status == "running" || entry.status == "pending" => {
            entry.status = "killed".to_string();
            entry.completed_at = Some(now_iso());
            add_progress_inner(entry, "Task killed by user");

            // Shutdown any realtime session and SIGKILL every process tree this
            // remote task spawned (processes register under the same session_id).
            let sid = entry.session_id.clone();
            drop(tasks);
            shutdown_realtime_session(&sid).await;
            crate::server::services::proc_registry::kill_session(&sid);

            (
                StatusCode::OK,
                Json(json!({ "ok": true, "message": "Task killed" })),
            )
        }
        Some(_) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "Task is not running" })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "Task not found" })),
        ),
    }
}

// ---------------------------------------------------------------------------
// Task processor
// ---------------------------------------------------------------------------

fn add_progress_inner(entry: &mut RemoteTaskEntry, msg: &str) {
    entry.progress_seq += 1;
    entry.progress.push(json!({
        "seq": entry.progress_seq,
        "timestamp": now_iso(),
        "message": msg,
    }));
    // Keep max 200 progress entries
    if entry.progress.len() > 200 {
        let drain = entry.progress.len() - 200;
        entry.progress.drain(..drain);
    }
}

async fn add_progress(task_id: &str, msg: &str) {
    let mut tasks = remote_tasks().lock().await;
    if let Some(entry) = tasks.get_mut(task_id) {
        add_progress_inner(entry, msg);
    }
    drop(tasks);
    notify_task_updated(task_id);
}

async fn set_status(task_id: &str, status: &str) {
    let mut tasks = remote_tasks().lock().await;
    if let Some(entry) = tasks.get_mut(task_id) {
        entry.status = status.to_string();
        if status == "completed" || status == "failed" || status == "killed" {
            entry.completed_at = Some(now_iso());
        }
    }
    drop(tasks);
    notify_task_updated(task_id);
}

async fn set_result(task_id: &str, result: &str) {
    let mut tasks = remote_tasks().lock().await;
    if let Some(entry) = tasks.get_mut(task_id) {
        entry.result = Some(result.to_string());
    }
    drop(tasks);
    notify_task_updated(task_id);
}

async fn is_killed(task_id: &str) -> bool {
    let tasks = remote_tasks().lock().await;
    tasks
        .get(task_id)
        .map(|e| e.status == "killed")
        .unwrap_or(true)
}

async fn process_remote_task(
    task_id: String,
    session_id: String,
    task: String,
    config_file: Option<String>,
    mode: String,
) {
    set_status(&task_id, "running").await;
    add_progress(&task_id, "Task started").await;

    let settings = get_settings().await;
    let api_key = settings.tiger_bot_api_key.clone();
    let api_url = settings
        .tiger_bot_api_url
        .clone()
        .unwrap_or_else(|| "https://api.anthropic.com".to_string());
    let model = if settings.tiger_bot_model.is_empty() {
        "claude-sonnet-4-20250514".to_string()
    } else {
        settings.tiger_bot_model.clone()
    };
    let sandbox_dir = crate::server::data::get_sandbox_dir_sync();

    if api_key.is_empty() {
        add_progress(&task_id, "Error: No API key configured").await;
        set_status(&task_id, "failed").await;
        set_result(&task_id, "No API key configured in settings").await;
        return;
    }

    // Router is self-contained: it ignores any pre-set YAML team and builds its
    // own LLM-assigned team via create_architecture (no up-front boot).
    let config_file = if mode == "router" { None } else { config_file };

    // Router heterogeneous model pool + tier from settings (empty otherwise).
    let (router_pool, router_tier) = if mode == "router" {
        let pool = settings
            .model_pool
            .clone()
            .unwrap_or_default()
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect::<Vec<_>>();
        (pool, settings.router_tier.clone().unwrap_or_else(|| "fast".into()))
    } else {
        (Vec::new(), String::new())
    };

    // Router: orchestrator runs on the user-chosen pool model (workers keep
    // their own per-agent models). Empty/unset → main model.
    let (api_key, api_url, model) = match (mode == "router")
        .then(|| settings.router_orchestrator_model.as_ref().filter(|s| !s.is_empty()))
        .flatten()
    {
        Some(want) => {
            match settings.model_pool.as_ref().and_then(|pool| pool.iter().find(|e| &e.model == want)) {
                Some(e) => {
                    let u = if e.api_url.trim().is_empty() {
                        api_url.clone()
                    } else if e.api_url == "claude-code" || e.api_url.ends_with("/chat/completions") {
                        e.api_url.clone()
                    } else {
                        format!("{}/chat/completions", e.api_url.trim_end_matches('/'))
                    };
                    let k = if e.api_key.trim().is_empty() { api_key.clone() } else { e.api_key.clone() };
                    (k, u, e.model.clone())
                }
                None => (api_key, api_url, want.clone()),
            }
        }
        None => (api_key, api_url, model),
    };

    // Check if we have a config file for realtime agents
    let has_agents = config_file
        .as_ref()
        .map(|f| load_agent_yaml(f).is_some())
        .unwrap_or(false);

    if has_agents {
        let cf = config_file.as_ref().unwrap();
        add_progress(
            &task_id,
            &format!("Booting agent team from {}", cf),
        )
        .await;

        let booted = start_realtime_session(
            &session_id, cf, &api_key, &api_url, &model, &sandbox_dir,
        )
        .await;

        if !booted {
            add_progress(&task_id, "Failed to boot agent team, falling back to simple mode").await;
        }
    }

    // Retry logic — load max retries from settings (default 2 → up to 3 total attempts)
    let max_retries = settings.remote_task_max_retries.unwrap_or(2) as usize;

    for attempt in 0..=max_retries {
        // Run the task through the tool loop
        let messages = vec![json!({"role": "user", "content": task.clone()})];
        let system_prompt = Some(
            "You are a helpful assistant processing a remote task. Complete the task thoroughly and return a clear result.".to_string()
        );

        let sub_agent = if mode == "router" {
            // Self-contained router: enabled with the model pool, no pre-set team.
            SubAgentConfig {
                enabled: true,
                api_key: api_key.clone(),
                api_url: api_url.clone(),
                model: model.clone(),
                session_id: session_id.clone(),
                agent_id: "main".to_string(),
                mode: "router".to_string(),
                agent_role: "orchestrator".to_string(),
                model_pool: router_pool.clone(),
                router_tier: router_tier.clone(),
                ..SubAgentConfig::default()
            }
        } else if let Some(ref cf) = config_file {
            if let Some((_, ids)) = load_agent_yaml(cf) {
                SubAgentConfig {
                    enabled: !ids.is_empty(),
                    config_file: cf.clone(),
                    agent_ids: ids,
                    api_key: api_key.clone(),
                    api_url: api_url.clone(),
                    model: model.clone(),
                    depth: 0,
                    session_id: session_id.clone(),
                    agent_id: "main".to_string(),
                    mode: if mode == "single" { "auto".to_string() } else { mode.clone() },
                    agent_role: "orchestrator".to_string(),
                    cancel_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    ..SubAgentConfig::default()
                }
            } else {
                SubAgentConfig {
                    session_id: session_id.clone(),
                    agent_id: "main".to_string(),
                    ..SubAgentConfig::default()
                }
            }
        } else {
            SubAgentConfig {
                session_id: session_id.clone(),
                agent_id: "main".to_string(),
                ..SubAgentConfig::default()
            }
        };

        let tid = task_id.clone();
        let on_update = move |update: ToolUpdate| {
            let tid = tid.clone();
            match &update {
                ToolUpdate::ToolCall { name, .. } => {
                    let msg = format!("Calling tool: {}", name);
                    let tid = tid.clone();
                    tokio::spawn(async move {
                        add_progress(&tid, &msg).await;
                    });
                }
                ToolUpdate::ToolResult { name, result } => {
                    let ok = result["ok"].as_bool().unwrap_or(false);
                    let msg = format!("Tool {} → {}", name, if ok { "ok" } else { "error" });
                    let tid = tid.clone();
                    tokio::spawn(async move {
                        add_progress(&tid, &msg).await;
                    });
                }
                _ => {}
            }
        };

        let result = call_with_tools(
            &api_key,
            &api_url,
            &model,
            messages,
            system_prompt,
            &sandbox_dir,
            on_update,
            sub_agent,
        )
        .await;

        // Check if killed during execution — preserve partial output
        if is_killed(&task_id).await {
            let partial_content = if result.content.is_empty() {
                "Task was cancelled.".to_string()
            } else {
                result.content
            };
            set_result(&task_id, &partial_content).await;
            add_progress(&task_id, "Task killed — partial result preserved").await;
            return;
        }

        // Detect timeout / context overflow / empty / cancelled responses that warrant a retry
        let content_lower = result.content.to_lowercase();
        let is_retryable = result.content.is_empty()
            || content_lower.contains("timeout")
            || content_lower.contains("context overflow")
            || content_lower.contains("cancelled")
            || content_lower.contains("canceled");

        if is_retryable && attempt < max_retries {
            add_progress(
                &task_id,
                &format!(
                    "Agent timed out or returned empty — re-delegating (attempt {}/{})",
                    attempt + 2,
                    max_retries + 1
                ),
            )
            .await;
            info!(
                "[Remote] Task {} retrying (attempt {}/{})",
                task_id,
                attempt + 2,
                max_retries + 1
            );
            continue;
        }

        // Store final result — either success or last attempt exhausted
        let final_text = if result.content.is_empty() {
            "No response generated".to_string()
        } else {
            result.content
        };
        set_result(&task_id, &final_text).await;
        add_progress(&task_id, "Task completed").await;
        set_status(&task_id, "completed").await;
        break;
    }

    // Cleanup realtime session if we booted one
    if has_agents {
        shutdown_realtime_session(&session_id).await;
    }

    info!("[Remote] Task {} completed", task_id);
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Public: get all remote tasks as JSON values (for native UI)
pub async fn get_all_remote_tasks() -> Vec<Value> {
    let tasks = remote_tasks().lock().await;
    let mut list: Vec<Value> = tasks
        .values()
        .map(|t| {
            json!({
                "id": t.id,
                "task": t.task,
                "status": t.status,
                "sessionId": t.session_id,
                "createdAt": t.created_at,
                "completedAt": t.completed_at,
                "progressCount": t.progress.len(),
                "result": t.result,
                "progress": t.progress,
            })
        })
        .collect();
    list.sort_by(|a, b| {
        b["createdAt"].as_str().cmp(&a["createdAt"].as_str())
    });
    list
}

/// Public: kill a remote task by ID (for native UI)
pub async fn kill_remote_task(id: &str) -> bool {
    let mut tasks = remote_tasks().lock().await;
    if let Some(entry) = tasks.get_mut(id) {
        if entry.status == "running" || entry.status == "pending" {
            entry.status = "killed".to_string();
            entry.completed_at = Some(now_iso());
            add_progress_inner(entry, "Task killed by user");
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// WebSocket endpoint — pushes task events to connected clients instantly
// ---------------------------------------------------------------------------

async fn ws_handler(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::server::AppState>>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
    ws: axum::extract::WebSocketUpgrade,
) -> impl IntoResponse {
    // Authenticate: require a valid token via ?token= query parameter
    let settings = get_settings().await;
    let remote_token = if settings.remote_enabled == Some(true) {
        settings.remote_token.clone().filter(|t| !t.is_empty())
    } else {
        None
    };

    // If no auth is configured at all, allow (local-only use)
    let has_any_auth = !state.access_token.is_empty() || remote_token.is_some();
    if has_any_auth {
        let token = params.get("token");
        let authorized = match token {
            Some(t) => {
                (!state.access_token.is_empty() && *t == state.access_token)
                    || remote_token.as_deref().map(|rt| t == rt).unwrap_or(false)
            }
            None => false,
        };
        if !authorized {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    }

    ws.on_upgrade(handle_ws_connection).into_response()
}

async fn handle_ws_connection(mut socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;

    // Send initial task list snapshot
    let snapshot = get_all_remote_tasks().await;
    let init_msg = json!({ "type": "snapshot", "tasks": snapshot });
    if socket
        .send(Message::Text(init_msg.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Subscribe to broadcast and forward events as JSON
    let mut rx = subscribe_task_events();
    loop {
        match rx.recv().await {
            Ok(TaskEvent::Updated(task_id)) => {
                // Send the updated task data
                let tasks = remote_tasks().lock().await;
                let payload = if let Some(entry) = tasks.get(&task_id) {
                    json!({
                        "type": "task_update",
                        "task": {
                            "id": entry.id,
                            "task": entry.task,
                            "status": entry.status,
                            "sessionId": entry.session_id,
                            "createdAt": entry.created_at,
                            "completedAt": entry.completed_at,
                            "progressCount": entry.progress.len(),
                            "result": entry.result,
                            "progress": entry.progress,
                            "progressSeq": entry.progress_seq,
                        }
                    })
                } else {
                    json!({ "type": "task_removed", "taskId": task_id })
                };
                drop(tasks);

                if socket
                    .send(Message::Text(payload.to_string().into()))
                    .await
                    .is_err()
                {
                    break; // Client disconnected
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                // Missed some events — send full snapshot to resync
                info!("[WS] Client lagged by {} events, sending snapshot", n);
                let snapshot = get_all_remote_tasks().await;
                let msg = json!({ "type": "snapshot", "tasks": snapshot });
                if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

pub fn router() -> Router<std::sync::Arc<crate::server::AppState>> {
    Router::new()
        .route("/task", post(submit_task))
        .route("/tasks", get(list_tasks))
        .route("/task/{id}", get(get_task))
        .route("/task/{id}/kill", post(kill_task))
        .route("/ws", axum::routing::get(ws_handler))
}
