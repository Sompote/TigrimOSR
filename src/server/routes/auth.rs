use std::sync::Arc;

use axum::{extract::State, response::Json};
use serde_json::{json, Value};

use crate::server::data::get_settings;
use crate::server::AppState;

pub async fn verify_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    // Check if any auth is configured
    let settings = get_settings().await;
    let remote_token = if settings.remote_enabled == Some(true) {
        settings.remote_token.clone().filter(|t| !t.is_empty())
    } else {
        None
    };

    let has_any_auth = !state.access_token.is_empty() || remote_token.is_some();

    if !has_any_auth {
        return Json(json!({"ok": true, "required": false}));
    }

    let token = body.get("token").and_then(|v| v.as_str()).unwrap_or("");

    if token.is_empty() {
        return Json(json!({"ok": false, "required": true, "error": "Token required"}));
    }

    // Brute-force guard — this endpoint is unauthenticated by design, so it
    // is the natural token-guessing target.
    if crate::server::auth_rate_limited() {
        return Json(
            json!({"ok": false, "error": "Too many failed attempts. Try again in a minute."}),
        );
    }

    // Check access token (constant-time)
    if !state.access_token.is_empty() && crate::server::ct_eq_pub(token, &state.access_token) {
        return Json(json!({"ok": true}));
    }

    // Check remote token (constant-time)
    if let Some(ref rt) = remote_token {
        if crate::server::ct_eq_pub(token, rt) {
            return Json(json!({"ok": true}));
        }
    }

    crate::server::record_auth_fail();
    Json(json!({"ok": false, "error": "Invalid access token"}))
}
