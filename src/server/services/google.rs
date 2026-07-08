// ---------------------------------------------------------------------------
// Google quick-connect — one-click Gmail / Calendar / Drive via MCP.
//
// Wraps the `workspace-mcp` server (uvx workspace-mcp): a single stdio MCP
// server covering Gmail, Calendar and Drive that runs its own OAuth flow —
// on the first Google tool call it starts a localhost callback listener and
// opens the Google login page in the user's browser; tokens are cached under
// ~/.google_workspace_mcp/credentials, so login happens once.
//
// TigrimOS's job: find/install `uvx`, write the MCP settings entry (command +
// OAuth client env vars), connect it, and trigger the login so the browser
// opens right away instead of mid-conversation.
// ---------------------------------------------------------------------------

use serde_json::{json, Value};
use tracing::{info, warn};

/// Settings entry name for the Google Workspace MCP server.
pub const GOOGLE_MCP_NAME: &str = "google";

/// URL of the Google Cloud Console credentials page (for the "Get credentials"
/// button) — the user creates an OAuth Client ID (Desktop app) there.
pub const GOOGLE_CONSOLE_URL: &str = "https://console.cloud.google.com/apis/credentials";

/// Locate `uvx` (the uv tool runner workspace-mcp is launched with). Checks
/// PATH first, then the standard install locations uv's installer uses.
pub fn find_uvx() -> Option<String> {
    let exe = if cfg!(windows) { "uvx.exe" } else { "uvx" };

    // PATH lookup via which/where.
    #[cfg(not(target_os = "windows"))]
    let probe = std::process::Command::new("which").arg("uvx").output();
    #[cfg(target_os = "windows")]
    let probe = std::process::Command::new("where").arg("uvx").output();
    if let Ok(out) = probe {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }

    let home = crate::server::services::toolbox::resolve_home();
    let mut candidates = vec![
        format!("{home}/.local/bin/{exe}"),
        format!("{home}/.cargo/bin/{exe}"),
    ];
    if !cfg!(windows) {
        candidates.push(format!("/opt/homebrew/bin/{exe}"));
        candidates.push(format!("/usr/local/bin/{exe}"));
    }
    candidates.into_iter().find(|c| std::path::Path::new(c).exists())
}

/// Install uv (which provides uvx) with the official installer. Returns the
/// uvx path on success.
pub async fn install_uv() -> Result<String, String> {
    info!("[google] Installing uv via the official installer…");
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("/bin/sh");
        c.arg("-c").arg("curl -LsSf https://astral.sh/uv/install.sh | sh");
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = tokio::process::Command::new("powershell");
        c.args(["-ExecutionPolicy", "ByPass", "-NoProfile", "-Command",
            "irm https://astral.sh/uv/install.ps1 | iex"]);
        c
    };
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    let run = tokio::time::timeout(std::time::Duration::from_secs(180), async {
        cmd.spawn().map_err(|e| format!("Failed to run installer: {e}"))?.wait_with_output().await
            .map_err(|e| format!("Installer failed: {e}"))
    })
    .await
    .map_err(|_| "uv installer timed out after 180s".to_string())??;

    if !run.status.success() {
        return Err(format!(
            "uv installer exited with {}: {}",
            run.status,
            crate::util::truncate_utf8(&String::from_utf8_lossy(&run.stderr), 500)
        ));
    }
    find_uvx().ok_or_else(|| "uv installed but uvx was not found on the expected paths".to_string())
}

