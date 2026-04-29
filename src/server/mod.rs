pub mod data;
pub mod routes;
pub mod services;

use std::sync::Arc;

use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    Router,
};
use serde_json::json;
use tokio::fs;
use tower_http::cors::{Any, CorsLayer};

use data::{generate_token, get_file_tokens, get_settings, is_valid_file_token, save_file_tokens, FileToken};

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AppState {
    pub sandbox_dir: String,
    pub data_dir: String,
    pub access_token: String,
}

// ---------------------------------------------------------------------------
// Auth middleware
// ---------------------------------------------------------------------------

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim_start_matches("Bearer ").to_string())
}

fn extract_query_token(uri: &axum::http::Uri) -> Option<String> {
    uri.query().and_then(|q| {
        q.split('&')
            .find_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                let key = parts.next()?;
                let val = parts.next()?;
                if key == "token" {
                    Some(val.to_string())
                } else {
                    None
                }
            })
    })
}

async fn auth_middleware(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    if state.access_token.is_empty() {
        return next.run(req).await;
    }

    let uri = req.uri().clone();
    let path = uri.path().to_string();

    // Skip auth for verify endpoint
    if path.starts_with("/api/auth/verify") {
        return next.run(req).await;
    }

    let token = extract_bearer(req.headers()).or_else(|| extract_query_token(&uri));

    if let Some(ref t) = token {
        if *t == state.access_token {
            return next.run(req).await;
        }
        // Check remote token
        let settings = get_settings().await;
        if settings.remote_enabled == Some(true) {
            if let Some(ref rt) = settings.remote_token {
                if t == rt {
                    return next.run(req).await;
                }
            }
        }
        // Check file token for /files routes
        if path.starts_with("/api/files") && is_valid_file_token(t).await {
            return next.run(req).await;
        }
    }

    (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response()
}

// ---------------------------------------------------------------------------
// Server bootstrap
// ---------------------------------------------------------------------------

pub async fn start_server(sandbox_dir: String, access_token: String) {
    let data_dir = "data".to_string();

    // Ensure directories
    let dirs = [
        sandbox_dir.clone(),
        data_dir.clone(),
        "skills".to_string(),
        format!("{}/output_file", sandbox_dir),
        format!("{}/agents", data_dir),
    ];
    for dir in &dirs {
        let _ = fs::create_dir_all(dir).await;
    }

    // Ensure data files
    let default_settings = serde_json::to_string_pretty(&json!({
        "sandboxDir": &sandbox_dir,
        "tigerBotApiKey": "",
        "tigerBotModel": "TigerBot-70B-Chat",
        "mcpTools": [],
        "webSearchEnabled": false
    }))
    .unwrap();

    let data_files: Vec<(&str, &str)> = vec![
        ("chat_history.json", "[]"),
        ("tasks.json", "[]"),
        ("settings.json", &default_settings),
        ("skills.json", "[]"),
        ("projects.json", "[]"),
        ("file_tokens.json", "[]"),
    ];
    for (file, default) in &data_files {
        let fp = format!("{}/{}", data_dir, file);
        if fs::metadata(&fp).await.is_err() {
            let _ = fs::write(&fp, default).await;
        }
    }

    // Auto-generate default file token
    let tokens = get_file_tokens().await;
    if tokens.is_empty() {
        let default_token = FileToken {
            id: format!("{:x}", chrono::Utc::now().timestamp()),
            name: "Default".to_string(),
            token: generate_token(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        tracing::info!(
            "[Security] Auto-generated file access token: {}",
            default_token.token
        );
        save_file_tokens(&[default_token]).await;
    }

    let state = Arc::new(AppState {
        sandbox_dir: sandbox_dir.clone(),
        data_dir,
        access_token,
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Build API routes
    let api_routes = routes::build_api_routes(state.clone());

    // Auth verify (no auth needed) - placed before auth middleware
    let auth_verify = Router::new().route(
        "/api/auth/verify",
        axum::routing::post(routes::auth::verify_handler),
    );

    let app = Router::new()
        .merge(auth_verify)
        .nest("/api", api_routes)
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .layer(cors)
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3001);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("Failed to bind port");

    tracing::info!("TigrimOS server running on http://localhost:{}", port);
    tracing::info!("Sandbox directory: {}", sandbox_dir);

    axum::serve(listener, app).await.expect("Server error");
}
