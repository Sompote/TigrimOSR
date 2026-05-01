use std::sync::Arc;

use eframe::egui;

use crate::vm::{VmManager, VmState, SharedFolderEntry};

use super::agents_view::AgentsView;
use super::chat::ChatView;
use super::console::console_view;
use super::files_view::FilesView;
use super::projects_view::ProjectsView;
use super::settings::SettingsView;
use super::setup::SetupView;
use super::shared_folders::shared_folders_view;
use super::skills_view::SkillsView;
use super::tasks_view::TasksView;
use super::terminal_view::TerminalView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Chat,
    Projects,
    Agents,
    Files,
    Tasks,
    Skills,
    Terminal,
    Console,
    Folders,
}

pub struct TigrimOSApp {
    pub vm_manager: Arc<VmManager>,
    pub selected_tab: Tab,
    pub show_reset_alert: bool,
    pub runtime: tokio::runtime::Handle,
    settings_view: SettingsView,
    setup_view: SetupView,
    chat_view: ChatView,
    files_view: FilesView,
    projects_view: ProjectsView,
    tasks_view: TasksView,
    skills_view: SkillsView,
    agents_view: AgentsView,
    terminal_view: TerminalView,
    logo_texture: Option<egui::TextureHandle>,
}

struct VmSnapshot {
    state: VmState,
    service_ready: bool,
    progress: f64,
    vm_ip_address: Option<String>,
    error_message: Option<String>,
    console_output: String,
    shared_folders: Vec<SharedFolderEntry>,
}

