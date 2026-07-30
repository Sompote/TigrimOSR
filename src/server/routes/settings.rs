use std::sync::Arc;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::data::*;
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Mask a sensitive string: show first 8 chars + "..." + last 4 chars.
/// Returns the original if shorter than 12 chars.
pub(crate) fn mask_key(s: &str) -> String {
    if s.len() <= 12 {
        return s.to_string();
    }
    format!("{}...{}", &s[..8], &s[s.len() - 4..])
}

/// Returns true if the value is a masked placeholder (contains "...").
fn is_masked(v: &Value) -> bool {
    v.as_str().map(|s| s.contains("...")).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(get_settings_handler).put(put_settings_handler))
        .route("/soul-identity", get(get_soul_identity).put(put_soul_identity))
        .route("/test-connection", post(test_connection))
        .route("/remote-token", get(get_remote_token))
        .route("/remote-token/regenerate", post(regenerate_remote_token))
        .route("/remote-instances/test", post(test_remote_instance))
        .route("/claude-code-oauth", get(get_claude_code_oauth))
        .route("/cli-models", get(get_cli_models))
        .route("/cli-models/refresh", post(refresh_cli_models))
        .route("/file-tokens", get(list_file_tokens).post(create_file_token))
        .route("/file-tokens/{id}", delete(delete_file_token))
        .route("/file-tokens/{id}/regenerate", post(regenerate_file_token))
        .route("/mcp/status", get(mcp_status))
        .route("/mcp/connect", post(mcp_connect))
        .route("/mcp/disconnect", post(mcp_disconnect))
        .route("/mcp/reconnect-all", post(mcp_reconnect_all))
}

// ---------------------------------------------------------------------------
// GET/PUT /soul-identity — orchestrator SOUL.md / IDENTITY.md
// (persona files injected into the system prompt; lets a connected desktop
// configure the persona of THIS server's orchestrator)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SoulIdentityBody {
    #[serde(default)]
    soul: String,
    #[serde(default)]
    identity: String,
}

async fn get_soul_identity() -> Json<Value> {
    let dir = data_dir();
    let soul = tokio::fs::read_to_string(dir.join("SOUL.md")).await.unwrap_or_default();
    let identity = tokio::fs::read_to_string(dir.join("IDENTITY.md")).await.unwrap_or_default();
    Json(json!({ "ok": true, "soul": soul, "identity": identity }))
}

async fn put_soul_identity(Json(body): Json<SoulIdentityBody>) -> Json<Value> {
    let dir = data_dir();
    if body.soul.trim().is_empty() {
        let _ = tokio::fs::remove_file(dir.join("SOUL.md")).await;
    } else {
        let _ = tokio::fs::write(dir.join("SOUL.md"), &body.soul).await;
    }
    if body.identity.trim().is_empty() {
        let _ = tokio::fs::remove_file(dir.join("IDENTITY.md")).await;
    } else {
        let _ = tokio::fs::write(dir.join("IDENTITY.md"), &body.identity).await;
    }
    Json(json!({ "ok": true }))
}

// ---------------------------------------------------------------------------
// GET / -- get settings (with masked keys)
// ---------------------------------------------------------------------------

