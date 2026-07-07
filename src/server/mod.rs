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

/// Constant-time comparison, exposed for the auth verify route.
pub(crate) fn ct_eq_pub(a: &str, b: &str) -> bool {
    ct_eq(a, b)
}

/// Constant-time string comparison — token checks must not leak length or
/// prefix-match timing to a remote attacker.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() != b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

// Global brute-force limiter: sliding 60s window of failed auth attempts.
// Global (not per-IP) because behind a Cloudflare tunnel every request
// arrives from loopback — per-IP limits would be useless there.
static AUTH_FAILS: std::sync::OnceLock<std::sync::Mutex<Vec<std::time::Instant>>> =
    std::sync::OnceLock::new();

pub(crate) fn auth_rate_limited() -> bool {
    let m = AUTH_FAILS.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let mut v = m.lock().unwrap();
    let now = std::time::Instant::now();
    v.retain(|t| now.duration_since(*t) < std::time::Duration::from_secs(60));
    v.len() >= 10
}

pub(crate) fn record_auth_fail() {
    let m = AUTH_FAILS.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let mut v = m.lock().unwrap();
    if v.len() < 1000 {
        v.push(std::time::Instant::now());
    }
}

/// Browser cross-origin guard (anti drive-by / DNS-rebinding). Requests with
/// an Origin header must originate either from loopback (the local web UI /
/// desktop app) or from the same authority the request was addressed to
/// (tunnel domain, LAN IP). Non-browser clients send no Origin and pass.
fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return true;
    };
    if origin.eq_ignore_ascii_case("null") {
        return false;
    }
    let authority = origin.split("://").nth(1).unwrap_or(origin);
    let host_only = authority.split(':').next().unwrap_or(authority);
    if matches!(host_only, "localhost" | "127.0.0.1" | "[::1]") {
        return true;
    }
    if let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) {
        if authority.eq_ignore_ascii_case(host) {
            return true;
        }
    }
    false
}

/// API prefixes a REMOTE-token session may not touch: these give code
/// execution on the bare host or reconfigure security. The OWNER token
/// (ACCESS_TOKEN env / headless setup token) retains full access, and the
/// user can opt remote sessions back in with settings `remoteFullAccess: true`.
const REMOTE_BLOCKED_PREFIXES: &[&str] = &["/api/terminal", "/api/local-files", "/api/plugins", "/api/vpn"];

