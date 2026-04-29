use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::fs;

use crate::server::data::*;
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RunPythonBody {
    code: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the sandbox directory to use as the working directory for Python.
async fn resolve_sandbox_dir(state: &AppState) -> String {
    let settings = get_settings().await;

    // 1. Check configured sandbox dir from settings
    if !settings.sandbox_dir.is_empty() {
        if fs::metadata(&settings.sandbox_dir).await.is_ok() {
            return settings.sandbox_dir;
        }
    }

    // 2. Check state sandbox dir
    if !state.sandbox_dir.is_empty() {
        if fs::metadata(&state.sandbox_dir).await.is_ok() {
            return state.sandbox_dir.clone();
        }
    }

    // 3. Check SANDBOX_DIR env var
    if let Ok(env_dir) = std::env::var("SANDBOX_DIR") {
        if fs::metadata(&env_dir).await.is_ok() {
            return env_dir;
        }
    }

    // 4. Current working directory
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.to_string_lossy().to_string();
        if fs::metadata(&cwd_str).await.is_ok() {
            return cwd_str;
        }
    }

    // 5. Fallback to tmp
    let tmp = format!(
        "{}/tigrimos_sandbox",
        std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string())
    );
    let _ = fs::create_dir_all(&tmp).await;
    tmp
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /run — write Python code to a temp file and execute it via python3
async fn run_python(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RunPythonBody>,
) -> impl IntoResponse {
    let code = match &body.code {
        Some(c) if !c.is_empty() => c.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "code required"})),
            )
        }
    };

    let sandbox_dir = resolve_sandbox_dir(&state).await;

    // Write code to a temporary file
    let tmp_file = format!(
        "/tmp/tigrimos_py_{}_{}.py",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    if let Err(e) = fs::write(&tmp_file, &code).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to write temp file: {}", e)})),
        );
    }

    // Determine python binary
    let settings = get_settings().await;
    let python_bin = settings
        .python_path
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "python3".to_string());

    // Execute
    let result = tokio::process::Command::new(&python_bin)
        .arg(&tmp_file)
        .current_dir(&sandbox_dir)
        .output()
        .await;

    // Clean up temp file
    let _ = fs::remove_file(&tmp_file).await;

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
                    "code": exit_code,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": e.to_string(),
                "stdout": "",
                "stderr": e.to_string(),
                "code": 1,
            })),
        ),
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/run", post(run_python))
}
