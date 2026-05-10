use std::collections::HashMap;
use std::sync::OnceLock;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex as TokioMutex;
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
}

static REMOTE_TASKS: OnceLock<TokioMutex<HashMap<String, RemoteTaskEntry>>> = OnceLock::new();

fn remote_tasks() -> &'static TokioMutex<HashMap<String, RemoteTaskEntry>> {
    REMOTE_TASKS.get_or_init(|| TokioMutex::new(HashMap::new()))
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

// ---------------------------------------------------------------------------
// Request/response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SubmitTaskBody {
    task: String,
    config_file: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /remote/task — submit a task for execution
async fn submit_task(Json(body): Json<SubmitTaskBody>) -> impl IntoResponse {
    let settings = get_settings().await;

    // Check if remote is enabled
    if settings.remote_enabled != Some(true) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "Remote tasks are not enabled" })),
        );
    }

    let task_id = Uuid::new_v4().to_string();
    let session_id = format!("remote_{}", &task_id[..8]);

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
    };

    remote_tasks()
        .lock()
        .await
        .insert(task_id.clone(), entry);

    // Spawn the task processor
    let task = body.task.clone();
    let tid = task_id.clone();
    let sid = session_id.clone();
    let config_file = body.config_file.clone();

    tokio::spawn(async move {
        process_remote_task(tid, sid, task, config_file).await;
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

/// GET /remote/tasks — list all remote tasks
async fn list_tasks() -> impl IntoResponse {
    let tasks = remote_tasks().lock().await;
    let mut list: Vec<Value> = tasks
        .values()
        .map(|t| {
            json!({
                "id": t.id,
                "task": if t.task.len() > 200 { format!("{}...", &t.task[..200]) } else { t.task.clone() },
                "status": t.status,
                "sessionId": t.session_id,
                "createdAt": t.created_at,
                "completedAt": t.completed_at,
                "progressCount": t.progress.len(),
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
async fn get_task(Path(id): Path<String>) -> impl IntoResponse {
    let tasks = remote_tasks().lock().await;
    match tasks.get(&id) {
        Some(entry) => {
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
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "Task not found" })),
        ),
    }
}

/// POST /remote/task/:id/kill — kill a running task
async fn kill_task(Path(id): Path<String>) -> impl IntoResponse {
    let mut tasks = remote_tasks().lock().await;
    match tasks.get_mut(&id) {
        Some(entry) if entry.status == "running" || entry.status == "pending" => {
            entry.status = "killed".to_string();
            entry.completed_at = Some(now_iso());
            add_progress_inner(entry, "Task killed by user");

            // Shutdown any realtime session
            let sid = entry.session_id.clone();
            drop(tasks);
            shutdown_realtime_session(&sid).await;

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
}

async fn set_status(task_id: &str, status: &str) {
    let mut tasks = remote_tasks().lock().await;
    if let Some(entry) = tasks.get_mut(task_id) {
        entry.status = status.to_string();
        if status == "completed" || status == "failed" || status == "killed" {
            entry.completed_at = Some(now_iso());
        }
    }
}

async fn set_result(task_id: &str, result: &str) {
    let mut tasks = remote_tasks().lock().await;
    if let Some(entry) = tasks.get_mut(task_id) {
        entry.result = Some(result.to_string());
    }
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
    let sandbox_dir = if settings.sandbox_dir.is_empty() {
        "sandbox".to_string()
    } else {
        settings.sandbox_dir.clone()
    };

    if api_key.is_empty() {
        add_progress(&task_id, "Error: No API key configured").await;
        set_status(&task_id, "failed").await;
        set_result(&task_id, "No API key configured in settings").await;
        return;
    }

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

        let sub_agent = if let Some(ref cf) = config_file {
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
                    mode: "auto".to_string(),
                    agent_role: "orchestrator".to_string(),
                    cancel_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

pub fn router() -> Router<std::sync::Arc<crate::server::AppState>> {
    Router::new()
        .route("/task", post(submit_task))
        .route("/tasks", get(list_tasks))
        .route("/task/{id}", get(get_task))
        .route("/task/{id}/kill", post(kill_task))
}
