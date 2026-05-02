use std::sync::Arc;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, patch, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::server::data::*;
#[allow(unused_imports)]
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateTaskBody {
    name: Option<String>,
    cron: Option<String>,
    command: Option<String>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpdateTaskBody {
    name: Option<String>,
    cron: Option<String>,
    command: Option<String>,
    enabled: Option<bool>,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_tasks).post(create_task))
        .route("/active", get(list_active))
        .route("/finished", get(list_finished))
        .route("/active/{id}/kill", post(kill_active))
        .route("/{id}", patch(update_task).delete(delete_task))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET / - list all scheduled tasks
async fn list_tasks() -> Json<Vec<ScheduledTask>> {
    Json(get_tasks().await)
}

/// GET /active - list currently running tasks (stub)
async fn list_active() -> Json<Vec<Value>> {
    Json(vec![])
}

/// GET /finished - list recently finished tasks (stub)
async fn list_finished() -> Json<Vec<Value>> {
    Json(vec![])
}

/// POST /active/:id/kill - kill a running task (stub)
async fn kill_active(Path(_id): Path<String>) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": "Task not found or already completed"})),
    )
}

/// POST / - create a new scheduled task
async fn create_task(Json(body): Json<CreateTaskBody>) -> impl IntoResponse {
    let mut tasks = get_tasks().await;

    let task = ScheduledTask {
        id: Uuid::new_v4().to_string(),
        name: body.name.unwrap_or_else(|| "Untitled Task".to_string()),
        cron: body.cron.unwrap_or_else(|| "0 * * * *".to_string()),
        command: body.command.unwrap_or_default(),
        enabled: body.enabled.unwrap_or(true),
        last_run: None,
        last_result: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    tasks.push(task.clone());
    save_tasks(&tasks).await;

    // TODO: if task.enabled { schedule_task(&task); }

    (StatusCode::CREATED, Json(serde_json::to_value(&task).unwrap()))
}

/// PATCH /:id - update an existing task
async fn update_task(
    Path(id): Path<String>,
    Json(body): Json<UpdateTaskBody>,
) -> impl IntoResponse {
    let mut tasks = get_tasks().await;

    let idx = tasks.iter().position(|t| t.id == id);
    let idx = match idx {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Not found"})),
            );
        }
    };

    if let Some(name) = body.name {
        tasks[idx].name = name;
    }
    if let Some(cron) = body.cron {
        tasks[idx].cron = cron;
    }
    if let Some(command) = body.command {
        tasks[idx].command = command;
    }
    if let Some(enabled) = body.enabled {
        tasks[idx].enabled = enabled;
    }

    save_tasks(&tasks).await;

    // TODO: if tasks[idx].enabled { schedule_task(&tasks[idx]); } else { stop_task(&tasks[idx].id); }

    (StatusCode::OK, Json(serde_json::to_value(&tasks[idx]).unwrap()))
}

/// DELETE /:id - delete a task
async fn delete_task(Path(id): Path<String>) -> Json<Value> {
    let tasks = get_tasks().await;
    // TODO: stop_task(&id);
    let tasks: Vec<_> = tasks.into_iter().filter(|t| t.id != id).collect();
    save_tasks(&tasks).await;
    Json(json!({"success": true}))
}
