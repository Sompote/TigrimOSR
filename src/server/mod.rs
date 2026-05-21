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

/// Bundled skill definition: name, description, SKILL.md content, optional extra files
struct BundledSkill {
    name: &'static str,
    description: &'static str,
    skill_md: &'static str,
    meta_json: &'static str,
    extra_files: &'static [(&'static str, &'static str)], // (relative_path, content)
}

const BUNDLED_SKILLS: &[BundledSkill] = &[
    BundledSkill {
        name: "web-search",
        description: "Search the web using DuckDuckGo — text, news, images, videos with filtering options",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/web-search/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/web-search/_meta.json")),
        extra_files: &[
            ("scripts/search.py", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/web-search/scripts/search.py"))),
        ],
    },
    BundledSkill {
        name: "code-review",
        description: "Automated code review — analyzes files for bugs, security issues, style violations, and improvements",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/code-review/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/code-review/_meta.json")),
        extra_files: &[],
    },
    BundledSkill {
        name: "doc-generator",
        description: "Generate documentation from source code — README, API docs, module summaries",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/doc-generator/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/doc-generator/_meta.json")),
        extra_files: &[],
    },
    BundledSkill {
        name: "test-scaffold",
        description: "Scaffold unit and integration tests with proper assertions and edge case coverage",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/test-scaffold/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/test-scaffold/_meta.json")),
        extra_files: &[],
    },
    BundledSkill {
        name: "debug-assist",
        description: "Intelligent debugging — analyzes errors, stack traces, and logs to find root causes",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/debug-assist/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/debug-assist/_meta.json")),
        extra_files: &[],
    },
    BundledSkill {
        name: "refactor-bot",
        description: "Suggest and apply code refactoring — simplify, extract, deduplicate, modernize",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/refactor-bot/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/refactor-bot/_meta.json")),
        extra_files: &[],
    },
    BundledSkill {
        name: "file-search",
        description: "Fast recursive file search using name patterns, content grep, and project structure analysis",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/file-search/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/file-search/_meta.json")),
        extra_files: &[],
    },
    BundledSkill {
        name: "git-summarize",
        description: "Summarize git history into changelogs, release notes, and activity reports",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/git-summarize/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/git-summarize/_meta.json")),
        extra_files: &[],
    },
    BundledSkill {
        name: "env-check",
        description: "Validate environment variables, dependencies, and system requirements",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/env-check/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/env-check/_meta.json")),
        extra_files: &[],
    },
    // --- Uploaded / user-contributed skills (now bundled) ---
    BundledSkill {
        name: "literature-review",
        description: "Search academic sources via Semantic Scholar, OpenAlex, Crossref and PubMed for literature reviews with proper citations",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/literature-review/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/literature-review/_meta.json")),
        extra_files: &[
            ("scripts/lit_search.py", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/literature-review/scripts/lit_search.py"))),
        ],
    },
    BundledSkill {
        name: "twitter-search",
        description: "Advanced Twitter/X search and social media data analysis — fetch tweets, trend analysis, sentiment analysis",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/twitter-search/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/twitter-search/_meta.json")),
        extra_files: &[
            ("scripts/twitter_search.py", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/twitter-search/scripts/twitter_search.py"))),
            ("scripts/run_search.sh", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/twitter-search/scripts/run_search.sh"))),
            ("references/twitter_api.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/twitter-search/references/twitter_api.md"))),
            ("README.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/twitter-search/README.md"))),
        ],
    },
    BundledSkill {
        name: "pdf",
        description: "PDF manipulation toolkit — extract text/tables, create, merge/split, fill forms, and analyze PDF documents",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/pdf/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/pdf/_meta.json")),
        extra_files: &[],
    },
    BundledSkill {
        name: "excel---xlsx",
        description: "Create, inspect, and edit Excel workbooks with reliable formulas, dates, formatting, and template preservation",
        skill_md: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/excel---xlsx/SKILL.md")),
        meta_json: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/skills/excel---xlsx/_meta.json")),
        extra_files: &[],
    },
];

/// Install bundled skills on first run (embedded in the binary via include_str!)
async fn install_bundled_skills(data_dir: &str) {
    let skills_json_path = format!("{}/skills.json", data_dir);
    let mut skills: Vec<serde_json::Value> = fs::read_to_string(&skills_json_path)
        .await
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut changed = false;

    for bundled in BUNDLED_SKILLS {
        let skill_dir = format!("{}/skills/{}", data_dir, bundled.name);
        let skill_md_path = format!("{}/SKILL.md", skill_dir);

        // Skip if already installed on disk
        if fs::metadata(&skill_md_path).await.is_ok() {
            // Still ensure registered in skills.json
            let already_registered = skills.iter().any(|s| s["name"].as_str() == Some(bundled.name));
            if !already_registered {
                skills.push(json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "name": bundled.name,
                    "description": bundled.description,
                    "source": "bundled",
                    "script": bundled.name,
                    "enabled": true,
                    "installedAt": chrono::Utc::now().to_rfc3339()
                }));
                changed = true;
            }
            continue;
        }

        tracing::info!("[init] Installing bundled skill: {}", bundled.name);

        // Create skill directory
        let _ = fs::create_dir_all(&skill_dir).await;

        // Write SKILL.md and _meta.json
        let _ = fs::write(&skill_md_path, bundled.skill_md).await;
        let _ = fs::write(format!("{}/_meta.json", skill_dir), bundled.meta_json).await;

        // Write extra files (e.g. scripts)
        for (rel_path, content) in bundled.extra_files {
            let full_path = format!("{}/{}", skill_dir, rel_path);
            if let Some(parent) = std::path::Path::new(&full_path).parent() {
                let _ = fs::create_dir_all(parent).await;
            }
            let _ = fs::write(&full_path, content).await;
        }

        // Register in skills.json
        let already_registered = skills.iter().any(|s| s["name"].as_str() == Some(bundled.name));
        if !already_registered {
            skills.push(json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "name": bundled.name,
                "description": bundled.description,
                "source": "bundled",
                "script": bundled.name,
                "enabled": true,
                "installedAt": chrono::Utc::now().to_rfc3339()
            }));
            changed = true;
            tracing::info!("[init] Registered {} skill in skills.json", bundled.name);
        }
    }

    if changed {
        let _ = fs::write(&skills_json_path, serde_json::to_string_pretty(&skills).unwrap_or_default()).await;
    }
}
