// ---------------------------------------------------------------------------
// /api/graph-profiles — CRUD for graph-mode profile YAML files
// (data_dir()/graph/*.yaml) and their judge rule files
// (data_dir()/graph/rules/*.yaml). Mirrors routes/agent_loops.rs; the active
// profile is selected via /api/settings (graphProfile field).
//
// Per-judge api_key values live in the profile YAML and are masked on read /
// restored on save with the shared helpers from routes/agent_loops.rs — the
// same bug class as settings API-key masking, handled in one place.
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

use crate::server::routes::agent_loops::{
    mask_json_api_keys, mask_yaml_api_keys, restore_masked_api_keys,
};
use crate::server::services::graph::{
    default_profile, ensure_default_profile, graph_dir, rules_dir, validate_graph_yaml,
    GraphProfile, DEFAULT_PROFILE_FILE,
};
use crate::server::AppState;

fn filename_ok(name: &str) -> bool {
    regex::Regex::new(r"^[\w\-. ]+\.ya?ml$").unwrap().is_match(name)
}

#[derive(Debug, Deserialize)]
struct SaveProfileBody {
    filename: Option<String>,
    content: Option<String>,
    /// Alternative to `content`: the profile as a JSON object (from form
    /// editors) — serialized to YAML server-side.
    profile: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct SaveRulesBody {
    content: String,
}

/// GET / — list all graph profile files with parsed metadata.
async fn list_profiles() -> impl IntoResponse {
    ensure_default_profile();
    let dir = graph_dir();
    let _ = fs::create_dir_all(&dir).await;

    let mut result: Vec<Value> = Vec::new();
    let mut entries = match fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(_) => return Json(json!([])),
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !entry.file_type().await.map(|t| t.is_file()).unwrap_or(false) {
            continue; // skip the rules/ subdirectory
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.ends_with(".yaml") || name.ends_with(".yml")) {
            continue;
        }
        let content = fs::read_to_string(dir.join(&name)).await.unwrap_or_default();
        let parsed = serde_yaml::from_str::<GraphProfile>(&content).ok();
        let display_name = parsed
            .as_ref()
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| name.trim_end_matches(".yaml").trim_end_matches(".yml").to_string());
        result.push(json!({
            "filename": name,
            "name": display_name,
            "description": parsed.as_ref().map(|p| p.description.clone()).unwrap_or_default(),
            "judges": parsed.as_ref().map(|p| p.judges.len()).unwrap_or(0),
            "workerMode": parsed.as_ref().map(|p| p.worker_mode().to_string()).unwrap_or_default(),
            "valid": parsed.is_some(),
        }));
    }
    result.sort_by(|a, b| {
        a["filename"].as_str().unwrap_or("").cmp(b["filename"].as_str().unwrap_or(""))
    });
    Json(json!(result))
}

/// GET /rules — list judge rule files.
async fn list_rules() -> impl IntoResponse {
    ensure_default_profile();
    let dir = rules_dir();
    let _ = fs::create_dir_all(&dir).await;
    let mut result: Vec<String> = Vec::new();
    if let Ok(mut entries) = fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".yaml") || name.ends_with(".yml") {
                result.push(name);
            }
        }
    }
    result.sort();
    Json(json!(result))
}

/// GET /rules/{filename} — raw rule-file text (rules carry no secrets).
async fn get_rules(Path(filename): Path<String>) -> impl IntoResponse {
    if !filename_ok(&filename) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid filename"})));
    }
    match fs::read_to_string(rules_dir().join(&filename)).await {
        Ok(content) => (StatusCode::OK, Json(json!({"filename": filename, "content": content}))),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "File not found"}))),
    }
}

/// POST /rules/{filename} — save a rule file. Content is free-form YAML text
/// rendered into judge prompts; only well-formedness is checked.
async fn save_rules(
    Path(filename): Path<String>,
    Json(body): Json<SaveRulesBody>,
) -> impl IntoResponse {
    if !filename_ok(&filename) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid filename"})));
    }
    if let Err(e) = serde_yaml::from_str::<serde_yaml::Value>(&body.content) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid YAML: {e}")})));
    }
    let dir = rules_dir();
    let _ = fs::create_dir_all(&dir).await;
    match fs::write(dir.join(&filename), &body.content).await {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true, "filename": filename}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to write file: {e}")})),
        ),
    }
}

