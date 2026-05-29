use std::sync::Arc;

use eframe::egui;

use crate::vm::{VmConfig, VmManager};

#[derive(Debug)]
pub struct SetupView {
    pub open: bool,
    step: usize,
    agreed_to_security: bool,
}

impl Default for SetupView {
    fn default() -> Self {
        Self { open: false, step: 0, agreed_to_security: false }
    }
}

impl SetupView {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        vm_manager: &Arc<VmManager>,
        runtime: &tokio::runtime::Handle,
    ) {
        if !self.open { return; }

        let mut still_open = self.open;

        egui::Window::new("TigrimOS Setup")
            .open(&mut still_open)
            .resizable(false).collapsible(false)
            .default_size([560.0, 480.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // Step indicator
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(ui.available_width() / 2.0 - 80.0);
                    for i in 0..3usize {
                        let color = if i <= self.step {
                            egui::Color32::from_rgb(249, 115, 22)
                        } else {
                            egui::Color32::from_rgb(75, 85, 99)
                        };
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 12.0, color);
                        let label = if i < self.step { "\u{2713}".to_string() } else { format!("{}", i + 1) };
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, &label, egui::FontId::proportional(11.0), egui::Color32::WHITE);
                        if i < 2 {
                            let line_color = if i < self.step { egui::Color32::from_rgb(249, 115, 22) } else { egui::Color32::from_rgb(75, 85, 99) };
                            let (lr, _) = ui.allocate_exact_size(egui::vec2(40.0, 24.0), egui::Sense::hover());
                            let y = lr.center().y;
                            ui.painter().line_segment([egui::pos2(lr.left(), y), egui::pos2(lr.right(), y)], egui::Stroke::new(2.0, line_color));
                        }
                    }
                });

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(12.0);

                match self.step {
                    0 => self.welcome_step(ui),
                    1 => self.security_step(ui),
                    2 => Self::ready_step(ui),
                    _ => {}
                }

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(8.0);

                // Navigation
                ui.horizontal(|ui| {
                    if self.step > 0 && ui.button("Back").clicked() { self.step -= 1; }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.step < 2 {
                            let enabled = !(self.step == 1 && !self.agreed_to_security);
                            if ui.add_enabled(enabled, egui::Button::new("Next")).clicked() { self.step += 1; }
                        } else {
                            let btn = egui::Button::new(egui::RichText::new("Start TigrimOS").size(15.0).strong())
                                .fill(egui::Color32::from_rgb(34, 197, 94));
                            if ui.add(btn).clicked() {
                                let vm = vm_manager.clone();
                                runtime.spawn(async move { let _ = vm.start_vm().await; });
                                self.open = false;
                            }
                        }
                    });
                });
            });

        self.open = still_open;
    }

    fn welcome_step(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("\u{1F42F}").size(64.0));
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Welcome to TigrimOS").size(26.0).strong());
            ui.add_space(8.0);
            ui.label(egui::RichText::new("TigrimOS runs inside a secure Ubuntu sandbox\non your machine. No Docker required.").size(14.0).color(egui::Color32::GRAY));
        });
        ui.add_space(16.0);
        for (icon, text) in [
            ("\u{1F6E1}", "Full VM isolation via QEMU"),
            ("\u{1F512}", "Host files only accessible with your permission"),
            ("\u{26A1}", "Native performance with HVF acceleration"),
            ("\u{2B07}", "~2GB download for Ubuntu base image"),
        ] {
            ui.horizontal(|ui| {
                ui.add_space(40.0);
                ui.label(egui::RichText::new(icon).size(16.0).color(egui::Color32::from_rgb(249, 115, 22)));
                ui.label(text);
            });
            ui.add_space(4.0);
        }
    }

    fn security_step(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("\u{1F6E1}").size(48.0).color(egui::Color32::from_rgb(34, 197, 94)));
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Security Model").size(22.0).strong());
        });
        ui.add_space(16.0);
        for (icon, title, detail) in [
            ("\u{1F5A5}", "VM Isolation", "TigrimOS runs in a real Ubuntu VM. It cannot access your host processes, files, or network except through controlled channels."),
            ("\u{1F4C1}", "File Access", "No host folders are shared by default. You choose which folders to share and their permissions."),
            ("\u{1F310}", "Network", "The VM uses NAT networking. Only the configured port is forwarded."),
        ] {
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label(egui::RichText::new(icon).size(20.0).color(egui::Color32::from_rgb(18, 154, 145)));
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(title).strong());
                    ui.label(egui::RichText::new(detail).size(12.0).color(egui::Color32::GRAY));
                });
            });
            ui.add_space(8.0);
        }
        ui.horizontal(|ui| {
            ui.add_space(40.0);
            ui.checkbox(&mut self.agreed_to_security, "I understand the security model");
        });
    }

    fn ready_step(ui: &mut egui::Ui) {
        let disk_size_gb = VmConfig::DISK_SIZE_GB;
        let port = VmConfig::VM_PORT;

        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("\u{2705}").size(48.0).color(egui::Color32::from_rgb(34, 197, 94)));
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Ready to Go").size(22.0).strong());
            ui.label("TigrimOS will now:");
        });
        ui.add_space(12.0);
        let steps = [
            "Download Ubuntu 22.04 cloud image (~700MB)".to_string(),
            format!("Create a {}GB virtual disk", disk_size_gb),
            "Install Node.js 20, Python 3, and dependencies".to_string(),
            "Deploy TigrimOS inside the VM".to_string(),
            format!("Start the web UI at localhost:{}", port),
        ];
        for (i, text) in steps.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add_space(40.0);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 11.0, egui::Color32::from_rgb(249, 115, 22));
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, &format!("{}", i + 1), egui::FontId::proportional(11.0), egui::Color32::WHITE);
                ui.label(text.as_str());
            });
            ui.add_space(4.0);
        }
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("First setup takes about 5-10 minutes.\nSubsequent starts are much faster.").size(12.0).color(egui::Color32::GRAY));
        });
    }
}
