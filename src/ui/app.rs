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
use super::remote_view::RemoteView;
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
    RemoteServer,
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
    remote_view: RemoteView,
    pub remote_mode: bool,
    logo_texture: Option<egui::TextureHandle>,
    // Kimi-style sidebar state
    sidebar_open: bool,
    chat_history_expanded: bool,
}

#[allow(dead_code)]
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

            // Add a symbol font so sidebar icons render correctly
            let symbol_paths = [
                "/System/Library/Fonts/Apple Symbols.ttf",
                "/System/Library/Fonts/Supplemental/Apple Symbols.ttf",
                "/System/Library/Fonts/SFCompact.ttf",
                "/System/Library/Fonts/Menlo.ttc",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            ];
            for path in &symbol_paths {
                if let Ok(data) = std::fs::read(path) {
                    fonts.font_data.insert(
                        "SymbolFallback".to_owned(),
                        egui::FontData::from_owned(data).into(),
                    );
                    if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                        v.push("SymbolFallback".to_owned());
                    }
                    if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                        v.push("SymbolFallback".to_owned());
                    }
                    break;
                }
            }

            cc.egui_ctx.set_fonts(fonts);
        }

        // ── CeTeaU-style light/blue theme ────────────────────────────
        let mut visuals = egui::Visuals::light();

        // Base palette – soft blue tones inspired by CeTeauAI
        let bg_white   = egui::Color32::WHITE;
        let bg_light   = egui::Color32::from_rgb(243, 246, 251);  // #f3f6fb very light blue-gray
        let bg_card    = egui::Color32::from_rgb(255, 255, 255);  // white cards
        let bg_hover   = egui::Color32::from_rgb(232, 237, 245);  // #e8edf5 soft blue hover
        let border     = egui::Color32::from_rgb(210, 218, 230);  // #d2dae6 soft blue border
        let text_primary   = egui::Color32::from_rgb(33, 37, 41);   // #212529 near black
        let text_secondary = egui::Color32::from_rgb(108, 117, 125); // #6c757d muted gray
        let accent     = egui::Color32::from_rgb(74, 144, 226);   // #4a90e2 clean blue
        let accent_hover = egui::Color32::from_rgb(56, 123, 199);

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
            remote_view: RemoteView::new(),
            remote_mode: false,
            logo_texture: None,
            sidebar_open: false,
            chat_history_expanded: false,
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

        // ── Colors ──────────────────────────────────────────────
        let accent = egui::Color32::from_rgb(74, 144, 226);   // #4a90e2
        let text_dim = egui::Color32::from_rgb(108, 117, 125);  // muted gray
        let text_dark = egui::Color32::from_rgb(33, 37, 41);
        let border_color = egui::Color32::from_rgb(210, 218, 230);
        let sidebar_bg = egui::Color32::from_rgb(232, 237, 245); // #e8edf5 soft blue sidebar

        // ── Left sidebar (Kimi-style) ────────────────────────────────
        let sidebar_w = if self.sidebar_open { 240.0 } else { 52.0 };

        egui::SidePanel::left("nav_sidebar")
            .exact_width(sidebar_w)
            .resizable(false)
            .frame(egui::Frame::new()
                .fill(sidebar_bg)
                .inner_margin(egui::Margin::symmetric(8, 12))
                .stroke(egui::Stroke::new(0.5, border_color)))
            .show(ctx, |ui| {
                ui.set_min_width(sidebar_w - 16.0);
                ui.spacing_mut().item_spacing.y = 2.0;

                // ── Header: Logo + collapse toggle ──
                ui.horizontal(|ui| {
                    if self.sidebar_open {
                        if let Some(tex) = &self.logo_texture {
                            ui.add(egui::Image::new(tex).max_size(egui::vec2(22.0, 22.0)).corner_radius(4.0));
                        }
                        ui.label(egui::RichText::new("TigrimOS").size(16.0).strong().color(text_dark));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let icon = if self.sidebar_open { "\u{25C0}" } else { "\u{2630}" }; // ◀ or ☰
                        let btn = egui::Button::new(egui::RichText::new(icon).size(14.0).color(text_dim))
                            .fill(egui::Color32::TRANSPARENT);
                        if ui.add(btn).on_hover_text(if self.sidebar_open { "Collapse sidebar" } else { "Expand sidebar" }).clicked() {
                            self.sidebar_open = !self.sidebar_open;
                        }
                    });
                });

                ui.add_space(8.0);

                // ── New Chat button ──
                {
                    let (label, icon) = if self.sidebar_open { ("+  New chat", "") } else { ("+", "New chat") };
                    let btn = egui::Button::new(egui::RichText::new(label).size(13.0).color(accent))
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(8.0)
                        .min_size(egui::vec2(if self.sidebar_open { ui.available_width() } else { 36.0 }, 32.0));
                    let resp = ui.add(btn);
                    if !self.sidebar_open {
                        resp.clone().on_hover_text(icon);
                    }
                    if resp.clicked() {
                        self.chat_view.create_new_chat_from_sidebar(&self.runtime);
                        self.selected_tab = Tab::Chat;
                    }
                }

                ui.add_space(8.0);
                ui.add(egui::Separator::default().spacing(4.0));
                ui.add_space(4.0);

                // ── Navigation items ──
                let nav_items: &[(Tab, &str, &str)] = &[
                    (Tab::Chat,         "\u{2709}",  "Chat"),        // ✉ envelope
                    (Tab::Projects,     "\u{2302}",  "Projects"),    // ⌂ house/home
                    (Tab::Agents,       "\u{2638}",  "Agent Swarm"), // ☸ wheel/dharma = swarm
                    (Tab::Files,        "\u{25A4}",  "Files"),       // ▤ square with horizontal fill = documents
                    (Tab::Tasks,        "\u{2611}",  "Tasks"),       // ☑ ballot box with check
                    (Tab::Skills,       "\u{2605}",  "Skills"),      // ★ black star
                    (Tab::Terminal,     "\u{2588}",  "Terminal"),     // █ full block = terminal cursor
                    (Tab::RemoteServer, "\u{2641}",  "Remote"),      // ♁ earth
                    (Tab::Console,      "\u{2261}",  "VM Log"),      // ≡ identical/log lines
                    (Tab::Folders,      "\u{21C4}",  "Shared"),      // ⇄ rightwards/leftwards arrows = share
                ];

                for &(tab, icon, label) in nav_items {
                    let is_active = self.selected_tab == tab;
                    let text_color = if is_active { accent } else { text_dim };
                    let bg = if is_active {
                        egui::Color32::from_rgba_premultiplied(88, 166, 255, 20)
                    } else {
                        egui::Color32::TRANSPARENT
                    };

                    let btn_text = if self.sidebar_open {
                        format!("{}  {}", icon, label)
                    } else {
                        icon.to_string()
                    };
                    let btn = egui::Button::new(egui::RichText::new(&btn_text).size(13.0).color(text_color))
                        .fill(bg)
                        .corner_radius(6.0)
                        .min_size(egui::vec2(if self.sidebar_open { ui.available_width() } else { 36.0 }, 30.0));
                    let resp = ui.add(btn);
                    if !self.sidebar_open {
                        resp.clone().on_hover_text(label);
                    }
                    if resp.clicked() {
                        self.selected_tab = tab;
                    }
                }

                ui.add_space(4.0);
                ui.add(egui::Separator::default().spacing(4.0));
                ui.add_space(4.0);

                // ── Chat History section (collapsible) ──
                if self.sidebar_open {
                    // Ensure sessions are loaded
                    self.chat_view.ensure_sessions_loaded(&self.runtime);

                    let arrow = if self.chat_history_expanded { "\u{25BC}" } else { "\u{25B6}" }; // ▼ or ▶
                    let header_btn = egui::Button::new(
                        egui::RichText::new(format!("{} \u{21BB}  Chat History", arrow)).size(12.0).color(text_dim),
                    )
                    .fill(egui::Color32::TRANSPARENT)
                    .min_size(egui::vec2(ui.available_width(), 26.0));
                    if ui.add(header_btn).clicked() {
                        self.chat_history_expanded = !self.chat_history_expanded;
                    }

                    if self.chat_history_expanded {
                        let sessions: Vec<(String, String, bool)> = self.chat_view.session_summaries()
                            .iter()
                            .take(15)
                            .map(|s| {
                                let streaming = self.chat_view.is_session_streaming(&s.id);
                                (s.id.clone(), s.title.clone(), streaming)
                            })
                            .collect();

                        egui::ScrollArea::vertical()
                            .id_salt("sidebar_chat_history")
                            .max_height(300.0)
                            .show(ui, |ui| {
                                let mut clicked_id: Option<String> = None;
                                let mut delete_id: Option<String> = None;
                                let selected_id = self.chat_view.selected_session_id.clone();
                                for (id, title, streaming) in &sessions {
                                    let is_sel = selected_id.as_deref() == Some(id.as_str());
                                    let bg = if is_sel {
                                        egui::Color32::from_rgba_premultiplied(88, 166, 255, 20)
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    };
                                    let display = if title.len() > 28 {
                                        format!("{}...", &title[..title.char_indices().nth(28).map(|(i,_)|i).unwrap_or(title.len())])
                                    } else {
                                        title.clone()
                                    };
                                    let label_text = if *streaming {
                                        format!("\u{1F7E1} {}", display) // 🟡
                                    } else {
                                        format!("  {}", display)
                                    };
                                    let btn = egui::Button::new(
                                        egui::RichText::new(&label_text).size(11.5).color(if is_sel { text_dark } else { text_dim }),
                                    )
                                    .fill(bg)
                                    .corner_radius(4.0)
                                    .min_size(egui::vec2(ui.available_width(), 24.0));
                                    let resp = ui.add(btn);
                                    if resp.clicked() {
                                        clicked_id = Some(id.clone());
                                    }
                                    // Right-click context menu
                                    let ctx_id = ui.id().with("chat_ctx").with(id.as_str());
                                    if resp.secondary_clicked() {
                                        ui.memory_mut(|mem| mem.toggle_popup(ctx_id));
                                    }
                                    egui::popup_below_widget(ui, ctx_id, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                                        ui.set_min_width(120.0);
                                        if ui.add(egui::Button::new(
                                            egui::RichText::new("\u{1F5D1} Delete Chat").size(12.0).color(egui::Color32::from_rgb(220, 38, 38)),
                                        ).fill(egui::Color32::TRANSPARENT).min_size(egui::vec2(110.0, 26.0))).clicked() {
                                            delete_id = Some(id.clone());
                                        }
                                    });
                                }
                                if let Some(id) = clicked_id {
                                    self.chat_view.select_session_from_sidebar(id);
                                    self.selected_tab = Tab::Chat;
                                }
                                if let Some(id) = delete_id {
                                    self.chat_view.delete_session_from_sidebar(&self.runtime, &id);
                                }
                            });
                    }
                } else {
                    // Collapsed: just show icon
                    let btn = egui::Button::new(egui::RichText::new("\u{21BB}").size(14.0).color(text_dim))
                        .fill(egui::Color32::TRANSPARENT)
                        .min_size(egui::vec2(36.0, 30.0));
                    if ui.add(btn).on_hover_text("Chat History").clicked() {
                        self.sidebar_open = true;
                        self.chat_history_expanded = true;
                    }
                }

                // ── Bottom section — push to bottom ──
                let remaining = ui.available_height() - 80.0;
                if remaining > 0.0 {
                    ui.add_space(remaining);
                }

                // VM status tag (compact — start/stop moved to Settings)
                let vm_color = Self::status_color(snap.state, snap.service_ready);
                if self.sidebar_open {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, vm_color);
                        let vm_label = if self.remote_mode {
                            let name = self.remote_view.instance_name();
                            if name.is_empty() { "Remote".to_string() } else { format!("Remote: {}", name) }
                        } else {
                            format!("VM: {}", snap.state.label())
                        };
                        ui.label(egui::RichText::new(&vm_label).size(11.0).color(text_dim));
                    });
                    if snap.state == VmState::Downloading {
                        ui.add(egui::ProgressBar::new(snap.progress as f32).show_percentage().animate(true));
                    }
                } else {
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, vm_color);
                }

                ui.add_space(4.0);

                // Settings
                {
                    let (label, tip) = if self.sidebar_open { ("\u{2699}  Settings", "") } else { ("\u{2699}", "Settings") };
                    let btn = egui::Button::new(egui::RichText::new(label).size(13.0).color(text_dim))
                        .fill(egui::Color32::TRANSPARENT)
                        .corner_radius(6.0)
                        .min_size(egui::vec2(if self.sidebar_open { ui.available_width() } else { 36.0 }, 30.0));
                    let resp = ui.add(btn);
                    if !self.sidebar_open {
                        resp.clone().on_hover_text(tip);
                    }
                    if resp.clicked() {
                        self.settings_view.open = true;
                    }
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

            // Handle navigation from Tasks -> Chat
            if let Some(session_id) = self.tasks_view.navigate_to_chat.take() {
                self.chat_view.selected_session_id = Some(session_id);
                self.chat_view.needs_refresh = true;
                self.selected_tab = Tab::Chat;
            }

            // In remote mode, the same tabs work transparently against the remote server
            // via the data layer's RemoteBackend proxy. Terminal passes VmState::Running
            // since the remote server is always "running".
            match self.selected_tab {
                Tab::Chat => self.chat_view.show(ui, &self.runtime),
                Tab::Projects => self.projects_view.show(ui, &self.runtime),
                Tab::Agents => self.agents_view.show(ui, &self.runtime),
                Tab::Files => self.files_view.show(ui, &self.runtime),
                Tab::Tasks => self.tasks_view.show(ui, &self.runtime),
                Tab::Skills => self.skills_view.show(ui, &self.runtime),
                Tab::Terminal => {
                    let effective_state = if self.remote_mode { VmState::Running } else { snap.state };
                    self.terminal_view.show(ui, &self.runtime, effective_state);
                }
                Tab::RemoteServer => self.remote_view.show(ui, &self.runtime),
                Tab::Console => console_view(ui, &snap.console_output, &self.vm_manager, &self.runtime),
                Tab::Folders => shared_folders_view(ui, &snap.shared_folders, &self.vm_manager, &self.runtime),
            }
        });
    }
}
