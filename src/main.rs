// Release builds run as a true Windows GUI app, so launching AndrewOS from a
// shortcut, the Start menu or Explorer opens just the window — no console
// flashing behind it. Debug builds keep the console for development, and
// `--headless` re-attaches to the calling terminal (see attach_parent_console)
// so server logs still appear when it is run from a shell.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod security;
mod server;
mod ui;
mod util;
mod vm;

use std::sync::Arc;
use vm::manager::VmManager;

/// Attach to the console of the process that launched us, so a GUI-subsystem
/// binary can still write logs when run from a terminal.
///
/// Declared inline rather than pulling in a Windows API crate for one call.
#[cfg(windows)]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    // Failure just means there was no parent console — nothing to recover from.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// In headless mode on an interactive terminal, offer to install the Playwright
/// browser and enable browser control. Skipped entirely on non-interactive
/// startups (systemd / Docker / cron) so it never blocks waiting for input.
fn maybe_prompt_browser_install(runtime: &tokio::runtime::Runtime) {
    use std::io::{IsTerminal, Write};

    if !std::io::stdin().is_terminal() {
        return; // no TTY — don't block automated deploys
    }

    // Already enabled? Nothing to do.
    let settings = runtime.block_on(server::data::get_settings());
    if settings.browser_control_enabled == Some(true) {
        return;
    }

    println!();
    println!("Browser control lets the agent drive a real web browser");
    println!("(search Google, click, fill forms, screenshot) — free, no search API.");
    print!("Enable it now? Downloads the Playwright browser (~280 MB) [y/N]: ");
    std::io::stdout().flush().ok();

    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return;
    }
    if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Skipped. Enable later in Settings → Security → Browser Control.");
        println!(
            "(Prefer Obscura? Install the `obscura` binary and pick it there — no npx needed.)"
        );
        println!();
        return;
    }

    println!("Installing Playwright browser (chrome-for-testing)…");
    match std::process::Command::new("npx")
        .args([
            "@playwright/mcp@latest",
            "install-browser",
            "chrome-for-testing",
        ])
        .status()
    {
        Ok(s) if s.success() => {
            let mut settings = runtime.block_on(server::data::get_settings());
            settings.browser_control_enabled = Some(true);
            if settings.browser_engine.is_none() {
                settings.browser_engine = Some("chromium".to_string());
            }
            runtime.block_on(server::data::save_settings(&settings));
            println!("✓ Browser control enabled (chromium, headless).");
            println!("  On Linux you may also need: npx playwright install-deps chromium  (sudo)");
        }
        Ok(_) => {
            println!("✗ Browser install failed. Ensure Node.js/npx is installed, then enable");
            println!("  Browser Control later in Settings.");
        }
        Err(e) => {
            println!("✗ Could not run npx ({e}). Install Node.js first, then enable Browser");
            println!("  Control later in Settings.");
        }
    }
    println!();
}

fn main() {
    // Load .env from data directory (same location as settings.json)
    let env_path = server::data::data_dir().join(".env");
    let _ = dotenvy::from_path(&env_path);
    // Also try .env in current working directory
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt::init();

    // Ensure PATH includes common tool locations (critical for .app bundles)
    let current_path = std::env::var("PATH").unwrap_or_default();
    if !current_path.contains("/opt/homebrew/bin") || !current_path.contains("/usr/local/bin") {
        std::env::set_var(
            "PATH",
            format!(
                "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}",
                current_path
            ),
        );
    }

    let headless = std::env::args().any(|a| a == "--headless");

    // A GUI-subsystem binary has no console of its own, so headless runs would
    // print their logs into the void. Re-attach to the terminal that launched
    // us. No-op when there isn't one (a service, Docker, a double-click).
    #[cfg(windows)]
    if headless {
        attach_parent_console();
    }

    let runtime = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let handle = runtime.handle().clone();

    // Resolve sandbox dir — use absolute path so .app bundles work (cwd may be /)
    let sandbox_dir = std::env::var("SANDBOX_DIR").unwrap_or_else(|_| {
        let raw = "sandbox".to_string();
        if std::path::Path::new(&raw).is_absolute() {
            raw
        } else {
            let abs = server::data::data_dir()
                .parent()
                .unwrap_or(&std::path::PathBuf::from("."))
                .join(&raw);
            abs.to_string_lossy().to_string()
        }
    });
    let _ = std::fs::create_dir_all(&sandbox_dir);
    let access_token = std::env::var("ACCESS_TOKEN").unwrap_or_default();

    if headless {
        // Require access token for security — prompt if not set via env
        let access_token = if access_token.is_empty() {
            println!("===========================================");
            println!("  AndrewOS Headless Mode — Security Setup");
            println!("===========================================");
            println!();
            loop {
                print!("Enter access token: ");
                use std::io::Write;
                std::io::stdout().flush().ok();
                let mut input = String::new();
                match std::io::stdin().read_line(&mut input) {
                    // EOF — there is no terminal, so no token will ever arrive.
                    // Re-prompting here spins forever and fills the log, which
                    // is what a Docker run, systemd unit or CI job without a
                    // TTY hits. Fail with something actionable instead.
                    Ok(0) => {
                        eprintln!();
                        eprintln!("No terminal is attached, so no access token can be entered.");
                        eprintln!("Set it in the environment instead:");
                        eprintln!("  ACCESS_TOKEN=<your-token> andrewos --headless");
                        std::process::exit(2);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!();
                        eprintln!("Could not read the access token: {e}");
                        eprintln!("Set ACCESS_TOKEN in the environment and start again.");
                        std::process::exit(2);
                    }
                }
                let token = input.trim().to_string();
                if !token.is_empty() {
                    println!();
                    println!("Token set. Use this to connect from your Mac or browser.");
                    println!(
                        "  Web UI:  http://<server-ip>:{}/web/",
                        std::env::var("PORT").unwrap_or_else(|_| "3001".to_string())
                    );
                    println!("  Token:   {}", token);
                    println!();
                    break token;
                }
                println!("Token cannot be empty.");
            }
        } else {
            println!("Access token loaded from ACCESS_TOKEN env var.");
            access_token
        };

        // Offer browser control setup (interactive terminals only).
        maybe_prompt_browser_install(&runtime);

        tracing::info!("Running in headless mode (HTTP server only)");
        server::services::skill_synthesizer::start_cron(handle.clone());
        runtime.block_on(server::start_server(sandbox_dir, access_token));
        return;
    }

    let vm_manager = Arc::new(VmManager::new());

    // Start the Axum server in background
    handle.spawn(server::start_server(sandbox_dir, access_token));

    // Start skill auto-update cron job
    server::services::skill_synthesizer::start_cron(handle.clone());

    // Load app icon
    let icon = {
        let icon_bytes = include_bytes!("../assets/icon.png");
        let img = image::load_from_memory(icon_bytes).expect("Failed to load icon");
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        egui::IconData {
            rgba: rgba.into_raw(),
            width: w,
            height: h,
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AndrewOS")
            .with_icon(std::sync::Arc::new(icon))
            .with_min_inner_size([1100.0, 700.0])
            .with_inner_size([1200.0, 800.0])
            // Allow the "Transparent" theme to show the desktop through translucent surfaces.
            .with_transparent(true),
        ..Default::default()
    };

    let vm_clone = Arc::clone(&vm_manager);
    let handle_clone = handle.clone();

    eframe::run_native(
        "AndrewOS",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(ui::app::AndrewOSApp::new(
                cc,
                vm_clone,
                handle_clone,
            )))
        }),
    )
    .expect("Failed to start eframe");
}
