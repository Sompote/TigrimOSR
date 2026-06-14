mod security;
mod server;
mod ui;
mod util;
mod vm;

use std::sync::Arc;
use vm::manager::VmManager;

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
            format!("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}", current_path),
        );
    }

    let headless = std::env::args().any(|a| a == "--headless");

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
            println!("  TigrimOS Headless Mode — Security Setup");
            println!("===========================================");
            println!();
            loop {
                print!("Enter access token: ");
                use std::io::Write;
                std::io::stdout().flush().unwrap();
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).unwrap();
                let token = input.trim().to_string();
                if !token.is_empty() {
                    println!();
                    println!("Token set. Use this to connect from your Mac or browser.");
                    println!("  Web UI:  http://<server-ip>:{}/web/", std::env::var("PORT").unwrap_or_else(|_| "3001".to_string()));
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
            .with_title("TigrimOS")
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
        "TigrimOS",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(ui::app::TigrimOSApp::new(
                cc,
                vm_clone,
                handle_clone,
            )))
        }),
    )
    .expect("Failed to start eframe");
}
