use std::sync::Arc;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::fs;
use tracing::info;

use crate::server::data::*;
use crate::server::AppState;

/// Directory where agent YAML configs are stored.
fn agents_dir() -> std::path::PathBuf {
    data_dir().join("agents")
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SaveAgentBody {
    filename: Option<String>,
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParseBody {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GenerateDefinitionBody {
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GenerateSystemBody {
    description: Option<String>,
    #[serde(rename = "architectureType")]
    architecture_type: Option<String>,
    #[serde(rename = "agentCount")]
    agent_count: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ValidateModelBody {
    model: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET / — list all .yml/.yaml files in data/agents/
async fn list_agents() -> impl IntoResponse {
    let dir = agents_dir();
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

        let path = dir.join(&name);
        let content = fs::read_to_string(&path).await.unwrap_or_default();

        // Try to parse YAML to extract metadata
        let parsed: Value = serde_yaml::from_str(&content).unwrap_or(Value::Null);
        let display_name = parsed
            .get("system")
            .and_then(|s| s.get("name"))
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                name.trim_end_matches(".yaml")
                    .trim_end_matches(".yml")
                    .to_string()
            });
        let agent_count = parsed
            .get("agents")
            .and_then(|a| a.as_array())
            .map(|a| a.len())
            .unwrap_or(0);

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
            "agentCount": agent_count,
            "updatedAt": updated_at,
        }));
    }

    Json(json!(result))
}

/// GET /:filename — read a specific agent config file
async fn get_agent(Path(filename): Path<String>) -> impl IntoResponse {
    // Validate filename
    let re = regex::Regex::new(r"^[\w\-. ]+\.ya?ml$").unwrap();
    if !re.is_match(&filename) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid filename"})),
        );
    }

    let fp = agents_dir().join(&filename);
    match fs::read_to_string(&fp).await {
        Ok(content) => {
            let parsed: Value = serde_yaml::from_str(&content).unwrap_or(Value::Null);
            (
                StatusCode::OK,
                Json(json!({
                    "filename": filename,
                    "content": content,
                    "parsed": parsed,
                })),
            )
        }
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "File not found"})),
        ),
    }
}

/// POST / — save (create/update) an agent config
async fn save_agent(Json(body): Json<SaveAgentBody>) -> impl IntoResponse {
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

    // Sanitize filename
    let safe_name: String = filename
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.' || *c == ' ')
        .collect();
    let final_name = if safe_name.ends_with(".yaml") || safe_name.ends_with(".yml") {
        safe_name
    } else {
        format!("{}.yaml", safe_name)
    };

    // Validate YAML
    if let Err(e) = serde_yaml::from_str::<Value>(&content) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid YAML: {}", e)})),
        );
    }

    let dir = agents_dir();
    let _ = fs::create_dir_all(&dir).await;
    let fp = dir.join(&final_name);

    match fs::write(&fp, &content).await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"ok": true, "filename": final_name})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to write file: {}", e)})),
        ),
    }
}

/// DELETE /:filename — delete an agent config file
async fn delete_agent(Path(filename): Path<String>) -> impl IntoResponse {
    let fp = agents_dir().join(&filename);
    if fp.exists() {
        let _ = fs::remove_file(&fp).await;
    }
    Json(json!({"ok": true}))
}

/// POST /parse — parse YAML content
async fn parse_yaml(Json(body): Json<ParseBody>) -> impl IntoResponse {
    let content = body.content.unwrap_or_default();
    match serde_yaml::from_str::<Value>(&content) {
        Ok(parsed) => (StatusCode::OK, Json(json!({"ok": true, "parsed": parsed}))),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": e.to_string()})),
        ),
    }
}

/// POST /generate — generate YAML from editor data (serializes the body back to YAML)
async fn generate_yaml(Json(body): Json<Value>) -> impl IntoResponse {
    match serde_yaml::to_string(&body) {
        Ok(yaml_content) => (
            StatusCode::OK,
            Json(json!({"ok": true, "content": yaml_content})),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": e.to_string()})),
        ),
    }
}

/// POST /validate-model — stub returning {ok: true}
async fn validate_model(Json(body): Json<ValidateModelBody>) -> impl IntoResponse {
    match &body.model {
        Some(m) if !m.is_empty() => Json(json!({
            "ok": true,
            "available": true,
            "warning": "Model validation not yet implemented in Rust backend"
        })),
        _ => Json(json!({
            "ok": false,
            "error": "model is required"
        })),
    }
}