async fn auth_middleware(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let method = req.method().clone();

    // Browser cross-origin guard — applies to ALL requests, even when no
    // auth is configured: blocks drive-by websites POSTing to localhost.
    if !origin_allowed(req.headers()) {
        return (StatusCode::FORBIDDEN, Json(json!({"error": "Cross-origin request rejected"}))).into_response();
    }

    // Skip auth for verify endpoint (it does its own rate-limited check)
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

    // If no access_token AND no remote_token configured, allow all (local-only
    // use — the server binds to 127.0.0.1 in this configuration).
    if state.access_token.is_empty() && remote_token.is_none() {
        return next.run(req).await;
    }

    // Brute-force guard: once token guessing is detected, reject before
    // comparing anything.
    if auth_rate_limited() {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "Too many failed authentication attempts. Try again in a minute."})),
        ).into_response();
    }

    // Query-string tokens (?token=) are accepted ONLY for /api/files — they
    // exist for browser download/preview links. Everywhere else tokens must
    // travel in the Authorization header so they don't leak into access logs.
    let token = extract_bearer(req.headers()).or_else(|| {
        if path.starts_with("/api/files") { extract_query_token(&uri) } else { None }
    });

    if let Some(ref t) = token {
        // OWNER token — full access
        if !state.access_token.is_empty() && ct_eq(t, &state.access_token) {
            return next.run(req).await;
        }
        // REMOTE token — chat/agents/files, but not host-level control
        if let Some(ref rt) = remote_token {
            if ct_eq(t, rt) {
                let remote_full = settings.extra.get("remoteFullAccess")
                    .and_then(|v| v.as_bool()).unwrap_or(false);
                if !remote_full {
                    if REMOTE_BLOCKED_PREFIXES.iter().any(|p| path.starts_with(p)) {
                        return (StatusCode::FORBIDDEN, Json(json!({
                            "error": "This endpoint is not available with a remote access token. \
                                      Use the owner ACCESS_TOKEN, or set remoteFullAccess=true in settings."
                        }))).into_response();
                    }
                    // Settings are read-only for remote tokens — except the
                    // soul/identity persona files, which are prompt-level
                    // configuration (no host control) and must be editable
                    // from a connected desktop.
                    if path.starts_with("/api/settings")
                        && !path.starts_with("/api/settings/soul-identity")
                        && method != axum::http::Method::GET
                    {
                        return (StatusCode::FORBIDDEN, Json(json!({
                            "error": "Settings are read-only for remote access tokens."
                        }))).into_response();
                    }
                }
                return next.run(req).await;
            }
        }
        // File token — /api/files routes only
        if path.starts_with("/api/files") && is_valid_file_token(t).await {
            return next.run(req).await;
        }
    }

    record_auth_fail();
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
        format!("{}/plugins", data_dir),
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
        ("plugins.json", "[]"),
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

    // Seed the default agent-loop profile (mirrors current loop behavior)
    services::agent_loop::ensure_default_profile();

    // Initialize MCP server connections from settings
    services::mcp::init_mcp_servers().await;

    // Start the cron scheduler for enabled scheduled tasks
    services::scheduler::init_scheduler().await;

    // Messaging bots: loads per-chat state and spawns the Telegram long-poll
    // supervisor (self-idles until telegramEnabled + token are set).
    services::messaging::init();

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

    // LINE webhook (no bearer auth — LINE can't send our token; the handler
    // verifies the X-Line-Signature HMAC over the raw body instead).
    let line_webhook = Router::new().route(
        "/line/webhook",
        axum::routing::post(routes::messaging::line_webhook),
    );

    // Web UI (no auth needed — auth happens client-side via API calls)
    let web_ui = routes::web_ui::router();

    // API routes with auth middleware
    let api_with_auth = Router::new()
        .nest("/api", api_routes)
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    let owner_token_set = !state.access_token.is_empty();

    // Combine: unauthenticated routes + authenticated API
    let app = Router::new()
        .merge(auth_verify)
        .merge(line_webhook)
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

    // Network exposure policy. Default is token-based: only listen on all
    // interfaces when some form of auth is configured (owner token, or remote
    // access enabled WITH a token). Otherwise bind loopback — an unauthenticated
    // TigrimOS must never be reachable from the network ("/api/terminal/exec" is
    // RCE). Cloudflare tunnels connect via loopback, so tunnels keep working.
    //
    // VPN-exclusive override: when VPN is enabled it takes precedence — the
    // server binds loopback + the tailnet (100.x) IP ONLY, so the ordinary
    // LAN/public IP can't reach it even though a token is set. This is what makes
    // "run over VPN" actually private rather than just adding a second path.
    let (settings_remote_ready, settings_vpn_enabled) = {
        let settings = get_settings().await;
        let token_set = settings.remote_token.as_deref().map(|t| !t.is_empty()).unwrap_or(false);
        let remote_ready = settings.remote_enabled == Some(true) && token_set;
        (remote_ready, settings.vpn_enabled == Some(true))
    };
    let env_bind_all = std::env::var("TIGRIMOS_BIND_ALL").ok().as_deref() == Some("1");

    // Resolve the concrete addresses to listen on.
    let bind_addrs: Vec<String> = if settings_vpn_enabled {
        // VPN on => exclusive. Loopback (host + Cloudflare tunnel) + tailnet IP.
        let mut addrs = vec!["127.0.0.1".to_string()];
        match crate::server::services::vpn::tailnet_ip().await {
            Some(ip) => {
                tracing::info!(
                    "TigrimOS server in VPN-exclusive mode: listening on 127.0.0.1 + tailnet IP {} only \
                     (ordinary LAN/public IP is NOT reachable)",
                    ip
                );
                addrs.push(ip);
            }
            None => {
                tracing::warn!(
                    "VPN enabled but no tailnet IP available (is Tailscale up?) — binding 127.0.0.1 only. \
                     Start Tailscale, then restart TigrimOS."
                );
            }
        }
        addrs
    } else if owner_token_set || settings_remote_ready || env_bind_all {
        tracing::info!("TigrimOS server listening on ALL interfaces (token auth configured)");
        vec!["0.0.0.0".to_string()]
    } else {
        tracing::info!(
            "TigrimOS server listening on 127.0.0.1 only (no auth configured). \
             Enable Remote Access with a token in Settings (then restart) for LAN/mobile access."
        );
        vec!["127.0.0.1".to_string()]
    };

    // Bind every resolved address; skip (with a warning) any that fail rather
    // than aborting the whole server for one bad interface.
    let mut listeners = Vec::with_capacity(bind_addrs.len());
    for addr in &bind_addrs {
        match tokio::net::TcpListener::bind(format!("{}:{}", addr, port)).await {
            Ok(l) => listeners.push(l),
            Err(e) => tracing::error!("Failed to bind {}:{}: {}", addr, port, e),
        }
    }
    if listeners.is_empty() {
        tracing::error!("No listeners bound on port {} — server not started", port);
        return;
    }

    tracing::info!("TigrimOS server running on http://localhost:{}", port);
    tracing::info!("Sandbox directory: {}", sandbox_dir);

    // Optional Tailscale VPN: detect/refresh the tailnet address if enabled so
    // the reachable URL is ready to show in Settings.
    crate::server::services::vpn::init_vpn(port).await;

    // Serve the same app on every bound listener.
    let mut handles = Vec::with_capacity(listeners.len());
    for listener in listeners {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("Server error: {}", e);
            }
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
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
