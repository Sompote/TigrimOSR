use std::sync::Arc;

use eframe::egui;

use crate::vm::{SharedFolderEntry, VmManager};

pub fn shared_folders_view(
    ui: &mut egui::Ui,
    shared_folders: &[SharedFolderEntry],
    vm_manager: &Arc<VmManager>,
    runtime: &tokio::runtime::Handle,
) {
    // Header
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.heading("Shared Folders");
            ui.label(egui::RichText::new("Only these folders are accessible inside the sandbox.").size(12.0).color(egui::Color32::GRAY));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(egui::Button::new(egui::RichText::new("+ Add Folder").color(egui::Color32::WHITE)).fill(egui::Color32::from_rgb(59, 130, 246))).clicked() {
                if let Some(path) = rfd::FileDialog::new().set_title("Select folder to share").pick_folder() {
                    let vm = vm_manager.clone();
                    runtime.spawn(async move { vm.add_shared_folder(path, true).await; });
                }
            }
        });
    });

    ui.separator();

    if shared_folders.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.label(egui::RichText::new("\u{1F512}").size(48.0).color(egui::Color32::GRAY));
            ui.add_space(12.0);
            ui.label(egui::RichText::new("No shared folders").size(18.0).color(egui::Color32::GRAY));
            ui.add_space(8.0);
            ui.label(egui::RichText::new("The VM is fully isolated from your file system.\nAdd folders here to grant controlled access.").size(12.0).color(egui::Color32::GRAY));
        });
    } else {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut toggle_id = None;
            let mut remove_id = None;

            for entry in shared_folders {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("\u{1F4C1}").size(18.0).color(egui::Color32::from_rgb(59, 130, 246)));
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&entry.name).strong());
                        ui.label(egui::RichText::new(entry.path.to_string_lossy().as_ref()).size(11.0).color(egui::Color32::GRAY));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new(egui::RichText::new("\u{1F5D1}").color(egui::Color32::from_rgb(239, 68, 68)))).clicked() {
                            remove_id = Some(entry.id);
                        }
                        let (label, color) = if entry.read_only {
                            ("\u{1F512} Read Only", egui::Color32::from_rgb(34, 197, 94))
                        } else {
                            ("\u{1F513} Read & Write", egui::Color32::from_rgb(249, 115, 22))
                        };
                        if ui.add(egui::Button::new(egui::RichText::new(label).color(color).size(12.0))).clicked() {
                            toggle_id = Some(entry.id);
                        }
                    });
                });
                ui.separator();
            }

            if let Some(id) = toggle_id {
                let vm = vm_manager.clone();
                runtime.spawn(async move { vm.toggle_read_only(id).await; });
            }
            if let Some(id) = remove_id {
                let vm = vm_manager.clone();
                runtime.spawn(async move { vm.remove_shared_folder(id).await; });
            }
        });
    }

    // Security notice
    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("\u{1F6E1}").color(egui::Color32::from_rgb(34, 197, 94)));
                    ui.label(egui::RichText::new("Changes require VM restart. Write access must be explicitly granted.").size(11.0).color(egui::Color32::GRAY));
                });
            });
    });
}