async fn get_settings_handler() -> Json<Value> {
    let settings = get_settings().await;
    let mut v = serde_json::to_value(&settings).unwrap_or(json!({}));

    // Mask top-level API keys
    for key in &[
        "tigerBotApiKey",
        "webSearchApiKey",
        "openRouterSearchApiKey",
        "telegramBotToken",
        "lineChannelSecret",
        "lineChannelAccessToken",
    ] {
        if let Some(val) = v.get(*key).and_then(|v| v.as_str()) {
            if !val.is_empty() {
                v[*key] = json!(mask_key(val));
            }
        }
    }

    // Mask provider_*_apiKey fields (in the `extra` flatten)
    let keys_to_mask: Vec<String> = v
        .as_object()
        .map(|obj| {
            obj.keys()
                .filter(|k| k.starts_with("provider_") && k.ends_with("_apiKey"))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for key in keys_to_mask {
        if let Some(val) = v.get(&key).and_then(|v| v.as_str()) {
            if !val.is_empty() {
                v[key] = json!(mask_key(val));
            }
        }
    }

    // Mask router model-pool API keys (each pool entry carries its own key)
    if let Some(pool) = v.get_mut("modelPool").and_then(|p| p.as_array_mut()) {
        for entry in pool.iter_mut() {
            if let Some(k) = entry.get("api_key").and_then(|k| k.as_str()) {
                if !k.is_empty() {
                    entry["api_key"] = json!(mask_key(k));
                }
            }
        }
    }

    // Mask MCP tool header AND env values that look sensitive (env carries
    // e.g. GOOGLE_OAUTH_CLIENT_SECRET for the Google quick-connect).
    if let Some(tools) = v.get_mut("mcpTools").and_then(|t| t.as_array_mut()) {
        for tool in tools.iter_mut() {
            for section in ["headers", "env"] {
                if let Some(map) = tool.get_mut(section).and_then(|h| h.as_object_mut()) {
                    let sensitive_keys: Vec<String> = map
                        .keys()
                        .filter(|k| {
                            let lower = k.to_lowercase();
                            // OAuth client IDs are public identifiers — leave
                            // them readable (GOOGLE_OAUTH_CLIENT_ID contains
                            // "auth" but is not a secret).
                            if lower.ends_with("_id") || lower.contains("client_id") {
                                return false;
                            }
                            lower.contains("auth")
                                || lower.contains("key")
                                || lower.contains("secret")
                                || lower.contains("token")
                                || lower.contains("bearer")
                                || lower.contains("password")
                        })
                        .cloned()
                        .collect();
                    for hk in sensitive_keys {
                        if let Some(hv) = map.get(&hk).and_then(|v| v.as_str()) {
                            if hv.len() > 12 {
                                map.insert(hk, json!(mask_key(hv)));
                            }
                        }
                    }
                }
            }
        }
    }

    // Mask remote token
    if let Some(val) = v.get("remoteToken").and_then(|v| v.as_str()) {
        if !val.is_empty() {
            v["remoteToken"] = json!(mask_key(val));
        }
    }

    // Mask remote instance tokens
    if let Some(instances) = v.get_mut("remoteInstances").and_then(|i| i.as_array_mut()) {
        for ri in instances.iter_mut() {
            if let Some(tok) = ri.get("token").and_then(|t| t.as_str()) {
                if tok.len() > 12 {
                    ri["token"] = json!(mask_key(tok));
                }
            }
        }
    }

    Json(v)
}

// ---------------------------------------------------------------------------
// PUT / -- save settings (preserve masked values)
// ---------------------------------------------------------------------------

async fn put_settings_handler(Json(body): Json<Value>) -> Json<Value> {
    let current = get_settings().await;
    let current_v = serde_json::to_value(&current).unwrap_or(json!({}));

    let mut updated = current_v.clone();
    // Merge body into current
    if let (Some(base), Some(incoming)) = (updated.as_object_mut(), body.as_object()) {
        for (k, v) in incoming {
            base.insert(k.clone(), v.clone());
        }
    }

    // Don't overwrite keys with masked values
    for key in &[
        "tigerBotApiKey",
        "webSearchApiKey",
        "openRouterSearchApiKey",
        "telegramBotToken",
        "lineChannelSecret",
        "lineChannelAccessToken",
    ] {
        if let Some(val) = body.get(*key) {
            if is_masked(val) {
                if let Some(orig) = current_v.get(*key) {
                    updated[*key] = orig.clone();
                }
            }
        }
    }

    // Don't overwrite provider_*_apiKey with masked values
    if let Some(obj) = body.as_object() {
        for (k, v) in obj {
            if k.starts_with("provider_") && k.ends_with("_apiKey") && is_masked(v) {
                if let Some(orig) = current_v.get(k) {
                    updated[k] = orig.clone();
                }
            }
        }
    }

    // Don't overwrite router model-pool API keys with masked values. Entry ids
    // are re-derived from label/model by the web UI, so match the masked
    // placeholder back to whichever current key produces that mask, falling
    // back to an id match.
    if let (Some(body_pool), Some(current_pool)) = (
        body.get("modelPool").and_then(|p| p.as_array()),
        current_v.get("modelPool").and_then(|p| p.as_array()),
    ) {
        let mut restored_pool = body_pool.clone();
        for entry in restored_pool.iter_mut() {
            let masked = match entry.get("api_key") {
                Some(v) if is_masked(v) => v.as_str().unwrap_or("").to_string(),
                _ => continue,
            };
            let entry_id = entry.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            let orig = current_pool
                .iter()
                .filter_map(|o| o.get("api_key").and_then(|k| k.as_str()))
                .find(|k| mask_key(k) == masked)
                .or_else(|| {
                    current_pool
                        .iter()
                        .find(|o| o.get("id").and_then(|i| i.as_str()).unwrap_or("") == entry_id)
                        .and_then(|o| o.get("api_key").and_then(|k| k.as_str()))
                });
            if let Some(orig) = orig {
                entry["api_key"] = json!(orig);
            }
        }
        updated["modelPool"] = json!(restored_pool);
    }

    // Don't overwrite MCP header/env values with masked values
    if let (Some(body_tools), Some(current_tools)) = (
        body.get("mcpTools").and_then(|t| t.as_array()),
        current_v.get("mcpTools").and_then(|t| t.as_array()),
    ) {
        let mut restored_tools = body_tools.clone();
        for tool in restored_tools.iter_mut() {
            let tool_name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let current_tool = current_tools
                .iter()
                .find(|t| t.get("name").and_then(|n| n.as_str()).unwrap_or("") == tool_name)
                .cloned();
            for section in ["headers", "env"] {
                if let Some(map) = tool.get_mut(section).and_then(|h| h.as_object_mut()) {
                    if let Some(ct_map) = current_tool
                        .as_ref()
                        .and_then(|ct| ct.get(section))
                        .and_then(|h| h.as_object())
                    {
                        let masked_keys: Vec<String> = map
                            .iter()
                            .filter(|(_, v)| is_masked(v))
                            .map(|(k, _)| k.clone())
                            .collect();
                        for hk in masked_keys {
                            if let Some(orig) = ct_map.get(&hk) {
                                map.insert(hk, orig.clone());
                            }
                        }
                    }
                }
            }
        }
        updated["mcpTools"] = json!(restored_tools);
    }

    // Don't overwrite remote token with masked value
    if let Some(val) = body.get("remoteToken") {
        if is_masked(val) {
            if let Some(orig) = current_v.get("remoteToken") {
                updated["remoteToken"] = orig.clone();
            }
        }
    }

    // Don't overwrite remote instance tokens with masked values
    if let (Some(body_instances), Some(current_instances)) = (
        body.get("remoteInstances").and_then(|i| i.as_array()),
        current_v.get("remoteInstances").and_then(|i| i.as_array()),
    ) {
        let mut restored = body_instances.clone();
        for ri in restored.iter_mut() {
            if let Some(tok) = ri.get("token") {
                if is_masked(tok) {
                    let ri_id = ri.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    if let Some(orig) = current_instances
                        .iter()
                        .find(|o| o.get("id").and_then(|i| i.as_str()).unwrap_or("") == ri_id)
                    {
                        if let Some(orig_tok) = orig.get("token") {
                            ri["token"] = orig_tok.clone();
                        }
                    }
                }
            }
        }
        updated["remoteInstances"] = json!(restored);
    }

    // Deserialize back into Settings and save
    match serde_json::from_value::<Settings>(updated) {
        Ok(settings) => {
            save_settings(&settings).await;
            Json(json!({"success": true}))
        }
        Err(e) => Json(json!({"success": false, "error": e.to_string()})),
    }
}

// ---------------------------------------------------------------------------
// POST /test-connection
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TestConnectionBody {
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
    #[serde(rename = "apiUrl")]
    api_url: Option<String>,
    model: Option<String>,
    provider: Option<String>,
}

async fn test_connection(Json(body): Json<TestConnectionBody>) -> Json<Value> {
    let api_key = body.api_key.unwrap_or_default();
    let api_url = body.api_url.unwrap_or_default();
    let model = body.model.unwrap_or_default();
    let provider = body.provider.unwrap_or_default();

    let is_anthropic = provider == "anthropic_claude_code"
        || (!api_url.is_empty() && api_url.contains("api.anthropic.com"));

    let client = reqwest::Client::new();

    if is_anthropic {
        let base = if api_url.is_empty() {
            "https://api.anthropic.com/v1".to_string()
        } else {
            api_url
                .trim_end_matches('/')
                .trim_end_matches("/messages")
                .to_string()
        };
        let url = format!("{}/messages", base);

        let is_oauth = api_key.starts_with("sk-ant-oat01-");
        let mut req = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01");

        if is_oauth {
            req = req.header("Authorization", format!("Bearer {}", api_key));
        } else {
            req = req.header("x-api-key", &api_key);
        }

        let model_name = if model.is_empty() {
            "claude-sonnet-4-20250514".to_string()
        } else {
            model
        };

        let result = req
            .json(&json!({
                "model": model_name,
                "messages": [{"role": "user", "content": "Hello"}],
                "max_tokens": 10
            }))
            .send()
            .await;

        match result {
            Ok(resp) => {
                if resp.status().is_success() {
                    Json(json!({"success": true, "message": "Connection successful (Anthropic API)"}))
                } else {
                    let status = resp.status().as_u16();
                    let err_text = resp.text().await.unwrap_or_default();
                    Json(json!({"success": false, "message": format!("Error {}: {}", status, err_text)}))
                }
            }
            Err(e) => Json(json!({"success": false, "message": e.to_string()})),
        }
    } else {
        let raw_url = if api_url.is_empty() {
            "https://api.tigerbot.com/bot-chat/openai/v1/chat/completions".to_string()
        } else {
            api_url
        };

        let url = if raw_url.ends_with("/chat/completions") {
            raw_url.clone()
        } else {
            format!("{}/chat/completions", raw_url.trim_end_matches('/'))
        };

        let is_kimi =
            provider == "kimi" || raw_url.contains("api.kimi.com");

        let mut req = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key));

        if is_kimi {
            req = req
                .header("User-Agent", "claude-code/1.0.6")
                .header("X-Client-Name", "claude-code")
                .header("X-Client-Version", "1.0.6")
                .header("HTTP-Referer", "https://claude.ai")
                .header("X-Traffic-Source", "claude-code");
        }

        let model_name = if model.is_empty() {
            "TigerBot-70B-Chat".to_string()
        } else {
            model
        };

        let result = req
            .json(&json!({
                "model": model_name,
                "messages": [{"role": "user", "content": "Hello"}],
                "max_tokens": 10
            }))
            .send()
            .await;

        match result {
            Ok(resp) => {
                if resp.status().is_success() {
                    Json(json!({"success": true, "message": "Connection successful"}))
                } else {
                    let status = resp.status().as_u16();
                    let err_text = resp.text().await.unwrap_or_default();
                    Json(json!({"success": false, "message": format!("Error {}: {}", status, err_text)}))
                }
            }
            Err(e) => Json(json!({"success": false, "message": e.to_string()})),
        }
    }
}

