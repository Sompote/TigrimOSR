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
    let sandbox_dir = std::env::var("SANDBOX_DIR").unwrap_or_else(|_| ".".to_string());
    let access_token = std::env::var("ACCESS_TOKEN").unwrap_or_default();
    handle.spawn(server::start_server(sandbox_dir, access_token));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TigrimOS")
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
            Ok(Box::new(ui::app::TigrimOSApp::new(
                cc,
                vm_clone,
                handle_clone,
            )))
        }),
    )
    .expect("Failed to start eframe");
}
