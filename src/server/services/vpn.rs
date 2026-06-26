use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::process::Command;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Tailscale VPN — a private alternative to the public Cloudflare tunnel for
// reaching this host remotely. Mirrors `tunnel.rs`: detect the binary, read
// status, optionally bring the tailnet up, and surface a reachable URL that can
// be pasted into a Remote Instance. Mutually exclusive with the Cloudflare
// tunnel (the user picks one "remote connection method").
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct VpnState {
    running: bool,
    ip: Option<String>,
    hostname: Option<String>,
    /// Set while `tailscale up` is waiting for the user to authenticate.
    auth_url: Option<String>,
    error: Option<String>,
}

static VPN_STATE: OnceLock<TokioMutex<VpnState>> = OnceLock::new();

fn vpn_state() -> &'static TokioMutex<VpnState> {
    VPN_STATE.get_or_init(|| TokioMutex::new(VpnState::default()))
}

/// Find the tailscale CLI binary.
async fn find_tailscale() -> Option<String> {
    for bin in &[
        "tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ] {
        if Command::new(bin)
            .arg("version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(bin.to_string());
        }
    }
    None
}

/// Build the URL a remote peer would use to reach this host over the tailnet.
fn vpn_url(ip: &str, port: u16) -> String {
    format!("http://{}:{}", ip, port)
}

/// Query `tailscale status --json` and extract the tailnet IP + MagicDNS name.
/// Returns (running, ip, hostname).
async fn tailscale_status(bin: &str) -> (bool, Option<String>, Option<String>) {
    let output = Command::new(bin)
        .args(["status", "--json"])
        .output()
        .await;

    let json: Value = match output {
        Ok(o) if o.status.success() => {
            serde_json::from_slice(&o.stdout).unwrap_or(Value::Null)
        }
        _ => return (false, None, None),
    };

    let running = json["BackendState"].as_str() == Some("Running");
    let self_node = &json["Self"];
    let ip = self_node["TailscaleIPs"]
        .as_array()
        .and_then(|ips| ips.iter().find_map(|v| v.as_str()))
        .map(|s| s.to_string());
    let hostname = self_node["DNSName"]
        .as_str()
        .map(|s| s.trim_end_matches('.').to_string())
        .filter(|s| !s.is_empty());

    (running, ip, hostname)
}

/// Resolve the current tailnet IP by querying Tailscale directly. Used at
/// startup to bind the server to the VPN interface only (VPN-exclusive mode),
/// before the cached VPN state has been populated by `init_vpn`. Returns `None`
/// if Tailscale isn't installed or the node isn't connected.
pub async fn tailnet_ip() -> Option<String> {
    let bin = find_tailscale().await?;
    let (running, ip, _) = tailscale_status(&bin).await;
    if running {
        ip
    } else {
        None
    }
}

/// Get current VPN status as JSON.
pub async fn get_vpn_state() -> Value {
    let state = vpn_state().lock().await;
    let url = state
        .ip
        .as_ref()
        .map(|ip| vpn_url(ip, current_port()));
    json!({
        "running": state.running,
        "ip": state.ip,
        "hostname": state.hostname,
        "url": url,
        "authUrl": state.auth_url,
        "error": state.error,
    })
}

/// Detect Tailscale and refresh cached state. Returns the status JSON.
async fn refresh_state(port: u16) -> Value {
    let bin = match find_tailscale().await {
        Some(b) => b,
        None => {
            let mut state = vpn_state().lock().await;
            state.running = false;
            state.error = Some(
                "tailscale not found. Install from https://tailscale.com/download".to_string(),
            );
            return get_vpn_state().await;
        }
    };

    let (running, ip, hostname) = tailscale_status(&bin).await;
    {
        let mut state = vpn_state().lock().await;
        state.running = running;
        state.ip = ip.clone();
        state.hostname = hostname;
        state.error = if running {
            None
        } else {
            Some("Tailscale is installed but not connected (run Start / tailscale up)".to_string())
        };
        if running {
            state.auth_url = None;
        }
    }

    if let Some(ip) = ip {
        let _ = save_vpn_to_settings(&vpn_url(&ip, port)).await;
    }
    get_vpn_state().await
}

