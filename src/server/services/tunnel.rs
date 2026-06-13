use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::process::Command;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

#[derive(Debug, Clone)]
struct TunnelState {
    running: bool,
    url: Option<String>,
    pid: Option<u32>,
    error: Option<String>,
}

static TUNNEL_STATE: OnceLock<TokioMutex<TunnelState>> = OnceLock::new();

fn tunnel_state() -> &'static TokioMutex<TunnelState> {
    TUNNEL_STATE.get_or_init(|| {
        TokioMutex::new(TunnelState {
            running: false,
            url: None,
            pid: None,
            error: None,
        })
    })
}

/// Find the cloudflared binary
async fn find_cloudflared() -> Option<String> {
    for bin in &["cloudflared", "/usr/local/bin/cloudflared"] {
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

/// Get current tunnel status
pub async fn get_tunnel_state() -> Value {
    let state = tunnel_state().lock().await;
    json!({
        "running": state.running,
        "url": state.url,
        "error": state.error,
    })
}

/// Start a Cloudflare tunnel for the given port
pub async fn start_tunnel(port: u16) -> Value {
    {
        let state = tunnel_state().lock().await;
        if state.running {
            return json!({
                "ok": true,
                "url": state.url,
                "message": "Tunnel already running"
            });
        }
    }

    let bin = match find_cloudflared().await {
        Some(b) => b,
        None => {
            // Try to install on Linux
            if cfg!(target_os = "linux") {
                info!("[Tunnel] cloudflared not found, attempting install...");
                let install_result = install_cloudflared().await;
                if !install_result {
                    return json!({ "ok": false, "error": "cloudflared not found and auto-install failed. Install from https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/" });
                }
                match find_cloudflared().await {
                    Some(b) => b,
                    None => return json!({ "ok": false, "error": "cloudflared installed but still not found in PATH" }),
                }
            } else {
                return json!({ "ok": false, "error": "cloudflared not found. Install from https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/" });
            }
        }
    };

    info!("[Tunnel] Starting cloudflared tunnel on port {}", port);

    let mut child = match Command::new(&bin)
        .args(["tunnel", "--url", &format!("http://localhost:{}", port)])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": format!("Failed to start cloudflared: {e}") }),
    };

    let pid = child.id();

    // Read stderr to find the tunnel URL (cloudflared outputs URL to stderr)
    let stderr = child.stderr.take();
    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<Option<String>>();

    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr);
            let mut found_url = false;
            let url_re = regex::Regex::new(r"https://[a-z0-9-]+\.trycloudflare\.com").unwrap();

            let mut url_tx = Some(url_tx);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if !found_url {
                            if let Some(m) = url_re.find(&line) {
                                found_url = true;
                                if let Some(tx) = url_tx.take() {
                                    let _ = tx.send(Some(m.as_str().to_string()));
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            if !found_url {
                if let Some(tx) = url_tx.take() {
                    let _ = tx.send(None);
                }
            }
        });
    } else {
        let _ = url_tx.send(None);
    }

    // Wait up to 15 seconds for URL
    let url = match tokio::time::timeout(std::time::Duration::from_secs(15), url_rx).await {
        Ok(Ok(Some(url))) => Some(url),
        _ => None,
    };

    let mut state = tunnel_state().lock().await;
    if let Some(ref u) = url {
        state.running = true;
        state.url = Some(u.clone());
        state.pid = pid;
        state.error = None;
        info!("[Tunnel] Started at {}", u);

        // Save to settings
        let _ = save_tunnel_to_settings(u).await;

        json!({ "ok": true, "url": u })
    } else {
        // Kill the process since we couldn't get a URL
        if let Some(pid) = pid {
            let _ = Command::new("kill").arg(pid.to_string()).output().await;
        }
        state.error = Some("Failed to get tunnel URL within 15 seconds".to_string());
        json!({ "ok": false, "error": "Failed to get tunnel URL within 15 seconds" })
    }
}

/// Stop the running tunnel
pub async fn stop_tunnel() -> Value {
    let mut state = tunnel_state().lock().await;
    if let Some(pid) = state.pid {
        let _ = Command::new("kill").arg(pid.to_string()).output().await;
        info!("[Tunnel] Stopped (pid {})", pid);
    }
    state.running = false;
    state.url = None;
    state.pid = None;
    state.error = None;
    json!({ "ok": true })
}

/// Auto-start tunnel if enabled in settings
pub async fn init_tunnel(port: u16) {
    let settings: Value = match tokio::fs::read_to_string(crate::server::data::data_dir().join("settings.json")).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or(json!({})),
        Err(_) => return,
    };

    if settings["tunnelEnabled"].as_bool().unwrap_or(false) {
        info!("[Tunnel] Auto-starting tunnel (enabled in settings)");
        let result = start_tunnel(port).await;
        if !result["ok"].as_bool().unwrap_or(false) {
            warn!(
                "[Tunnel] Auto-start failed: {}",
                result["error"].as_str().unwrap_or("unknown")
            );
        }
    }
}

/// Install cloudflared on Linux
async fn install_cloudflared() -> bool {
    // Try dpkg first
    let arch = std::env::consts::ARCH;
    let deb_arch = match arch {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        _ => return false,
    };

    let url = format!(
        "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-{}.deb",
        deb_arch
    );
    let deb_path = "/tmp/cloudflared.deb";

    // Download
    let dl = Command::new("curl")
        .args(["-sL", "-o", deb_path, &url])
        .output()
        .await;

    if dl.map(|o| o.status.success()).unwrap_or(false) {
        // Try dpkg install
        let install = Command::new("sudo")
            .args(["dpkg", "-i", deb_path])
            .output()
            .await;
        if install.map(|o| o.status.success()).unwrap_or(false) {
            return true;
        }
    }

    // Fallback: download binary directly
    let bin_url = format!(
        "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-{}",
        deb_arch
    );
    let bin_result = Command::new("curl")
        .args(["-sL", "-o", "/usr/local/bin/cloudflared", &bin_url])
        .output()
        .await;

    if bin_result.map(|o| o.status.success()).unwrap_or(false) {
        let _ = Command::new("chmod")
            .args(["+x", "/usr/local/bin/cloudflared"])
            .output()
            .await;
        return true;
    }

    false
}

async fn save_tunnel_to_settings(url: &str) {
    if let Ok(content) = tokio::fs::read_to_string(crate::server::data::data_dir().join("settings.json")).await {
        if let Ok(mut settings) = serde_json::from_str::<Value>(&content) {
            settings["tunnelUrl"] = json!(url);
            settings["tunnelRunning"] = json!(true);
            let _ = tokio::fs::write(
                crate::server::data::data_dir().join("settings.json"),
                serde_json::to_string_pretty(&settings).unwrap_or_default(),
            )
            .await;
        }
    }
}
