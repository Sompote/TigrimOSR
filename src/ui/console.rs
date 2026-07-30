use std::sync::Arc;

use eframe::egui;

use crate::vm::VmManager;

pub fn console_view(
    ui: &mut egui::Ui,
    console_output: &str,
    vm_manager: &Arc<VmManager>,
    runtime: &tokio::runtime::Handle,
) {
    ui.horizontal(|ui| {
        ui.heading("VM Console");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Clear").clicked() {
                let vm = vm_manager.clone();
                runtime.spawn(async move {
                    vm.clear_console().await;
                });
            }
        });
    });

    ui.separator();

    let text = if console_output.is_empty() {
        "No output yet. Start the VM to see logs."
    } else {
        console_output
    };

    let frame = egui::Frame::new()
        .fill(egui::Color32::BLACK)
        .inner_margin(egui::Margin::same(8));

    frame.show(ui, |ui| {
        let available = ui.available_size();
        egui::ScrollArea::vertical()
            .max_height(available.y)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .monospace()
                            .color(egui::Color32::from_rgb(34, 197, 94))
                            .size(13.0),
                    )
                    .selectable(true)
                    .wrap(),
                );
            });
    });
}
