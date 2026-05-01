mod security;
mod server;
mod ui;
mod vm;

use std::sync::Arc;
use vm::manager::VmManager;

fn main() {
    tracing_subscriber::fmt::init();

    let runtime = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let handle = runtime.handle().clone();

    let vm_manager = Arc::new(VmManager::new());

    // Start the Axum server in background
    let sandbox_dir = std::env::var("SANDBOX_DIR").unwrap_or_else(|_| "sandbox".to_string());
    let _ = std::fs::create_dir_all(&sandbox_dir);
    let access_token = std::env::var("ACCESS_TOKEN").unwrap_or_default();
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
            .with_inner_size([1200.0, 800.0]),
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