/// Start / connect the Tailscale VPN. If the node needs authentication,
/// captures the login URL (like cloudflared's tunnel URL) and returns it so the
/// UI can prompt the user to authenticate.
pub async fn start_vpn(port: u16) -> Value {
    let bin = match find_tailscale().await {
        Some(b) => b,
        None => {
            return json!({
                "ok": false,
                "error": "tailscale not found. Install from https://tailscale.com/download"
            })
        }
    };

    // Already connected? Just refresh and report.
    let (running, _, _) = tailscale_status(&bin).await;
    if running {
        let state = refresh_state(port).await;
        return json!({ "ok": true, "url": state["url"], "running": true });
    }

    info!("[VPN] Bringing Tailscale up...");
    let mut child = match Command::new(&bin)
        .arg("up")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": format!("Failed to start tailscale: {e}") }),
    };

    // `tailscale up` prints a login URL to stderr when authentication is needed.
    let stderr = child.stderr.take();
    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<Option<String>>();
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr);
            let url_re = regex::Regex::new(r"https://login\.tailscale\.com/[^\s]+").unwrap();
            let mut url_tx = Some(url_tx);
            let mut found = false;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if !found {
                            if let Some(m) = url_re.find(&line) {
                                found = true;
                                if let Some(tx) = url_tx.take() {
                                    let _ = tx.send(Some(m.as_str().to_string()));
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            if let Some(tx) = url_tx.take() {
                let _ = tx.send(None);
            }
        });
    } else {
        let _ = url_tx.send(None);
    }

    // Wait up to 15s either for the process to finish (already authenticated) or
    // for a login URL to appear (needs auth).
    let auth_url = match tokio::time::timeout(std::time::Duration::from_secs(15), url_rx).await {
        Ok(Ok(Some(u))) => Some(u),
        _ => None,
    };

    if let Some(url) = auth_url {
        // Needs authentication — leave `tailscale up` running and report the URL.
        let mut state = vpn_state().lock().await;
        state.running = false;
        state.auth_url = Some(url.clone());
        state.error = Some("Authentication required — open the login URL".to_string());
        info!("[VPN] Authentication required: {}", url);
        return json!({ "ok": true, "authUrl": url, "running": false,
            "message": "Open the login URL to authenticate, then refresh status" });
    }

    // Either authenticated already or `up` returned — wait briefly then refresh.
    let _ = child.wait().await;
    let state = refresh_state(port).await;
    if state["running"].as_bool().unwrap_or(false) {
        json!({ "ok": true, "url": state["url"], "running": true })
    } else {
        json!({ "ok": false, "error": state["error"], "running": false })
    }
}

/// Disconnect the Tailscale VPN (`tailscale down`).
pub async fn stop_vpn() -> Value {
    let bin = match find_tailscale().await {
        Some(b) => b,
        None => return json!({ "ok": false, "error": "tailscale not found" }),
    };
    let _ = Command::new(&bin).arg("down").output().await;
    let mut state = vpn_state().lock().await;
    state.running = false;
    state.ip = None;
    state.hostname = None;
    state.auth_url = None;
    state.error = None;
    info!("[VPN] Tailscale down");
    let _ = save_vpn_running(false).await;
    json!({ "ok": true })
}

/// Auto-detect Tailscale on startup if enabled in settings.
pub async fn init_vpn(port: u16) {
    set_current_port(port);
    let settings: Value = match tokio::fs::read_to_string(
        crate::server::data::data_dir().join("settings.json"),
    )
    .await
    {
        Ok(s) => serde_json::from_str(&s).unwrap_or(json!({})),
        Err(_) => return,
    };

    if settings["vpnEnabled"].as_bool().unwrap_or(false) {
        info!("[VPN] vpnEnabled — detecting Tailscale status");
        let state = refresh_state(port).await;
        if !state["running"].as_bool().unwrap_or(false) {
            warn!(
                "[VPN] Not connected: {}",
                state["error"].as_str().unwrap_or("unknown")
            );
        } else {
            info!("[VPN] Reachable at {}", state["url"].as_str().unwrap_or("?"));
        }
    }
}

// ---------------------------------------------------------------------------
// Port memory — so get_vpn_state() can build the URL without a port argument.
// ---------------------------------------------------------------------------

static VPN_PORT: OnceLock<std::sync::Mutex<u16>> = OnceLock::new();

fn set_current_port(port: u16) {
    *VPN_PORT.get_or_init(|| std::sync::Mutex::new(3001)).lock().unwrap() = port;
}

fn current_port() -> u16 {
    *VPN_PORT.get_or_init(|| std::sync::Mutex::new(3001)).lock().unwrap()
}

// ---------------------------------------------------------------------------
// Settings persistence (mirror of tunnel.rs save_tunnel_to_settings)
// ---------------------------------------------------------------------------

async fn save_vpn_to_settings(url: &str) {
    if let Ok(content) =
        tokio::fs::read_to_string(crate::server::data::data_dir().join("settings.json")).await
    {
        if let Ok(mut settings) = serde_json::from_str::<Value>(&content) {
            settings["vpnUrl"] = json!(url);
            settings["vpnRunning"] = json!(true);
            let _ = tokio::fs::write(
                crate::server::data::data_dir().join("settings.json"),
                serde_json::to_string_pretty(&settings).unwrap_or_default(),
            )
            .await;
        }
    }
}

async fn save_vpn_running(running: bool) {
    if let Ok(content) =
        tokio::fs::read_to_string(crate::server::data::data_dir().join("settings.json")).await
    {
        if let Ok(mut settings) = serde_json::from_str::<Value>(&content) {
            settings["vpnRunning"] = json!(running);
            let _ = tokio::fs::write(
                crate::server::data::data_dir().join("settings.json"),
                serde_json::to_string_pretty(&settings).unwrap_or_default(),
            )
            .await;
        }
    }
}
