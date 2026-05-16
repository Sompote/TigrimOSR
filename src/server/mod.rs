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
#[allow(dead_code)]
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
    let uri = req.uri().clone();
    let path = uri.path().to_string();

    // Skip auth for verify endpoint
    if path.starts_with("/api/auth/verify") {
        return next.run(req).await;
    }

    // Check if any auth is configured (access_token or remote_token)
    let settings = get_settings().await;
    let remote_token = if settings.remote_enabled == Some(true) {
        settings.remote_token.clone().filter(|t| !t.is_empty())
    } else {
        None
    };

    // If no access_token AND no remote_token configured, allow all (local-only use)
    if state.access_token.is_empty() && remote_token.is_none() {
        return next.run(req).await;
    }

    let token = extract_bearer(req.headers()).or_else(|| extract_query_token(&uri));

    if let Some(ref t) = token {
        // Check main access token
        if !state.access_token.is_empty() && *t == state.access_token {
            return next.run(req).await;
        }
        // Check remote token
        if let Some(ref rt) = remote_token {
            if t == rt {
                return next.run(req).await;
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
    let data_dir = data::data_dir().to_string_lossy().to_string();

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

    // Pre-install bundled skills (web-search)
    install_bundled_skills(&data_dir).await;

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

    // Initialize MCP server connections from settings
    services::mcp::init_mcp_servers().await;

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

    // Web UI (no auth needed — auth happens client-side via API calls)
    let web_ui = routes::web_ui::router();

    // API routes with auth middleware
    let api_with_auth = Router::new()
        .nest("/api", api_routes)
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    // Combine: unauthenticated routes + authenticated API
    let app = Router::new()
        .merge(auth_verify)
        .nest("/web", web_ui.clone())
        .route("/web/", axum::routing::get(|| async {
            axum::response::Redirect::permanent("/web")
        }))
        .merge(api_with_auth)
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

/// Install bundled skills on first run (embedded in the binary via include_str!/include_bytes!)
async fn install_bundled_skills(data_dir: &str) {
    let skills_dir = format!("{}/skills/web-search", data_dir);
    let skill_md_path = format!("{}/SKILL.md", skills_dir);

    // Skip if already installed on disk
    if fs::metadata(&skill_md_path).await.is_ok() {
        return;
    }

    tracing::info!("[init] Installing bundled skill: web-search");

    // Create directories
    let _ = fs::create_dir_all(format!("{}/scripts", skills_dir)).await;

    // Write bundled files
    let skill_md = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/web-search/SKILL.md"));
    let search_py = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/web-search/scripts/search.py"));
    let meta_json = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/web-search/_meta.json"));

    let _ = fs::write(&skill_md_path, skill_md).await;
    let _ = fs::write(format!("{}/scripts/search.py", skills_dir), search_py).await;
    let _ = fs::write(format!("{}/{}", skills_dir, "_meta.json"), meta_json).await;

    // Register in skills.json if not already present
    let skills_json_path = format!("{}/skills.json", data_dir);
    let mut skills: Vec<serde_json::Value> = fs::read_to_string(&skills_json_path)
        .await
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let already_registered = skills.iter().any(|s| s["name"].as_str() == Some("web-search"));
    if !already_registered {
        skills.push(json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "name": "web-search",
            "description": "Search the web using DuckDuckGo — text, news, images, videos with filtering options",
            "source": "bundled",
            "script": "web-search",
            "enabled": true,
            "installedAt": chrono::Utc::now().to_rfc3339()
        }));
        let _ = fs::write(&skills_json_path, serde_json::to_string_pretty(&skills).unwrap_or_default()).await;
        tracing::info!("[init] Registered web-search skill in skills.json");
    }
}