// ---------------------------------------------------------------------------
// Remote Token
// ---------------------------------------------------------------------------

async fn get_remote_token() -> Json<Value> {
    let mut settings = get_settings().await;
    if settings.remote_token.is_none() || settings.remote_token.as_deref() == Some("") {
        settings.remote_token = Some(generate_token());
        save_settings(&settings).await;
    }
    Json(json!({"token": settings.remote_token}))
}

async fn regenerate_remote_token() -> Json<Value> {
    let mut settings = get_settings().await;
    settings.remote_token = Some(generate_token());
    save_settings(&settings).await;
    Json(json!({"token": settings.remote_token}))
}

// ---------------------------------------------------------------------------
// Remote Instance Test (stub)
// ---------------------------------------------------------------------------

async fn test_remote_instance(Json(body): Json<Value>) -> impl IntoResponse {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "id is required"})),
        );
    }
    // Stub: return ok
    (StatusCode::OK, Json(json!({"ok": true, "message": "pong"})))
}

// ---------------------------------------------------------------------------
// Claude Code OAuth
// ---------------------------------------------------------------------------
// GET  /cli-models          — models + effort levels of the CLIs installed here
// POST /cli-models/refresh  — same, bypassing the cache
//
// Backs the provider/model/effort dropdowns in Settings, the Sub-Agent roster,
// and the chat composer, so none of them carry a hardcoded model list.
// ---------------------------------------------------------------------------

