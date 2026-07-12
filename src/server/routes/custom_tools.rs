// ---------------------------------------------------------------------------
// /api/custom-tools — CRUD + validate + test for user-defined YAML tools
// (data_dir()/tools/*.yaml). Mirrors routes/agent_loops.rs.
// ---------------------------------------------------------------------------

use std::sync::Arc;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::fs;

use crate::server::services::custom_tools::{
    ensure_examples, tools_dir, validate, CustomTool,
};
use crate::server::AppState;

#[derive(Debug, Deserialize)]
struct SaveBody {
    filename: Option<String>,
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TestBody {
    #[serde(default)]
    args: Value,
}

fn filename_ok(name: &str) -> bool {
    regex::Regex::new(r"^[\w\-. ]+\.ya?ml$").unwrap().is_match(name)
}

fn normalize_filename(name: &str) -> String {
    let safe: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.' || *c == ' ')
        .collect();
    if safe.ends_with(".yaml") || safe.ends_with(".yml") {
        safe
    } else {
        format!("{}.yaml", safe)
    }
}

/// GET / — list tool files with parsed metadata.
async fn list_tools() -> impl IntoResponse {
    ensure_examples();
    let dir = tools_dir();
    let _ = fs::create_dir_all(&dir).await;

    let mut result: Vec<Value> = Vec::new();
    let mut entries = match fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(_) => return Json(json!([])),
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.ends_with(".yaml") || name.ends_with(".yml")) {
            continue;
        }
        let content = fs::read_to_string(dir.join(&name)).await.unwrap_or_default();
        let parsed = serde_yaml::from_str::<CustomTool>(&content);
        let (tool_name, kind, enabled, valid, error) = match &parsed {
            Ok(t) => (t.name.clone(), t.kind.clone(), t.enabled, true, String::new()),
            Err(e) => (
                name.trim_end_matches(".yaml").trim_end_matches(".yml").to_string(),
                String::new(),
                false,
                false,
                e.to_string(),
            ),
        };
        result.push(json!({
            "filename": name,
            "name": tool_name,
            "kind": kind,
            "enabled": enabled,
            "valid": valid,
            "error": error,
        }));
    }
    result.sort_by(|a, b| a["filename"].as_str().unwrap_or("").cmp(b["filename"].as_str().unwrap_or("")));
    Json(json!(result))
}

/// GET /:filename — raw YAML for one tool.
async fn get_tool(Path(filename): Path<String>) -> impl IntoResponse {
    if !filename_ok(&filename) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid filename"})));
    }
    match fs::read_to_string(tools_dir().join(&filename)).await {
        Ok(content) => {
            let parsed: Value = serde_yaml::from_str(&content).unwrap_or(Value::Null);
            (StatusCode::OK, Json(json!({"filename": filename, "content": content, "parsed": parsed})))
        }
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "File not found"}))),
    }
}

/// POST / — validate + save a tool YAML file.
async fn save_tool(Json(body): Json<SaveBody>) -> impl IntoResponse {
    let filename = match &body.filename {
        Some(f) if !f.is_empty() => f.clone(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "filename and content required"}))),
    };
    let content = match &body.content {
        Some(c) if !c.is_empty() => c.clone(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "content required"}))),
    };

    let tool = match serde_yaml::from_str::<CustomTool>(&content) {
        Ok(t) => t,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid tool YAML: {e}")}))),
    };
    let warnings = match validate(&tool) {
        Ok(w) => w,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))),
    };

    let dir = tools_dir();
    let _ = fs::create_dir_all(&dir).await;
    let final_name = normalize_filename(&filename);
    match fs::write(dir.join(&final_name), &content).await {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true, "filename": final_name, "warnings": warnings}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("Failed to write file: {e}")}))),
    }
}

/// POST /:name/test — dry-run a tool with the given args (no LLM).
async fn test_tool(Path(name): Path<String>, Json(body): Json<TestBody>) -> impl IntoResponse {
    if !crate::server::services::custom_tools::is_custom_tool(&name) {
        return (StatusCode::NOT_FOUND, Json(json!({"error": format!("No enabled custom tool named '{name}'")})));
    }
    let sandbox_dir = crate::server::data::data_dir()
        .join("..")
        .join("sandbox")
        .to_string_lossy()
        .to_string();
    let result =
        crate::server::services::custom_tools::execute(&name, &body.args, &sandbox_dir).await;
    (StatusCode::OK, Json(result))
}

/// DELETE /:filename — remove a tool file.
async fn delete_tool(Path(filename): Path<String>) -> impl IntoResponse {
    if !filename_ok(&filename) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid filename"})));
    }
    let fp = tools_dir().join(&filename);
    if fp.exists() {
        let _ = fs::remove_file(&fp).await;
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_tools).post(save_tool))
        .route("/{name}/test", post(test_tool))
        .route("/{filename}", get(get_tool).delete(delete_tool))
}
