//! Messaging-bot HTTP surface.
//!
//! - POST /line/webhook (registered OUTSIDE the auth layer in server::start_server;
//!   LINE cannot send our bearer token — the request is authenticated by the
//!   X-Line-Signature HMAC over the exact body bytes instead).
//! - GET /api/messaging/status (authed) — connection state + the webhook URL
//!   to paste into the LINE Developers console.

use std::sync::Arc;

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::server::data::get_settings;
use crate::server::AppState;

pub fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/status", get(status_handler))
        // Tunnel control lives here (no dedicated tunnel router exists); the
        // web UI uses it to get a public URL for the LINE webhook. Blocked
        // for remote tokens via REMOTE_BLOCKED_PREFIXES.
        .route("/tunnel/start", post(tunnel_start_handler))
        .route("/tunnel/stop", post(tunnel_stop_handler))
}

async fn tunnel_start_handler() -> Json<Value> {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3001);
    Json(crate::server::services::tunnel::start_tunnel(port).await)
}

async fn tunnel_stop_handler() -> Json<Value> {
    Json(crate::server::services::tunnel::stop_tunnel().await)
}

// ---------------------------------------------------------------------------
// POST /line/webhook
// ---------------------------------------------------------------------------

pub async fn line_webhook(headers: HeaderMap, body: Bytes) -> StatusCode {
    let settings = get_settings().await;
    if settings.line_enabled != Some(true) {
        // Ack and drop — LINE's console "Verify" button should succeed even
        // before the bot is fully configured, and we never error-retry-loop.
        return StatusCode::OK;
    }
    let secret = settings.line_channel_secret.clone().unwrap_or_default();
    if secret.is_empty() {
        return StatusCode::OK;
    }

    let signature = headers
        .get("x-line-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // HMAC-SHA256 over the EXACT raw body bytes, base64-encoded.
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    mac.update(&body);
    let expected = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    if !crate::server::ct_eq_pub(&expected, signature) {
        tracing::warn!("[line] webhook signature mismatch — dropping request");
        return StatusCode::FORBIDDEN;
    }

    if let Ok(payload) = serde_json::from_slice::<Value>(&body) {
        // Ack immediately; LINE retries slow responses, which would double-run
        // the agent. Processing continues in the background.
        tokio::spawn(crate::server::services::messaging::line::handle_events(payload));
    }
    StatusCode::OK
}

// ---------------------------------------------------------------------------
// GET /api/messaging/status
// ---------------------------------------------------------------------------

async fn status_handler() -> Json<Value> {
    let settings = get_settings().await;
    let tg = crate::server::services::messaging::telegram::get_status().await;

    let tunnel = crate::server::services::tunnel::get_tunnel_state().await;
    let tunnel_running = tunnel.get("running").and_then(|v| v.as_bool()).unwrap_or(false);
    let tunnel_url = tunnel
        .get("url")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            settings
                .extra
                .get("tunnelUrl")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .filter(|u| !u.is_empty());

    let webhook_url = tunnel_url
        .as_ref()
        .map(|u| format!("{}/line/webhook", u.trim_end_matches('/')));
    let line_hint = if webhook_url.is_some() {
        "Set this URL in LINE Developers Console > Messaging API > Webhook settings. Note: quick tunnels get a new URL each time the tunnel restarts."
    } else {
        "Start the Cloudflare tunnel in Settings first, then re-check this endpoint for the webhook URL."
    };

    Json(json!({
        "telegram": {
            "enabled": settings.telegram_enabled == Some(true),
            "connected": tg.connected,
            "botUsername": tg.bot_username,
            "error": tg.error,
            "allowedUserCount": settings.telegram_allowed_user_ids.as_ref().map(|l| l.len()).unwrap_or(0),
        },
        "line": {
            "enabled": settings.line_enabled == Some(true),
            "webhookUrl": webhook_url,
            "tunnelRunning": tunnel_running,
            "hint": line_hint,
            "allowedUserCount": settings.line_allowed_user_ids.as_ref().map(|l| l.len()).unwrap_or(0),
        },
    }))
}
