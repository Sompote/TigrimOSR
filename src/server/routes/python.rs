use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use serde::Deserialize;
use serde_json::json;
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

    // 1. Check configured sandbox dir from settings (resolved to absolute)
    let resolved = crate::server::data::get_sandbox_dir_sync();
    if !resolved.is_empty() {
        if fs::metadata(&resolved).await.is_ok() {
            return resolved;
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

    // 5. Fallback to the OS temp directory (cross-platform).
    let tmp = std::env::temp_dir().join("andrewos_sandbox");
    let _ = fs::create_dir_all(&tmp).await;
    tmp.to_string_lossy().to_string()
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

    // Route through the same sandboxed runner the agents use: dangerous-pattern
    // check, container/sandbox-exec tiers, 60s timeout, kill-on-timeout. The
    // previous direct `python3 file.py` here had NO sandbox and NO timeout.
    let result = crate::server::services::toolbox::run_python_sandboxed(&code, &sandbox_dir).await;

    let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let stdout = result.get("stdout").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut stderr = result.get("stderr").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
        if stderr.is_empty() { stderr = err.to_string(); }
    }
    let exit_code = result.get("exit_code").and_then(|v| v.as_i64())
        .unwrap_or(if ok { 0 } else { 1 });

    (
        StatusCode::OK,
        Json(json!({
            "stdout": stdout,
            "stderr": stderr,
            "code": exit_code,
        })),
    )
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/run", post(run_python))
}