/// POST /generate-definition — generate single agent definition via LLM
async fn generate_definition(Json(body): Json<GenerateDefinitionBody>) -> impl IntoResponse {
    let description = match &body.description {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return Json(json!({"ok": false, "error": "description is required"})),
    };

    let settings = get_settings().await;
    let api_key = &settings.tiger_bot_api_key;
    if api_key.is_empty() {
        return Json(json!({"ok": false, "error": "API key not configured"}));
    }
    let api_url = settings
        .tiger_bot_api_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1/chat/completions");
    let model = if settings.tiger_bot_model.is_empty() {
        "gpt-4o-mini"
    } else {
        &settings.tiger_bot_model
    };

    let user_msg = format!(
        r#"Based on this description, generate a JSON object for an agent definition.

Description: {}

Return ONLY a valid JSON object (no markdown, no code fences) with these fields:
- "name": string (short agent name)
- "role": one of ["orchestrator", "worker", "checker", "reporter", "researcher", "peer"]
- "persona": detailed persona description (2-3 sentences)
- "responsibilities": array of 3-5 responsibility strings

Example:
{{"name": "Code Reviewer", "role": "checker", "persona": "You are a meticulous code reviewer who checks for bugs, security issues, and best practices.", "responsibilities": ["Review code for correctness", "Check for security vulnerabilities", "Suggest improvements"]}}"#,
        description
    );

    match call_llm_simple(api_key, api_url, model, &user_msg, "You are a helpful assistant that generates JSON agent definitions. Return ONLY valid JSON, nothing else.").await {
        Ok(content) => {
            if let Some(parsed) = extract_json_object(&content) {
                Json(json!({"ok": true, "definition": parsed}))
            } else {
                Json(json!({"ok": false, "error": "Failed to parse LLM response", "raw": content}))
            }
        }
        Err(e) => Json(json!({"ok": false, "error": e})),
    }
}

/// POST /generate-system — Auto Architecture: generate complete agent system via LLM
async fn generate_system(Json(body): Json<GenerateSystemBody>) -> impl IntoResponse {
    let description = match &body.description {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return Json(json!({"ok": false, "error": "description is required"})),
    };

    let arch_type = body.architecture_type.as_deref().unwrap_or("hierarchical");
    let count = body
        .agent_count
        .as_ref()
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .unwrap_or_else(|| "auto".to_string());

    let settings = get_settings().await;
    let api_key = &settings.tiger_bot_api_key;
    if api_key.is_empty() {
        return Json(json!({"ok": false, "error": "API key not configured"}));
    }
    let api_url = settings
        .tiger_bot_api_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1/chat/completions");
    let model = if settings.tiger_bot_model.is_empty() {
        "gpt-4o-mini"
    } else {
        &settings.tiger_bot_model
    };

    let p2p_governance = if arch_type == "p2p" {
        r#",
    "p2p_governance": {
      "consensus_mechanism": "contract_net",
      "bid_timeout_seconds": 30,
      "min_confidence_threshold": 0.5,
      "audit_log": true
    }"#
    } else {
        ""
    };

    let p2p_agent_field = if arch_type == "p2p" {
        r#",
      "p2p": { "confidence_domains": ["domain1", "domain2"], "reputation_score": 0.8 }"#
    } else {
        ""
    };

    let count_instruction = if count == "auto" {
        "Determine the optimal number based on the task".to_string()
    } else {
        count.clone()
    };

    let user_msg = format!(
        r#"Based on this description, generate a complete multi-agent system configuration as a JSON object.

User Request: {description}

Architecture Type: {arch_type}
Number of Agents: {count_instruction}

Return ONLY a valid JSON object (no markdown, no code fences) with this exact structure:
{{
  "system": {{
    "name": "System Name",
    "orchestration_mode": "{arch_type}",
    "communication_protocol": "structured_handoff",
    "context_passing": "full_chain"{p2p_governance}
  }},
  "agents": [
    {{
      "id": "unique_snake_case_id",
      "name": "Agent Display Name",
      "role": "one of: human, orchestrator, worker, checker, reporter, researcher, peer",
      "persona": "Detailed 2-3 sentence persona description",
      "responsibilities": ["responsibility 1", "responsibility 2", "responsibility 3"],
      "bus": {{ "enabled": true, "topics": ["topic1", "topic2"] }},
      "mesh": {{ "enabled": true }}{p2p_agent_field}
    }}
  ],
  "connections": [
    {{
      "from": "source_agent_id",
      "to": "target_agent_id",
      "label": "connection_label",
      "protocol": "one of: tcp, queue",
      "topics": ["topic1"]
    }}
  ]
}}

IMPORTANT RULES:

CONNECTION POLICY:
- Connections use ONLY "tcp" or "queue" protocol. NEVER use "bus" as a connection protocol.
- Every non-human agent MUST have at least one incoming TCP or queue connection.
- "tcp" is for direct point-to-point task delegation. "queue" is for async ordered delivery.

BUS POLICY:
- Bus is a separate broadcast channel configured per-agent via "bus.enabled", NOT via connection lines.
- Enable bus on agents that need to share data broadly.

ARCHITECTURE RULES:
- Always include exactly ONE agent with role "human" and id "human" as the entry point
- For hierarchical: human connects to orchestrator, orchestrator connects to all workers/checkers/reporters.
- For flat: human connects to all agents directly
- For mesh: do NOT generate connections — mesh mode bypasses access control.
- For hybrid: human connects to ONE orchestrator. Orchestrator connects to workers via tcp. Workers have "mesh.enabled: true".
- For pipeline: agents form a sequential chain.
- For p2p: ALL non-human agents MUST use role "peer". No connections — uses blackboard coordination.
- Each non-human agent must have a meaningful persona and 3-5 responsibilities
- Agent IDs must be snake_case
- Generate between 3-8 agents (including human) unless user specifies otherwise"#
    );

    info!(
        "[AutoArch] Generating {} system: {}",
        arch_type,
        crate::util::truncate_utf8(&description, 80)
    );

    match call_llm_simple(
        api_key,
        api_url,
        model,
        &user_msg,
        "You are an expert multi-agent system architect. Generate complete, well-structured agent system configurations as JSON. Return ONLY valid JSON, nothing else.",
    )
    .await
    {
        Ok(content) => {
            if let Some(parsed) = extract_json_object(&content) {
                if parsed.get("system").is_some() && parsed.get("agents").is_some() {
                    Json(json!({"ok": true, "system": parsed}))
                } else {
                    Json(json!({"ok": false, "error": "LLM returned invalid structure", "raw": content}))
                }
            } else {
                Json(json!({"ok": false, "error": "Failed to parse LLM response as JSON", "raw": content}))
            }
        }
        Err(e) => Json(json!({"ok": false, "error": e})),
    }
}

