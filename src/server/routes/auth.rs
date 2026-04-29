use std::sync::Arc;

use axum::{extract::State, response::Json};
use serde_json::{json, Value};

use crate::server::data::get_settings;
use crate::server::AppState;

pub async fn verify_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    if state.access_token.is_empty() {
        return Json(json!({"ok": true, "required": false}));
    }

    let token = body.get("token").and_then(|v| v.as_str()).unwrap_or("");

    if token == state.access_token {
        return Json(json!({"ok": true}));
    }

    if !token.is_empty() {
        let settings = get_settings().await;
        if settings.remote_enabled == Some(true) {
            if let Some(ref rt) = settings.remote_token {
                if token == rt {
                    return Json(json!({"ok": true}));
                }
            }
        }
    }

    Json(json!({"ok": false, "error": "Invalid access token"}))
}
