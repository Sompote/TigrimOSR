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

/// Matches `api_key: <value>` lines in profile YAML (model / evaluation
/// overrides). Group 1 = prefix up to and including any opening quote,
/// group 2 = the value itself (quotes and trailing comments excluded).
fn api_key_line_re() -> regex::Regex {
    regex::Regex::new(r#"(?m)^(\s*api_key\s*:\s*["']?)([^"'\s#][^"'\n#]*)"#).unwrap()
}

/// Mask api_key values in raw profile YAML before returning it to clients.
fn mask_yaml_api_keys(content: &str) -> String {
    let re = api_key_line_re();
    re.replace_all(content, |caps: &regex::Captures| {
        format!(
            "{}{}",
            &caps[1],
            crate::server::routes::settings::mask_key(caps[2].trim())
        )
    })
    .to_string()
}

/// Mask api_key string values anywhere in the parsed profile JSON.
fn mask_json_api_keys(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if k == "api_key" {
                    if let Some(s) = val.as_str() {
                        if !s.is_empty() {
                            *val = json!(crate::server::routes::settings::mask_key(s));
                        }
                    }
                } else {
                    mask_json_api_keys(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                mask_json_api_keys(item);
            }
        }
        _ => {}
    }
}

/// Replace masked api_key placeholders in an incoming profile with the
/// original values from the existing file on disk, so a masked round-trip
/// through the editor doesn't corrupt stored keys.
fn restore_masked_api_keys(incoming: &str, existing: &str) -> String {
    let re = api_key_line_re();
    let mut originals: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for caps in re.captures_iter(existing) {
        let orig = caps[2].trim().to_string();
        if !orig.is_empty() {
            originals.insert(crate::server::routes::settings::mask_key(&orig), orig);
        }
    }
    re.replace_all(incoming, |caps: &regex::Captures| {
        let val = caps[2].trim();
        if val.contains("...") {
            if let Some(orig) = originals.get(val) {
                return format!("{}{}", &caps[1], orig);
            }
        }
        format!("{}{}", &caps[1], &caps[2])
    })
    .to_string()
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
            // Tools gated by a global approval setting whose default is ON —
            // lets editors show which tools require_approval actually changes.
            let approval_gated_by_default = matches!(
                name.as_str(),
                "run_shell" | "cron_create" | "cron_run_now" | "run_python" | "run_react" | "delete_file"
            );
            json!({
                "name": name,
                "description": description,
                "protected": protected,
                "approvalGatedByDefault": approval_gated_by_default,
            })
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
            let masked = mask_yaml_api_keys(&content);
            let mut parsed: Value = serde_yaml::from_str(&content).unwrap_or(Value::Null);
            mask_json_api_keys(&mut parsed);
            (
                StatusCode::OK,
                Json(json!({"filename": filename, "content": masked, "parsed": parsed})),
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
        let is_protected = |name: &str| {
            matches!(
                name,
                "send_task" | "wait_result" | "check_agents" | "create_architecture"
                    | "select_swarm" | "spawn_subagent"
            ) || name.starts_with("proto_")
                || name.starts_with("bb_")
        };
        for (name, cfg) in &tf.config {
            // MCP tool names are dynamic; only checked at runtime.
            if !known.iter().any(|k| k == name) && !is_protected(name) {
                warnings.push(format!(
                    "tools.config: '{}' is not in the base tool catalog (fine if it is an MCP tool name)",
                    name
                ));
            }
            if cfg.timeout_secs == Some(0) {
                return Err(format!("tools.config.{}.timeout_secs must be > 0", name));
            }
            if cfg.max_result_len.map(|m| m > 0 && m < 200).unwrap_or(false) {
                warnings.push(format!(
                    "tools.config.{}.max_result_len below 200 bytes will destroy most tool output",
                    name
                ));
            }
            if let (Some(p), Some(pins)) = (&cfg.params, &cfg.pinned_params) {
                for k in p.keys().filter(|k| pins.contains_key(*k)) {
                    warnings.push(format!(
                        "tools.config.{}: '{}' appears in both params and pinned_params — pinned_params wins",
                        name, k
                    ));
                }
            }
            if is_protected(name)
                && (cfg.require_approval == Some(true)
                    || cfg.enabled == Some(false)
                    || cfg.timeout_secs.is_some())
            {
                warnings.push(format!(
                    "tools.config.{}: require_approval/enabled/timeout_secs are ignored for protected coordination tools while orchestration is active",
                    name
                ));
            }
            if cfg.enabled == Some(false) && tf.mode == "allowlist" && tf.list.iter().any(|t| t == name) {
                warnings.push(format!(
                    "tools.config.{}: enabled:false contradicts its allowlist entry — disabled wins",
                    name
                ));
            }
        }
        // Typo guard: ToolConfig doesn't use deny_unknown_fields, so surface
        // unrecognized keys as warnings instead of silently dropping them.
        if let Ok(raw) = serde_yaml::from_str::<serde_yaml::Value>(content) {
            const KNOWN_KEYS: [&str; 7] = [
                "enabled", "require_approval", "description",
                "params", "pinned_params", "max_result_len", "timeout_secs",
            ];
            if let Some(cfg_map) = raw
                .get("tools")
                .and_then(|t| t.get("config"))
                .and_then(|c| c.as_mapping())
            {
                for (tool, entry) in cfg_map {
                    let tool = tool.as_str().unwrap_or("?");
                    if let Some(fields) = entry.as_mapping() {
                        for key in fields.keys().filter_map(|k| k.as_str()) {
                            if !KNOWN_KEYS.contains(&key) {
                                warnings.push(format!(
                                    "tools.config.{}: unknown key '{}' (expected one of {})",
                                    tool, key, KNOWN_KEYS.join(", ")
                                ));
                            }
                        }
                    }
                }
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
    if let Some(e) = &profile.evaluation {
        if let Some(t) = e.threshold {
            if !(0.0..=1.0).contains(&t) {
                return Err(format!("evaluation.threshold must be within 0.0..=1.0, got {}", t));
            }
        }
        if e.max_retries.map(|v| v > 5).unwrap_or(false) {
            warnings.push("evaluation.max_retries above 5 will be clamped to 5".to_string());
        }
        if e.max_judge_rounds.map(|v| v > 6).unwrap_or(false) {
            warnings.push("evaluation.max_judge_rounds above 6 will be clamped to 6".to_string());
        }
        if e.allow_execute == Some(true) {
            warnings.push(
                "evaluation.allow_execute is on — the evaluator judge may execute code (run_python/run_shell) in the sandbox".to_string(),
            );
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

    // A round-trip through the editor carries masked api_key placeholders —
    // swap the originals from the existing file back in before validating.
    let dir = agent_loops_dir();
    let content = match fs::read_to_string(dir.join(&final_name)).await {
        Ok(existing) => restore_masked_api_keys(&content, &existing),
        Err(_) => content,
    };

    let warnings = match validate_profile_yaml(&content) {
        Ok((_, w)) => w,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))),
    };

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
        Ok(y) => format!("{y}\n{}", crate::server::services::agent_loop::TOOL_CONFIG_EXAMPLE),
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

#[cfg(test)]
mod tests {
    use super::validate_profile_yaml;

    #[test]
    fn tool_config_validation_warnings_and_errors() {
        // timeout_secs: 0 is a hard error.
        let err = validate_profile_yaml(
            "name: x\ntools:\n  config:\n    run_shell: { timeout_secs: 0 }\n",
        );
        assert!(err.is_err());

        let yaml = r#"
name: x
tools:
  mode: allowlist
  list: [run_shell, web_search]
  config:
    run_shell:
      enabled: false
      require_approval: true
      max_result_len: 100
      params: { cwd: "/a" }
      pinned_params: { cwd: "/b" }
      timout_secs: 5
    send_task:
      require_approval: true
    not_a_real_tool:
      enabled: false
"#;
        let (_, warnings) = validate_profile_yaml(yaml).unwrap();
        let joined = warnings.join("\n");
        assert!(joined.contains("max_result_len below 200"), "{joined}");
        assert!(joined.contains("both params and pinned_params"), "{joined}");
        assert!(joined.contains("unknown key 'timout_secs'"), "{joined}");
        assert!(joined.contains("contradicts its allowlist entry"), "{joined}");
        assert!(joined.contains("protected coordination tools"), "{joined}");
        assert!(joined.contains("not_a_real_tool"), "{joined}");
    }

    #[test]
    fn clean_tool_config_passes_without_warnings() {
        let yaml = r#"
name: tooltest
tools:
  mode: all
  list: []
  config:
    run_shell: { require_approval: false, timeout_secs: 5, pinned_params: { cwd: "." } }
    write_file: { require_approval: true }
    read_file: { max_result_len: 300 }
    web_search: { enabled: false }
"#;
        let (profile, warnings) = validate_profile_yaml(yaml).unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            profile.tool_config("run_shell").unwrap().require_approval,
            Some(false)
        );
    }
}