/// GET /protocols/status — stub
async fn protocols_status() -> impl IntoResponse {
    Json(json!({
        "tcp": { "active": false },
        "queue": { "active": false },
        "bus": { "active": false },
        "mesh": { "active": false },
        "p2p": { "active": false }
    }))
}

// ---------------------------------------------------------------------------
// LLM helpers
// ---------------------------------------------------------------------------

/// Simple LLM call — returns the assistant content string.
async fn call_llm_simple(
    api_key: &str,
    api_url: &str,
    model: &str,
    user_msg: &str,
    system_msg: &str,
) -> Result<String, String> {
    // Ensure URL ends with /chat/completions
    let url = if api_url.ends_with("/chat/completions") {
        api_url.to_string()
    } else {
        format!("{}/chat/completions", api_url.trim_end_matches('/'))
    };

    let client = Client::new();
    // Kimi "thinking" / reasoning models reject any temperature but 1.
    let temperature = if crate::server::services::toolbox::model_requires_default_temperature(model)
    {
        1.0
    } else {
        0.3
    };
    let body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system_msg},
            {"role": "user", "content": user_msg}
        ],
        "temperature": temperature,
        "max_tokens": 4096,
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("API request failed: {e}"))?;

    let resp_json: Value = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    if let Some(err) = resp_json.get("error") {
        return Err(format!("API error: {}", err));
    }

    let raw = resp_json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // Strip <think>...</think> tags
    let mut s = raw;
    while let Some(start) = s.find("<think>") {
        if let Some(end) = s.find("</think>") {
            s = format!("{}{}", &s[..start], &s[end + 8..]);
        } else {
            break;
        }
    }

    Ok(s.trim().to_string())
}

/// Extract a JSON object from possibly messy LLM output.
fn extract_json_object(text: &str) -> Option<Value> {
    // Try direct parse first
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        if v.is_object() {
            return Some(v);
        }
    }
    // Find first { and last }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        serde_json::from_str(&text[start..=end]).ok()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_agents).post(save_agent))
        .route("/parse", post(parse_yaml))
        .route("/generate", post(generate_yaml))
        .route("/validate-model", post(validate_model))
        .route("/generate-definition", post(generate_definition))
        .route("/generate-system", post(generate_system))
        .route("/protocols/status", get(protocols_status))
        .route("/{filename}", get(get_agent).delete(delete_agent))
}
