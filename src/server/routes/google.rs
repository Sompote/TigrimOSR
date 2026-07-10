// ---------------------------------------------------------------------------
// /api/google — Google quick-connect endpoints for the web/mobile UI.
// Mirrors the desktop Settings → MCP Tools card: uvx status/install, write the
// workspace-mcp settings entry, connect it, and trigger the OAuth login.
// On a headless host the login URL is returned for the client to open
// (the OAuth callback is localhost on the HOST, so remote users may need to
// run the browser there or port-forward — surfaced in the UI hint).
// ---------------------------------------------------------------------------

use std::sync::Arc;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::services::google;
use crate::server::AppState;

/// GET / — uvx availability + current Google MCP entry + connection state.
async fn status() -> Json<Value> {
    let settings = crate::server::data::get_settings().await;
    let entry = settings
        .mcp_tools
        .iter()
        .find(|t| t.name == google::GOOGLE_MCP_NAME);
    let env = entry.and_then(|e| e.env.as_ref());
    let connected =
        crate::server::services::mcp::is_server_connected(google::GOOGLE_MCP_NAME).await;
    Json(json!({
        "uvx": google::find_uvx(),
        "configured": entry.is_some(),
        // Client IDs are public identifiers — safe to return for prefill.
        "clientId": env.and_then(|e| e.get("GOOGLE_OAUTH_CLIENT_ID")).cloned().unwrap_or_default(),
        "hasSecret": env.map(|e| e.contains_key("GOOGLE_OAUTH_CLIENT_SECRET")).unwrap_or(false),
        "email": env.and_then(|e| e.get("USER_GOOGLE_EMAIL")).cloned().unwrap_or_default(),
        "services": entry
            .and_then(|e| e.args.as_ref())
            .map(|args| {
                args.iter()
                    .filter(|a| ["gmail", "calendar", "drive"].contains(&a.as_str()))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        "connected": connected,
        "consoleUrl": google::GOOGLE_CONSOLE_URL,
    }))
}

/// POST /install-uv — run the official uv installer (owner-token only via
/// REMOTE_BLOCKED_PREFIXES; installs software on the host).
async fn install_uv() -> impl IntoResponse {
    match google::install_uv().await {
        Ok(path) => (StatusCode::OK, Json(json!({"ok": true, "uvx": path}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectBody {
    client_id: String,
    /// Empty = keep the secret already stored for this client id (the web UI
    /// never receives the raw secret back).
    #[serde(default)]
    client_secret: String,
    #[serde(default)]
    email: String,
    #[serde(default = "yes")]
    gmail: bool,
    #[serde(default = "yes")]
    calendar: bool,
    #[serde(default = "yes")]
    drive: bool,
}
fn yes() -> bool {
    true
}

/// POST /connect — upsert the MCP entry, (re)connect the server, trigger the
/// Google OAuth login. Returns {ok, message, url?}.
async fn connect(Json(body): Json<ConnectBody>) -> impl IntoResponse {
    if body.client_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "clientId is required"})),
        );
    }
    if !(body.gmail || body.calendar || body.drive) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "select at least one service"})),
        );
    }
    if google::find_uvx().is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "uvx not found — install the uv runtime first"})),
        );
    }

    let mut settings = crate::server::data::get_settings().await;

    // Keep the stored secret when the client sends an empty one for the same
    // client id (secrets are never echoed back to the web UI).
    let mut secret = body.client_secret.trim().to_string();
    if secret.is_empty() {
        if let Some(prev) = settings
            .mcp_tools
            .iter()
            .find(|t| t.name == google::GOOGLE_MCP_NAME)
            .and_then(|t| t.env.as_ref())
        {
            if prev.get("GOOGLE_OAUTH_CLIENT_ID").map(|s| s.trim())
                == Some(body.client_id.trim())
            {
                secret = prev
                    .get("GOOGLE_OAUTH_CLIENT_SECRET")
                    .cloned()
                    .unwrap_or_default();
            }
        }
    }

    let entry = google::build_google_mcp_entry(
        &body.client_id,
        &secret,
        &body.email,
        body.gmail,
        body.calendar,
        body.drive,
    );
    let cfg = json!({
        "name": entry.name,
        "transport": "stdio",
        "command": entry.command,
        "args": entry.args,
        "env": entry.env,
    });

    if let Some(existing) = settings
        .mcp_tools
        .iter_mut()
        .find(|t| t.name == google::GOOGLE_MCP_NAME)
    {
        *existing = entry;
    } else {
        settings.mcp_tools.push(entry);
    }
    crate::server::data::save_settings(&settings).await;

    // Reconnect just the Google server with the fresh config.
    crate::server::services::mcp::disconnect_server(google::GOOGLE_MCP_NAME).await;
    let res = crate::server::services::mcp::connect_server(&cfg).await;
    if !res["ok"].as_bool().unwrap_or(false) {
        return (
            StatusCode::OK,
            Json(json!({
                "ok": false,
                "error": format!(
                    "Failed to start the Google MCP server: {} (first launch downloads packages — retry in ~30s)",
                    res["error"].as_str().unwrap_or("unknown")
                )
            })),
        );
    }

    let login = google::start_login(&body.email).await;
    (StatusCode::OK, Json(login))
}

/// Public (no-auth) relay for the Google OAuth callback.
///
/// The workspace-mcp callback listener binds localhost on the machine RUNNING
/// TigrimOS — a remote browser's redirect to http://localhost:8000/... lands
/// on the USER's machine and dies ("Safari can't connect to the server").
/// Mounting /oauth2callback on the TigrimOS server itself (which the remote
/// browser CAN reach) lets the user rescue the login by replacing
/// `localhost:8000` in the failed redirect URL with the TigrimOS host:port —
/// we forward the query string to the local listener and return its response.
///
/// No bearer auth: Google redirects the user's browser here without our
/// token (same pattern as the LINE webhook). SSRF surface is minimal — the
/// target host and path are fixed; only the query string passes through.
pub async fn oauth_callback_relay(uri: axum::http::Uri) -> axum::response::Response {
    use axum::response::IntoResponse;
    let query = uri.query().unwrap_or("");
    let target = format!(
        "http://127.0.0.1:{}/oauth2callback?{}",
        google::GOOGLE_CALLBACK_PORT,
        query
    );
    match reqwest::Client::new()
        .get(&target)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            let body = resp.text().await.unwrap_or_default();
            (
                status,
                [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                body,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!(
                "OAuth relay: could not reach the Google MCP callback listener on \
                 localhost:{} ({e}). Start the login again from Settings → MCP Tools — \
                 the listener only runs while a login is in progress.",
                google::GOOGLE_CALLBACK_PORT
            ),
        )
            .into_response(),
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(status))
        .route("/install-uv", post(install_uv))
        .route("/connect", post(connect))
}
