use eframe::egui;
use std::sync::{Arc, Mutex};

pub struct TerminalView {
    command_input: String,
    output_history: String,
    running: bool,
    /// Shared buffer so the spawned task can append output asynchronously.
    shared_output: Arc<Mutex<Option<String>>>,
}

impl TerminalView {
    pub fn new() -> Self {
        Self {
            command_input: String::new(),
            output_history: String::new(),
            running: false,
            shared_output: Arc::new(Mutex::new(None)),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // ---------- check for async command completion ----------
        {
            let mut guard = self.shared_output.lock().unwrap();
            if let Some(result) = guard.take() {
                self.output_history.push_str(&result);
                self.running = false;
            }
        }

        // ---------- header ----------
        ui.horizontal(|ui| {
            ui.heading("Terminal");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear").clicked() {
                    self.output_history.clear();
                }
            });
        });

        ui.separator();

        // ---------- output area (green-on-black) ----------
        let frame = egui::Frame::new()
            .fill(egui::Color32::BLACK)
            .inner_margin(egui::Margin::same(8));

        let text = if self.output_history.is_empty() {
            "Ready. Type a command below and press Run."
        } else {
            self.output_history.as_str()
        };

        frame.show(ui, |ui| {
            let available = ui.available_size();
            // Reserve space for the input bar at the bottom (~36 px)
            let output_height = (available.y - 44.0).max(100.0);

            egui::ScrollArea::vertical()
                .max_height(output_height)
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

        // ---------- command input bar ----------
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("$")
                    .monospace()
                    .color(egui::Color32::from_rgb(34, 197, 94)),
            );

            let input_response = ui.add_sized(
                [ui.available_width() - 60.0, 24.0],
                egui::TextEdit::singleline(&mut self.command_input)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            );

            let enter_pressed = input_response.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));

            let can_run = !self.command_input.trim().is_empty() && !self.running;

            let run_clicked = ui
                .add_enabled(can_run, egui::Button::new("Run"))
                .clicked();

            if can_run && (run_clicked || enter_pressed) {
                let command = self.command_input.trim().to_string();
                self.output_history
                    .push_str(&format!("\n$ {}\n", command));
                self.command_input.clear();
                self.running = true;

                let shared = self.shared_output.clone();
                runtime.spawn(async move {
                    let output = tokio::process::Command::new("bash")
                        .arg("-c")
                        .arg(&command)
                        .output()
                        .await;

                    let result = match output {
                        Ok(out) => {
                            let mut buf = String::new();
                            let stdout = String::from_utf8_lossy(&out.stdout);
                            let stderr = String::from_utf8_lossy(&out.stderr);
                            if !stdout.is_empty() {
                                buf.push_str(&stdout);
                            }
                            if !stderr.is_empty() {
                                buf.push_str(&stderr);
                            }
                            if buf.is_empty() {
                                buf.push_str("(no output)\n");
                            }
                            buf
                        }
                        Err(e) => format!("Error: {}\n", e),
                    };

                    let mut guard = shared.lock().unwrap();
                    *guard = Some(result);
                });
            }
        });

        // keep repainting while a command is running so we pick up the result
        if self.running {
            ui.ctx().request_repaint();
        }
    }
}