/// DELETE /rules/{filename}
async fn delete_rules(Path(filename): Path<String>) -> impl IntoResponse {
    if !filename_ok(&filename) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid filename"})));
    }
    let fp = rules_dir().join(&filename);
    if fp.exists() {
        let _ = fs::remove_file(&fp).await;
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

/// GET /{filename} — read one profile, api_key values masked.
async fn get_profile(Path(filename): Path<String>) -> impl IntoResponse {
    if !filename_ok(&filename) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid filename"})));
    }
    match fs::read_to_string(graph_dir().join(&filename)).await {
        Ok(content) => {
            let masked = mask_yaml_api_keys(&content);
            let mut parsed: Value = serde_yaml::from_str(&content).unwrap_or(Value::Null);
            mask_json_api_keys(&mut parsed);
            (
                StatusCode::OK,
                Json(json!({"filename": filename, "content": masked, "parsed": parsed})),
            )
        }
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "File not found"}))),
    }
}

/// POST / — save (create/update) a profile with typed validation and
/// masked-key restoration.
async fn save_profile(Json(body): Json<SaveProfileBody>) -> impl IntoResponse {
    let filename = match &body.filename {
        Some(f) if !f.is_empty() => f.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "filename and content required"})),
            )
        }
    };
    let content = match (&body.content, &body.profile) {
        (Some(c), _) if !c.is_empty() => c.clone(),
        (_, Some(p)) => {
            match serde_json::from_value::<GraphProfile>(p.clone())
                .map_err(|e| e.to_string())
                .and_then(|prof| serde_yaml::to_string(&prof).map_err(|e| e.to_string()))
            {
                Ok(y) => y,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("Invalid profile object: {e}")})),
                    )
                }
            }
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "filename and content (or profile) required"})),
            )
        }
    };

    let safe_name: String = filename
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.' || *c == ' ')
        .collect();
    let final_name = if safe_name.ends_with(".yaml") || safe_name.ends_with(".yml") {
        safe_name
    } else {
        format!("{}.yaml", safe_name)
    };

    // A round-trip through the editor carries masked api_key placeholders —
    // swap the originals from the existing file back in before validating.
    let dir = graph_dir();
    let content = match fs::read_to_string(dir.join(&final_name)).await {
        Ok(existing) => restore_masked_api_keys(&content, &existing),
        Err(_) => content,
    };

    let warnings = match validate_graph_yaml(&content) {
        Ok((_, w)) => w,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))),
    };

    let _ = fs::create_dir_all(&dir).await;
    match fs::write(dir.join(&final_name), &content).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "filename": final_name, "warnings": warnings})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to write file: {e}")})),
        ),
    }
}

/// POST /reset-default — regenerate default.yaml (and re-seed the default
/// rules file if it went missing).
async fn reset_default() -> impl IntoResponse {
    let yaml = match serde_yaml::to_string(&default_profile()) {
        Ok(y) => y,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to serialize: {e}")})),
            )
        }
    };
    let dir = graph_dir();
    let _ = fs::create_dir_all(&dir).await;
    match fs::write(dir.join(DEFAULT_PROFILE_FILE), &yaml).await {
        Ok(()) => {
            ensure_default_profile(); // re-seed rules/default_rules.yaml if missing
            (
                StatusCode::OK,
                Json(json!({"ok": true, "filename": DEFAULT_PROFILE_FILE, "content": yaml})),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to write file: {e}")})),
        ),
    }
}

/// DELETE /{filename}
async fn delete_profile(Path(filename): Path<String>) -> impl IntoResponse {
    if !filename_ok(&filename) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid filename"})));
    }
    let fp = graph_dir().join(&filename);
    if fp.exists() {
        let _ = fs::remove_file(&fp).await;
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_profiles).post(save_profile))
        .route("/reset-default", post(reset_default))
        .route("/rules", get(list_rules))
        .route("/rules/{filename}", get(get_rules).post(save_rules).delete(delete_rules))
        .route("/{filename}", get(get_profile).delete(delete_profile))
}
