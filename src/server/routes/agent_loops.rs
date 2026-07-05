// ---------------------------------------------------------------------------
// /api/agent-loops — CRUD for agent-loop profile YAML files
// (data_dir()/agent_loops/*.yaml). Mirrors the shape of routes/agents.rs.
// The active profile is selected via the existing /api/settings
// (agentLoopProfile field).
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

use crate::server::AppState;

use crate::server::services::agent_loop::{
    agent_loops_dir, default_profile_from_settings, ensure_default_profile, AgentLoopProfile,
    DEFAULT_PROFILE_FILE,
};

#[derive(Debug, Deserialize)]
struct SaveProfileBody {
    filename: Option<String>,
    content: Option<String>,
}

/// GET / — list all profile files with parsed metadata.
async fn list_profiles() -> impl IntoResponse {
    ensure_default_profile();
    let dir = agent_loops_dir();
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
        let parsed = serde_yaml::from_str::<AgentLoopProfile>(&content).ok();
        let display_name = parsed
            .as_ref()
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| name.trim_end_matches(".yaml").trim_end_matches(".yml").to_string());
        let description = parsed.as_ref().map(|p| p.description.clone()).unwrap_or_default();
        let updated_at = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.to_rfc3339()
            })
            .unwrap_or_default();

        result.push(json!({
            "filename": name,
            "name": display_name,
            "description": description,
            "valid": parsed.is_some(),
            "updatedAt": updated_at,
        }));
    }

    result.sort_by(|a, b| {
        a["filename"].as_str().unwrap_or("").cmp(b["filename"].as_str().unwrap_or(""))
    });
    Json(json!(result))
}

/// GET /catalog — tool + MCP server + skill catalogs for profile editors.
async fn get_catalog() -> impl IntoResponse {
    let dummy = crate::server::services::toolbox::SubAgentConfig {
        enabled: true,
        ..Default::default()
    };
    let tools: Vec<Value> = crate::server::services::toolbox::tool_catalog()
        .into_iter()
        .map(|(name, description)| {
            let protected = crate::server::services::toolbox::is_protected_tool(&name, &dummy);
            json!({"name": name, "description": description, "protected": protected})
        })
        .collect();

    let settings = crate::server::data::get_settings().await;
    let mcp_servers: Vec<Value> = settings
        .mcp_tools
        .iter()
        .map(|s| json!({"name": s.name, "enabled": s.enabled}))
        .collect();

    let skills: Vec<Value> = crate::server::data::read_json::<Vec<Value>>("skills.json")
        .await
        .into_iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(|n| json!({"name": n})))
        .collect();

    Json(json!({"tools": tools, "mcpServers": mcp_servers, "skills": skills}))
}

/// GET /:filename — read one profile.
async fn get_profile(Path(filename): Path<String>) -> impl IntoResponse {
    let re = regex::Regex::new(r"^[\w\-. ]+\.ya?ml$").unwrap();
    if !re.is_match(&filename) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid filename"})),
        );
    }
    let fp = agent_loops_dir().join(&filename);
    match fs::read_to_string(&fp).await {
        Ok(content) => {
            let parsed: Value = serde_yaml::from_str(&content).unwrap_or(Value::Null);
            (
                StatusCode::OK,
                Json(json!({"filename": filename, "content": content, "parsed": parsed})),
            )
        }
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "File not found"})),
        ),
    }
}

/// Typed validation shared by save: shape errors are rejected, soft issues
/// (unknown tool/mode names) come back as warnings.
pub fn validate_profile_yaml(content: &str) -> Result<(AgentLoopProfile, Vec<String>), String> {
    let profile: AgentLoopProfile =
        serde_yaml::from_str(content).map_err(|e| format!("Invalid profile YAML: {}", e))?;
    let mut warnings = Vec::new();

    if let Some(tf) = &profile.tools {
        if !matches!(tf.mode.as_str(), "allowlist" | "denylist" | "all" | "") {
            return Err(format!("tools.mode must be allowlist|denylist|all, got '{}'", tf.mode));
        }
        let known: Vec<String> = crate::server::services::toolbox::tool_catalog()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        for t in &tf.list {
            if !known.iter().any(|k| k == t) {
                warnings.push(format!("Unknown tool name '{}' (not in the base tool catalog)", t));
            }
        }
    }
    if let Some(m) = &profile.mcp {
        if !matches!(m.mode.as_str(), "all" | "selected" | "none" | "") {
            return Err(format!("mcp.mode must be all|selected|none, got '{}'", m.mode));
        }
    }
    if let Some(s) = &profile.skills {
        if !matches!(s.mode.as_str(), "all" | "selected" | "none" | "") {
            return Err(format!("skills.mode must be all|selected|none, got '{}'", s.mode));
        }
    }
    Ok((profile, warnings))
}

/// POST / — save (create/update) a profile with typed validation.
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
    let content = match &body.content {
        Some(c) if !c.is_empty() => c.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "filename and content required"})),
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

    let warnings = match validate_profile_yaml(&content) {
        Ok((_, w)) => w,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))),
    };

    let dir = agent_loops_dir();
    let _ = fs::create_dir_all(&dir).await;
    match fs::write(dir.join(&final_name), &content).await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"ok": true, "filename": final_name, "warnings": warnings})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to write file: {}", e)})),
        ),
    }
}

/// POST /reset-default — regenerate default.yaml from live settings.
async fn reset_default() -> impl IntoResponse {
    let settings = std::fs::read_to_string(crate::server::data::data_dir().join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    let profile = default_profile_from_settings(&settings);
    let yaml = match serde_yaml::to_string(&profile) {
        Ok(y) => y,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to serialize: {}", e)})),
            )
        }
    };
    let dir = agent_loops_dir();
    let _ = fs::create_dir_all(&dir).await;
    match fs::write(dir.join(DEFAULT_PROFILE_FILE), &yaml).await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"ok": true, "filename": DEFAULT_PROFILE_FILE, "content": yaml})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to write file: {}", e)})),
        ),
    }
}

/// DELETE /:filename — delete a profile file.
async fn delete_profile(Path(filename): Path<String>) -> impl IntoResponse {
    let re = regex::Regex::new(r"^[\w\-. ]+\.ya?ml$").unwrap();
    if !re.is_match(&filename) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid filename"})),
        );
    }
    let fp = agent_loops_dir().join(&filename);
    if fp.exists() {
        let _ = fs::remove_file(&fp).await;
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_profiles).post(save_profile))
        .route("/catalog", get(get_catalog))
        .route("/reset-default", post(reset_default))
        .route("/{filename}", get(get_profile).delete(delete_profile))
}
