use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::AppState;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ExecBody {
    command: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /exec — execute a shell command and return stdout, stderr, exitCode
async fn exec_command(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExecBody>,
) -> impl IntoResponse {
    let command = match &body.command {
        Some(c) if !c.is_empty() => c.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "No command provided"})),
            )
        }
    };

    let cwd = if state.sandbox_dir.is_empty() {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/tmp".to_string())
    } else {
        state.sandbox_dir.clone()
    };

    let result = tokio::process::Command::new("/bin/bash")
        .arg("-c")
        .arg(&command)
        .current_dir(&cwd)
        .env("TERM", "dumb")
        .output()
        .await;

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(1);
            (
                StatusCode::OK,
                Json(json!({
                    "stdout": stdout,
                    "stderr": stderr,
                    "exitCode": exit_code,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": e.to_string(),
                "exitCode": 1,
            })),
        ),
    }
}

/// GET /history — stub returning empty array
async fn get_history() -> impl IntoResponse {
    Json(json!([]))
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/exec", post(exec_command))
        .route("/history", get(get_history))
}
