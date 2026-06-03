use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Multipart, Path},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, patch, post, put},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::services::plugin;
#[allow(unused_imports)]
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_plugins))
        .route("/upload", post(upload_plugin))
        .route("/{id}", get(get_plugin).patch(toggle_plugin).delete(delete_plugin))
        .route("/{id}/readme", get(get_readme))
        .route("/{id}/icon", get(get_icon))
        .route("/{id}/connectors", get(list_connectors))
        .route("/{id}/connectors/{service}/config", get(get_connector_config).put(save_connector_config))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET / — list all installed plugins
async fn list_plugins() -> impl IntoResponse {
    let plugins = plugin::list_plugins().await;
    Json(json!(plugins))
}

/// POST /upload — install plugin from zip (multipart)
async fn upload_plugin(mut multipart: Multipart) -> impl IntoResponse {
    let field = match multipart.next_field().await {
        Ok(Some(f)) => f,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "No file uploaded"})),
            );
        }
    };

    let bytes = match field.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            );
        }
    };

    match plugin::install_plugin(&bytes).await {
        Ok(installed) => (StatusCode::OK, Json(json!(installed))),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": e}))),
    }
}

/// GET /{id} — get plugin details
async fn get_plugin(Path(id): Path<String>) -> impl IntoResponse {
    match plugin::get_plugin(&id).await {
        Some(p) => (StatusCode::OK, Json(json!(p))),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "Plugin not found"}))),
    }
}

/// PATCH /{id} — toggle enable/disable
#[derive(Deserialize)]
struct ToggleBody {
    enabled: Option<bool>,
}

async fn toggle_plugin(Path(id): Path<String>, Json(body): Json<ToggleBody>) -> impl IntoResponse {
    let enabled = body.enabled.unwrap_or(true);
    match plugin::toggle_plugin(&id, enabled).await {
        Ok(p) => (StatusCode::OK, Json(json!(p))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e}))),
    }
}

/// DELETE /{id} — uninstall plugin
async fn delete_plugin(Path(id): Path<String>) -> impl IntoResponse {
    match plugin::uninstall_plugin(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e}))),
    }
}

/// GET /{id}/readme
async fn get_readme(Path(id): Path<String>) -> impl IntoResponse {
    match plugin::get_plugin_readme(&id).await {
        Some(content) => (StatusCode::OK, Json(json!({"content": content}))),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "No README found"}))),
    }
}

/// GET /{id}/icon
async fn get_icon(Path(id): Path<String>) -> Response {
    match plugin::get_plugin_icon(&id).await {
        Some(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| Response::new(Body::empty())),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("{\"error\":\"No icon found\"}"))
            .unwrap_or_else(|_| Response::new(Body::empty())),
    }
}

/// GET /{id}/connectors — list connectors with config status
async fn list_connectors(Path(id): Path<String>) -> impl IntoResponse {
    let p = match plugin::get_plugin(&id).await {
        Some(p) => p,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error": "Plugin not found"}))),
    };

    let mut connectors = Vec::new();
    for conn in &p.components.connectors {
        let cfg = plugin::get_connector_config(&id, &conn.service).await;
        let configured = cfg.as_object().map(|m| !m.is_empty()).unwrap_or(false);
        connectors.push(json!({
            "name": conn.name,
            "service": conn.service,
            "description": conn.description,
            "config_fields": conn.config_fields,
            "configured": configured,
        }));
    }
    (StatusCode::OK, Json(json!(connectors)))
}

/// GET /{id}/connectors/{service}/config
async fn get_connector_config(Path((id, service)): Path<(String, String)>) -> impl IntoResponse {
    let config = plugin::get_connector_config(&id, &service).await;
    (StatusCode::OK, Json(config))
}

/// PUT /{id}/connectors/{service}/config
async fn save_connector_config(
    Path((id, service)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    match plugin::save_connector_config(&id, &service, body).await {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    }
}