impl TigrimOSApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        vm_manager: Arc<VmManager>,
        runtime_handle: tokio::runtime::Handle,
    ) -> Self {
        // ── Thai font support ────────────────────────────────────────
        // egui 0.31 uses ab_glyph which has no OpenType shaping, so Thai
        // combining marks won't be kerned perfectly. Loading ThonburiUI
        // (the macOS system UI Thai font) as PRIMARY font gives the best
        // result available until we upgrade to egui 0.32+ (cosmic-text).
        {
            let mut fonts = egui::FontDefinitions::default();

            // Ayuthaya has clean combining marks without dotted-circle placeholders.
            // Fall back to Silom / SukhumvitSet / Arial Unicode on other systems.
            let thai_paths = [
                "/System/Library/Fonts/Supplemental/Ayuthaya.ttf",
                "/System/Library/Fonts/Supplemental/Silom.ttf",
                "/System/Library/Fonts/Supplemental/SukhumvitSet.ttc",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
                "/System/Library/Fonts/Supplemental/Thonburi.ttc",
                "/Library/Fonts/Thonburi.ttc",
                "/System/Library/Fonts/ThonburiUI.ttc",
            ];
            for path in &thai_paths {
                if let Ok(data) = std::fs::read(path) {
                    fonts.font_data.insert(
                        "ThaiFallback".to_owned(),
                        egui::FontData::from_owned(data).into(),
                    );
                    // Insert as FIRST fallback so Thai codepoints are found early
                    if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                        // insert after the default Latin font so Latin still renders sharp
                        let pos = v.len().saturating_sub(0);
                        v.insert(pos, "ThaiFallback".to_owned());
                    }
                    if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                        v.push("ThaiFallback".to_owned());
                    }
                    break;
                }
            }
            cc.egui_ctx.set_fonts(fonts);
        }

        // ── Canva-style light/white theme ────────────────────────────
        let mut visuals = egui::Visuals::light();

        // Base palette (light)
        let bg_white   = egui::Color32::WHITE;
        let bg_light   = egui::Color32::from_rgb(248, 249, 250);  // #f8f9fa
        let bg_card    = egui::Color32::from_rgb(255, 255, 255);  // white cards
        let bg_hover   = egui::Color32::from_rgb(240, 242, 245);  // light hover
        let border     = egui::Color32::from_rgb(225, 228, 232);  // #e1e4e8
        let text_primary   = egui::Color32::from_rgb(31, 35, 40);   // near black
        let text_secondary = egui::Color32::from_rgb(101, 109, 118); // gray
        let accent     = egui::Color32::from_rgb(88, 166, 255);   // blue accent
        let accent_hover = egui::Color32::from_rgb(56, 139, 253);

        // Window & panel backgrounds
        visuals.panel_fill = bg_white;
        visuals.window_fill = bg_light;
        visuals.extreme_bg_color = bg_light;
        visuals.faint_bg_color = bg_light;

        // Widget styles
        visuals.widgets.noninteractive.bg_fill = bg_card;
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, text_secondary);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(0.5, border);
        visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(6);

        visuals.widgets.inactive.bg_fill = bg_light;
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text_primary);
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(0.5, border);
        visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);

        visuals.widgets.hovered.bg_fill = bg_hover;
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, text_primary);
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent);
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);

        visuals.widgets.active.bg_fill = accent;
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, accent_hover);
        visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);

        visuals.widgets.open.bg_fill = bg_hover;
        visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, text_primary);
        visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, accent);
        visuals.widgets.open.corner_radius = egui::CornerRadius::same(6);

        // Selection & hyperlinks
        visuals.selection.bg_fill = egui::Color32::from_rgba_premultiplied(88, 166, 255, 50);
        visuals.selection.stroke = egui::Stroke::new(1.0, accent);
        visuals.hyperlink_color = accent;

        // Window shadow & stroke
        visuals.window_stroke = egui::Stroke::new(1.0, border);
        visuals.window_shadow = egui::epaint::Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: egui::Color32::from_rgba_premultiplied(0, 0, 0, 20),
        };
        visuals.popup_shadow = visuals.window_shadow;

        // Cursor
        visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);

        // Separator
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(0.5, border);

        cc.egui_ctx.set_visuals(visuals);

        // Spacing / global style
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);  // extra vertical for Thai
        style.spacing.window_margin = egui::Margin::same(12);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.scroll = egui::style::ScrollStyle {
            bar_width: 6.0,
            ..style.spacing.scroll
        };

        // Larger body font so Thai diacritics are legible
        // (ab_glyph has no OpenType shaping; bigger size helps readability)
        use egui::{FontFamily, FontId, TextStyle};
        style.text_styles = [
            (TextStyle::Small,   FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Body,    FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Button,  FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ].into();

        cc.egui_ctx.set_style(style);

        Self {
            vm_manager,
            selected_tab: Tab::Chat,
            show_reset_alert: false,
            runtime: runtime_handle,
            settings_view: SettingsView::default(),
            setup_view: SetupView::default(),
            chat_view: ChatView::new(),
            files_view: FilesView::new(),
            projects_view: ProjectsView::new(),
            tasks_view: TasksView::new(),
            skills_view: SkillsView::new(),
            agents_view: AgentsView::default(),
            terminal_view: TerminalView::new(),
            logo_texture: None,
        }
    }

    fn get_logo_texture(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if self.logo_texture.is_none() {
            if let Ok(bytes) = std::fs::read("assets/icon.png") {
                if let Ok(image) = image::load_from_memory(&bytes) {
                    let rgba = image.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let pixels = rgba.into_raw();
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                    self.logo_texture = Some(ctx.load_texture(
                        "app_logo",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        }
        self.logo_texture.as_ref()
    }

    fn status_color(state: VmState, service_ready: bool) -> egui::Color32 {
        match state {
            VmState::Running if service_ready => egui::Color32::from_rgb(34, 197, 94),
            VmState::Running => egui::Color32::from_rgb(250, 204, 21),
            VmState::Stopped => egui::Color32::from_rgb(156, 163, 175),
            VmState::Error => egui::Color32::from_rgb(239, 68, 68),
            _ => egui::Color32::from_rgb(250, 204, 21),
        }
    }

    fn snapshot(&self) -> VmSnapshot {
        let vm = self.vm_manager.clone();
        self.runtime.block_on(async {
            VmSnapshot {
                state: vm.state().await,
                service_ready: vm.service_ready().await,
                progress: vm.progress().await,
                vm_ip_address: vm.vm_ip_address().await,
                error_message: vm.error_message().await,
                console_output: vm.console_output().await,
                shared_folders: vm.shared_folders().await,
            }
        })
    }

    fn spawn_start(&self) {
        let vm = self.vm_manager.clone();
        self.runtime.spawn(async move { let _ = vm.start_vm().await; });
    }

    fn spawn_stop(&self) {
        let vm = self.vm_manager.clone();
        self.runtime.spawn(async move { vm.stop_vm().await; });
    }

    fn spawn_reset(&self) {
        let vm = self.vm_manager.clone();
        self.runtime.spawn(async move { vm.reset_vm().await; });
    }
}

impl eframe::App for TigrimOSApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let snap = self.snapshot();
        self.get_logo_texture(ctx);

        if snap.state != VmState::Stopped && snap.state != VmState::Running && snap.state != VmState::Error {
            ctx.request_repaint();
        }

        // ── Top navigation bar ──────────────────────────────────
        let topbar_bg = egui::Color32::WHITE;
        let accent = egui::Color32::from_rgb(88, 166, 255);
        let text_dim = egui::Color32::from_rgb(101, 109, 118);
        let border_color = egui::Color32::from_rgb(225, 228, 232);

        egui::TopBottomPanel::top("top_bar")
            .frame(egui::Frame::new()
                .fill(topbar_bg)
                .inner_margin(egui::Margin::symmetric(16, 8))
                .stroke(egui::Stroke::new(1.0, border_color)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Brand logo
                    if let Some(tex) = &self.logo_texture {
                        let img = egui::Image::new(tex)
                            .max_size(egui::vec2(24.0, 24.0))
                            .rounding(4.0);
                        ui.add(img);
                    }
                    ui.label(
                        egui::RichText::new("TigrimOS")
                            .size(17.0)
                            .strong()
                            .color(egui::Color32::from_rgb(31, 35, 40)),
                    );
                    ui.add_space(16.0);

                    // VM status pill
                    let vm_color = Self::status_color(snap.state, snap.service_ready);
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(240, 242, 245))
                        .corner_radius(12.0)
                        .inner_margin(egui::Margin::symmetric(8, 3))
                        .stroke(egui::Stroke::new(0.5, border_color))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                                ui.painter().circle_filled(dot.center(), 4.0, vm_color);
                                ui.label(egui::RichText::new(snap.state.label()).size(11.0).color(egui::Color32::from_rgb(31, 35, 40)));
                            });
                        });

                    // VM controls
                    ui.add_space(4.0);
                    if snap.state == VmState::Stopped || snap.state == VmState::Error {
                        let btn = egui::Button::new(egui::RichText::new("\u{25B6}").size(11.0).color(egui::Color32::WHITE))
                            .fill(egui::Color32::from_rgb(34, 197, 94))
                            .corner_radius(4.0);
                        if ui.add(btn).on_hover_text("Start VM").clicked() { self.spawn_start(); }
                    } else if snap.state == VmState::Running {
                        let btn = egui::Button::new(egui::RichText::new("\u{25A0}").size(11.0).color(egui::Color32::WHITE))
                            .fill(egui::Color32::from_rgb(239, 68, 68))
                            .corner_radius(4.0);
                        if ui.add(btn).on_hover_text("Stop VM").clicked() { self.spawn_stop(); }
                    } else {
                        ui.spinner();
                    }

                    ui.add_space(12.0);

                    // Navigation tabs — styled buttons
                    let tabs: &[(Tab, &str)] = &[
                        (Tab::Chat, "Chat"),
                        (Tab::Projects, "Projects"),
                        (Tab::Agents, "Agents"),
                        (Tab::Files, "Files"),
                        (Tab::Tasks, "Tasks"),
                        (Tab::Skills, "Skills"),
                        (Tab::Terminal, "Terminal"),
                    ];

                    for &(tab, label) in tabs {
                        let is_active = self.selected_tab == tab;
                        let text_color = if is_active {
                            accent
                        } else {
                            text_dim
                        };
                        let fill = if is_active {
                            egui::Color32::from_rgba_premultiplied(88, 166, 255, 15)
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let stroke = if is_active {
                            egui::Stroke::new(1.0, accent)
                        } else {
                            egui::Stroke::NONE
                        };

                        let btn = egui::Button::new(
                            egui::RichText::new(label).size(13.0).color(text_color),
                        )
                        .fill(fill)
                        .stroke(stroke)
                        .corner_radius(6.0);

                        if ui.add(btn).clicked() {
                            self.selected_tab = tab;
                        }
                    }

                    ui.add_space(4.0);

                    // Secondary tabs (smaller)
                    for &(tab, label) in &[
                        (Tab::Console, "VM Log"),
                        (Tab::Folders, "Shared"),
                    ] {
                        let is_active = self.selected_tab == tab;
                        let text_color = if is_active { accent } else { text_dim };
                        let fill = if is_active {
                            egui::Color32::from_rgba_premultiplied(88, 166, 255, 15)
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let btn = egui::Button::new(
                            egui::RichText::new(label).size(11.0).color(text_color),
                        )
                        .fill(fill)
                        .corner_radius(4.0);
                        if ui.add(btn).clicked() {
                            self.selected_tab = tab;
                        }
                    }

                    // Right-aligned buttons
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let gear = egui::Button::new(
                            egui::RichText::new("\u{2699}").size(16.0).color(text_dim),
                        )
                        .fill(egui::Color32::TRANSPARENT);
                        if ui.add(gear).on_hover_text("Settings").clicked() {
                            self.settings_view.open = true;
                        }
                    });
                });

                if snap.state == VmState::Downloading {
                    ui.add_space(4.0);
                    ui.add(egui::ProgressBar::new(snap.progress as f32).show_percentage().animate(true));
                }
            });

        // Reset dialog
        if self.show_reset_alert {
            egui::Window::new("Reset VM?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("This will stop the VM and delete its disk.\nOn next start it will re-download and re-provision.")
                            .color(egui::Color32::from_rgb(139, 148, 158)),
                    );
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_reset_alert = false;
                        }
                        ui.add_space(8.0);
                        let reset_btn = egui::Button::new(
                            egui::RichText::new("Reset VM").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(220, 38, 38))
                        .corner_radius(6.0);
                        if ui.add(reset_btn).clicked() {
                            self.show_reset_alert = false;
                            self.spawn_reset();
                        }
                    });
                });
        }

        self.settings_view.show(ctx, &self.vm_manager, &self.runtime);
        self.setup_view.show(ctx, &self.vm_manager, &self.runtime);

        // Central panel - main content
        egui::CentralPanel::default()
            .frame(egui::Frame::new()
                .fill(egui::Color32::WHITE)
                .inner_margin(egui::Margin::same(0)))
            .show(ctx, |ui| {
            // Handle navigation from Projects -> Chat
            if let Some((project_id, session_id)) = self.projects_view.navigate_to_chat_session.take() {
                self.chat_view.selected_project_id = Some(project_id);
                self.chat_view.selected_session_id = Some(session_id);
                self.chat_view.needs_refresh = true;
                self.selected_tab = Tab::Chat;
            }

            match self.selected_tab {
                Tab::Chat => self.chat_view.show(ui, &self.runtime),
                Tab::Projects => self.projects_view.show(ui, &self.runtime),
                Tab::Agents => self.agents_view.show(ui, &self.runtime),
                Tab::Files => self.files_view.show(ui, &self.runtime),
                Tab::Tasks => self.tasks_view.show(ui, &self.runtime),
                Tab::Skills => self.skills_view.show(ui, &self.runtime),
                Tab::Terminal => self.terminal_view.show(ui, &self.runtime),
                Tab::Console => console_view(ui, &snap.console_output, &self.vm_manager, &self.runtime),
                Tab::Folders => shared_folders_view(ui, &snap.shared_folders, &self.vm_manager, &self.runtime),
            }
        });
    }
}