async fn get_cli_models() -> Json<Value> {
    cli_models_payload(false).await
}

async fn refresh_cli_models() -> Json<Value> {
    cli_models_payload(true).await
}

async fn cli_models_payload(force: bool) -> Json<Value> {
    let providers = crate::server::services::cli_models::get_providers(force).await;
    Json(json!({
        "success": true,
        "providers": providers,
        "available": providers.iter().filter(|p| p.available).count(),
    }))
}

// ---------------------------------------------------------------------------

async fn get_claude_code_oauth() -> Json<Value> {
    let home = dirs::home_dir().unwrap_or_default();
    let cred_path = home.join(".claude").join(".credentials.json");

    let raw = match tokio::fs::read_to_string(&cred_path).await {
        Ok(content) => content,
        Err(_) => {
            return Json(json!({
                "success": false,
                "message": "Claude Code credentials not found. Make sure Claude Code is installed and you are logged in."
            }));
        }
    };

    let creds: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            return Json(json!({
                "success": false,
                "message": "Failed to parse credentials file."
            }));
        }
    };

    let oauth = match creds.get("claudeAiOauth") {
        Some(o) => o,
        None => {
            return Json(json!({
                "success": false,
                "message": "No Claude Code OAuth token found. Please log in to Claude Code first."
            }));
        }
    };

    let access_token = match oauth.get("accessToken").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return Json(json!({
                "success": false,
                "message": "No Claude Code OAuth token found. Please log in to Claude Code first."
            }));
        }
    };

    // Check expiry
    if let Some(expires_at) = oauth.get("expiresAt").and_then(|v| v.as_i64()) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        if expires_at < now_ms {
            return Json(json!({
                "success": false,
                "message": "Claude Code OAuth token has expired. Please refresh your Claude Code session."
            }));
        }
    }

    Json(json!({
        "success": true,
        "accessToken": access_token,
        "expiresAt": oauth.get("expiresAt"),
        "subscriptionType": oauth.get("subscriptionType"),
    }))
}