/// Build the Settings McpTool entry for the Google Workspace server.
pub fn build_google_mcp_entry(
    client_id: &str,
    client_secret: &str,
    email: &str,
    gmail: bool,
    calendar: bool,
    drive: bool,
) -> crate::server::data::McpTool {
    let mut args = vec!["workspace-mcp".to_string(), "--single-user".to_string(), "--tools".to_string()];
    if gmail {
        args.push("gmail".to_string());
    }
    if calendar {
        args.push("calendar".to_string());
    }
    if drive {
        args.push("drive".to_string());
    }

    let mut env = std::collections::HashMap::new();
    env.insert("GOOGLE_OAUTH_CLIENT_ID".to_string(), client_id.trim().to_string());
    if !client_secret.trim().is_empty() {
        env.insert("GOOGLE_OAUTH_CLIENT_SECRET".to_string(), client_secret.trim().to_string());
    }
    // The local OAuth callback (http://localhost:8000/oauth2callback) is plain
    // http — allow it.
    env.insert("OAUTHLIB_INSECURE_TRANSPORT".to_string(), "1".to_string());
    if !email.trim().is_empty() {
        env.insert("USER_GOOGLE_EMAIL".to_string(), email.trim().to_string());
    }

    crate::server::data::McpTool {
        name: GOOGLE_MCP_NAME.to_string(),
        url: String::new(),
        enabled: true,
        tool_type: Some("stdio".to_string()),
        command: Some(find_uvx().unwrap_or_else(|| "uvx".to_string())),
        args: Some(args),
        headers: None,
        env: Some(env),
    }
}

/// Trigger the Google OAuth login on the connected server. The server opens
/// the browser itself; if it returns an auth URL instead, we pass it back so
/// the caller can open it. Returns {ok, message, url?}.
pub async fn start_login(email: &str) -> Value {
    // Give a just-(re)connected server a moment to finish tool discovery.
    for _ in 0..20 {
        if crate::server::services::mcp::is_server_connected(GOOGLE_MCP_NAME).await {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if !crate::server::services::mcp::is_server_connected(GOOGLE_MCP_NAME).await {
        return json!({
            "ok": false,
            "message": "Google MCP server is not connected. Check that uvx works and the OAuth Client ID is set, then press Connect again."
        });
    }

    let tools = crate::server::services::mcp::server_tool_names(GOOGLE_MCP_NAME).await;
    // Prefer the dedicated auth tool; fall back to any auth-ish tool, then to a
    // light real tool — an unauthenticated call makes the server begin OAuth.
    let tool = tools
        .iter()
        .find(|t| t.as_str() == "start_google_auth")
        .or_else(|| tools.iter().find(|t| t.contains("auth")))
        .or_else(|| tools.iter().find(|t| t.contains("list") || t.contains("search")))
        .cloned();
    let Some(tool) = tool else {
        return json!({ "ok": false, "message": "Connected, but the server exposed no tools to trigger login with." });
    };

    let mut args = json!({});
    if !email.trim().is_empty() {
        args["user_google_email"] = json!(email.trim());
    }
    if tool == "start_google_auth" {
        args["service_name"] = json!("Google Workspace");
    }
    info!("[google] Triggering OAuth via tool '{}'", tool);
    let result = crate::server::services::mcp::call_mcp_tool(
        &format!("mcp_{}_{}", GOOGLE_MCP_NAME, tool),
        &args,
    )
    .await;

    // Surface an auth URL if the server returned one instead of opening the
    // browser itself (e.g. headless).
    let blob = result.to_string();
    let url = blob
        .find("https://accounts.google.com/o/oauth2")
        .map(|start| {
            let tail = &blob[start..];
            let end = tail
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '\\' || c == ')')
                .unwrap_or(tail.len());
            tail[..end].to_string()
        });

    if let Some(u) = &url {
        info!("[google] Auth URL returned by server: {}", u);
        json!({ "ok": true, "url": u, "message": "Open the login URL in your browser to authorize Google access." })
    } else if result.to_string().to_lowercase().contains("error") && result["ok"] == json!(false) {
        warn!("[google] Login trigger returned an error: {}", blob);
        json!({ "ok": false, "message": crate::util::truncate_utf8(&blob, 400) })
    } else {
        json!({ "ok": true, "message": "Login started — a Google sign-in page should have opened in your browser. After approving, Google access is ready." })
    }
}
