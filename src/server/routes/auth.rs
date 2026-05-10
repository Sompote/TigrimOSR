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

    // Check access token
    if !state.access_token.is_empty() && token == state.access_token {
        return Json(json!({"ok": true}));
    }

    // Check remote token
    if let Some(ref rt) = remote_token {
        if token == rt {
            return Json(json!({"ok": true}));
        }
    }

    Json(json!({"ok": false, "error": "Invalid access token"}))
}