// ---------------------------------------------------------------------------
// File Tokens
// ---------------------------------------------------------------------------

async fn list_file_tokens() -> Json<Value> {
    let tokens = get_file_tokens().await;
    Json(serde_json::to_value(&tokens).unwrap_or(json!([])))
}

async fn create_file_token(Json(body): Json<Value>) -> Json<Value> {
    let mut tokens = get_file_tokens().await;
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let token_name = if name.is_empty() {
        format!("Token {}", tokens.len() + 1)
    } else {
        name
    };

    let new_token = FileToken {
        id: format!("{:x}", chrono::Utc::now().timestamp()),
        name: token_name,
        token: generate_token(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    tokens.push(new_token.clone());
    save_file_tokens(&tokens).await;
    Json(serde_json::to_value(&new_token).unwrap_or(json!({})))
}

async fn delete_file_token(Path(id): Path<String>) -> Json<Value> {
    let tokens = get_file_tokens().await;
    let filtered: Vec<FileToken> = tokens.into_iter().filter(|t| t.id != id).collect();
    save_file_tokens(&filtered).await;
    Json(json!({"success": true}))
}

async fn regenerate_file_token(Path(id): Path<String>) -> impl IntoResponse {
    let mut tokens = get_file_tokens().await;
    let token = tokens.iter_mut().find(|t| t.id == id);
    match token {
        Some(t) => {
            t.token = generate_token();
            let result = serde_json::to_value(&*t).unwrap_or(json!({}));
            save_file_tokens(&tokens).await;
            (StatusCode::OK, Json(result))
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Token not found"})),
        ),
    }
}

// ---------------------------------------------------------------------------
// MCP stubs
// ---------------------------------------------------------------------------

async fn mcp_status() -> Json<Value> {
    let status = crate::server::services::mcp::get_connection_status().await;
    Json(json!(status))
}

async fn mcp_connect(Json(body): Json<Value>) -> impl IntoResponse {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() || url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "name and url required"})),
        );
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

async fn mcp_disconnect(Json(body): Json<Value>) -> impl IntoResponse {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "name required"})),
        );
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

async fn mcp_reconnect_all() -> Json<Value> {
    use crate::server::services::mcp;
    mcp::disconnect_all().await;
    mcp::init_mcp_servers().await;
    let status = mcp::get_connection_status().await;
    Json(json!({"ok": true, "status": status}))
}
