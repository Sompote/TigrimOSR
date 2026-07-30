use std::sync::Arc;

use eframe::egui;

use crate::vm::{SharedFolderEntry, VmManager, VmState};

use super::agents_view::AgentsView;
use super::chat::ChatView;
use super::console::console_view;
use super::files_view::FilesView;
use super::projects_view::ProjectsView;
use super::remote_view::RemoteView;
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
    RemoteServer,
    Console,
    Folders,
}

pub struct AndrewOSApp {
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
    sidebar_logo_texture: Option<egui::TextureHandle>,
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

impl AndrewOSApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        vm_manager: Arc<VmManager>,
        runtime_handle: tokio::runtime::Handle,
    ) -> Self {
        // ── User-customizable theme (fonts, colors, sizes) ─────────────
        // Loaded from data/theme.yaml; falls back to the warm-neutral / teal
        // defaults + bundled Plus Jakarta Sans if the file is missing.
        // Editable live in Settings → Theme.
        let theme = super::theme::Theme::load();
        theme.apply_fonts(&cc.egui_ctx);
        theme.apply(&cc.egui_ctx);

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
            sidebar_logo_texture: None,
            sidebar_open: false,
            chat_history_expanded: false,
        }
    }

    fn get_sidebar_logo(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if self.sidebar_logo_texture.is_none() {
            // Try multiple paths for the sidebar logo
            let paths = [
                "assets/logo_sidebar.png",
                &format!("{}/assets/logo_sidebar.png", env!("CARGO_MANIFEST_DIR")),
            ];
            for path in &paths {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(image) = image::load_from_memory(&bytes) {
                        let rgba = image.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let pixels = rgba.into_raw();
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                        self.sidebar_logo_texture = Some(ctx.load_texture(
                            "sidebar_logo",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                        break;
                    }
                }
            }
        }
        self.sidebar_logo_texture.as_ref()
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
        self.runtime.spawn(async move {
            let _ = vm.start_vm().await;
        });
    }

    fn spawn_stop(&self) {
        let vm = self.vm_manager.clone();
        self.runtime.spawn(async move {
            vm.stop_vm().await;
        });
    }

    fn spawn_reset(&self) {
        let vm = self.vm_manager.clone();
        self.runtime.spawn(async move {
            vm.reset_vm().await;
        });
    }
}

/// A collapsed-rail icon button, painted by hand.
///
/// egui derives a button's text position from the *parent layout's* alignment,
/// so in the rail's top-down column the glyph is pushed to the left edge of the
/// button box. Padding cannot correct that — every glyph has a different width,
/// so any fixed padding centres exactly one of them. Allocating the square and
/// painting the glyph at `rect.center()` centres all of them on both axes.
#[allow(clippy::too_many_arguments)]
fn rail_icon_button(
    ui: &mut egui::Ui,
    glyph: &str,
    side: f32,
    glyph_size: f32,
    fill: egui::Color32,
    hover_fill: egui::Color32,
    stroke: egui::Stroke,
    ink: egui::Color32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let radius = egui::CornerRadius::same((side * 0.27) as u8);
        let bg = if resp.hovered() { hover_fill } else { fill };
        let painter = ui.painter();
        painter.rect_filled(rect, radius, bg);
        if stroke.width > 0.0 {
            painter.rect_stroke(rect, radius, stroke, egui::StrokeKind::Inside);
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(glyph_size),
            ink,
        );
    }
    resp
}

impl eframe::App for AndrewOSApp {
    /// Clear to fully transparent so the "Transparent" theme's translucent
    /// panel fills reveal the desktop. Opaque themes paint over this entirely.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let snap = self.snapshot();
        self.get_logo_texture(ctx);

        if snap.state != VmState::Stopped
            && snap.state != VmState::Running
            && snap.state != VmState::Error
        {
            ctx.request_repaint();
        }

        // ── Colors (from the active theme) ──────────────────────────
        let accent = super::theme::accent_color();
        let text_dim = super::theme::text_secondary_color();
        let text_dark = super::theme::text_primary_color();
        let sidebar_bg = super::theme::hover_color(); // cool rail

        // ── Ambient background (liquid glass) ───────────────────────
        // One full-window gradient + teal glow on the background layer; the
        // panels above use translucent fills so it bleeds through like glass.
        super::theme::paint_ambient(
            &ctx.layer_painter(egui::LayerId::background()),
            ctx.screen_rect(),
        );

        // ── Left sidebar (Kimi-style) ────────────────────────────────
        // Collapsed rail: 48px controls + the frame's margins each side.
        const RAIL_BTN: f32 = 48.0;
        let sidebar_w = if self.sidebar_open {
            240.0
        } else {
            RAIL_BTN + 18.0
        };

        egui::SidePanel::left("nav_sidebar")
            .exact_width(sidebar_w)
            .resizable(false)
            .frame(egui::Frame::new()
                // Translucent rail: the ambient gradient glows through.
                .fill(super::theme::glass_fill(sidebar_bg, 208))
                .inner_margin(egui::Margin::symmetric(8, 12))
                .stroke(egui::Stroke::NONE))
            .show(ctx, |ui| {
                // Glass finish: right-edge hairline + top specular highlight.
                let rail_rect = ui.max_rect();
                ui.painter().vline(
                    rail_rect.right() - 0.5,
                    rail_rect.y_range(),
                    super::theme::hairline_stroke(),
                );
                ui.painter().hline(
                    rail_rect.x_range(),
                    rail_rect.top() + 0.5,
                    super::theme::specular_stroke(),
                );
                ui.set_min_width(sidebar_w - 16.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                // Expanded, these are real labelled buttons and need padding.
                // Collapsed, they are drawn by `rail_icon_button`, which centres
                // the glyph itself and ignores padding entirely.
                if self.sidebar_open {
                    ui.spacing_mut().button_padding = egui::vec2(12.0, 8.0);
                }

                // ── Header: Logo icon + text + collapse toggle ──
                // Pre-load sidebar logo texture
                let sidebar_logo_id = self.get_sidebar_logo(ctx).map(|t| t.id());
                ui.horizontal(|ui| {
                    if self.sidebar_open {
                        // "Andrew" in dark + "OS" in accent bold (matching template)
                        ui.label(egui::RichText::new("Andrew").size(19.0).strong().color(text_dark));
                        ui.add_space(-6.0);
                        ui.label(egui::RichText::new("OS").size(19.0).strong().color(accent));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let icon = if self.sidebar_open { "\u{2039}" } else { "\u{2630}" }; // ‹ or ☰
                        let resp = rail_icon_button(
                            ui,
                            icon,
                            RAIL_BTN,
                            20.0,
                            egui::Color32::TRANSPARENT,
                            super::theme::hover_color(),
                            egui::Stroke::new(1.0, super::theme::border_color()),
                            text_dim,
                        );
                        if resp.on_hover_text(if self.sidebar_open { "Collapse sidebar" } else { "Expand sidebar" }).clicked() {
                            self.sidebar_open = !self.sidebar_open;
                        }
                    });
                });

                ui.add_space(8.0);

                // ── New Chat button — the rail's one filled control ──
                {
                    let (label, icon) = if self.sidebar_open { ("+  New chat", "") } else { ("+", "New chat") };
                    // White text washes out on the teal accent; readable_on
                    // picks a contrasting ink and tracks re-theming.
                    let ink = super::theme::readable_on(accent);
                    let resp = if self.sidebar_open {
                        ui.add(
                            egui::Button::new(egui::RichText::new(label).size(15.0).strong().color(ink))
                                .fill(accent)
                                .stroke(egui::Stroke::NONE)
                                .corner_radius(super::theme::RADIUS_BUTTON)
                                .min_size(egui::vec2(ui.available_width(), RAIL_BTN)),
                        )
                    } else {
                        rail_icon_button(
                            ui,
                            "+",
                            RAIL_BTN,
                            24.0,
                            accent,
                            super::theme::accent_color().gamma_multiply(0.85),
                            egui::Stroke::NONE,
                            ink,
                        )
                    };
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

                // ── Navigation items (clean line-icon style matching template) ──
                let nav_items: &[(Tab, &str, &str)] = &[
                    (Tab::Chat,         "\u{1F4AC}", "Chat"),         // 💬 speech bubble
                    (Tab::Projects,     "\u{1F4C2}", "Projects"),     // 📂 open folder
                    (Tab::Agents,       "\u{1F578}", "Agent Swarm"),  // 🕸 spider web = network
                    (Tab::Files,        "\u{1F4C4}", "Files"),        // 📄 document
                    (Tab::Tasks,        "\u{2611}",  "Tasks"),        // ☑ ballot box with check
                    (Tab::Skills,       "\u{2728}",  "Skills"),       // ✨ sparkles
                    (Tab::Terminal,     "\u{1F4BB}", "Terminal"),     // 💻 computer
                    (Tab::RemoteServer, "\u{1F4E1}", "Remote"),       // 📡 satellite antenna
                    (Tab::Console,      "\u{1F4CB}", "VM Log"),       // 📋 clipboard
                    (Tab::Folders,      "\u{1F517}", "Shared"),       // 🔗 share/link
                ];

                for &(tab, icon, label) in nav_items {
                    let is_active = self.selected_tab == tab;
                    // Active state is an accent tint, not a solid card.
                    let text_color = if is_active { accent } else { text_dim };
                    let bg = if is_active {
                        egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 38)
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let stroke = if is_active {
                        egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_unmultiplied(
                                accent.r(), accent.g(), accent.b(), 90,
                            ),
                        )
                    } else {
                        egui::Stroke::NONE
                    };

                    let btn_text = if self.sidebar_open {
                        format!("{}  {}", icon, label)
                    } else {
                        icon.to_string()
                    };
                    let font_weight = if self.sidebar_open { 14.5 } else { 18.0 };
                    let rich = if is_active {
                        egui::RichText::new(&btn_text).size(font_weight).strong().color(text_color)
                    } else {
                        egui::RichText::new(&btn_text).size(font_weight).color(text_color)
                    };
                    let resp = if self.sidebar_open {
                        ui.add(
                            egui::Button::new(rich)
                                .fill(bg)
                                .stroke(stroke)
                                .corner_radius(super::theme::RADIUS_BUTTON)
                                .min_size(egui::vec2(ui.available_width(), RAIL_BTN)),
                        )
                    } else {
                        let hover_bg = if is_active { bg } else { super::theme::hover_color() };
                        rail_icon_button(
                            ui, icon, RAIL_BTN, 18.0, bg, hover_bg, stroke, text_color,
                        )
                    };
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

                    // Template-style uppercase section header
                    let header_btn = egui::Button::new(
                        egui::RichText::new("CHAT HISTORY").size(11.0).strong().color(
                            crate::ui::theme::text_secondary_color() // ink-faint
                        ),
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
                                    let display = if title.len() > 28 {
                                        format!("{}...", &title[..title.char_indices().nth(28).map(|(i,_)|i).unwrap_or(title.len())])
                                    } else {
                                        title.clone()
                                    };
                                    let label_text = if *streaming {
                                        format!("\u{1F7E1} {}", display)
                                    } else {
                                        display.clone()
                                    };
                                    // Active = accent ink + bold, inactive = dim
                                    let item_color = if is_sel {
                                        crate::ui::theme::accent_color()
                                    } else {
                                        text_dim
                                    };
                                    let rich = if is_sel {
                                        egui::RichText::new(&label_text).size(13.0).strong().color(item_color)
                                    } else {
                                        egui::RichText::new(&label_text).size(13.0).color(item_color)
                                    };
                                    let btn = egui::Button::new(rich)
                                    .fill(egui::Color32::TRANSPARENT)
                                    .corner_radius(9.0)
                                    .min_size(egui::vec2(ui.available_width(), 28.0));
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

                // Run on: Local / Remote toggle
                if self.sidebar_open {
                    self.remote_view.ensure_instances_loaded(&self.runtime);
                    let instances = self.remote_view.get_instances();
                    if !instances.is_empty() {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let local_color = if !self.remote_mode {
                                crate::ui::theme::accent_color()
                            } else {
                                text_dim
                            };
                            let remote_color = if self.remote_mode {
                                egui::Color32::from_rgb(168, 85, 247)
                            } else {
                                text_dim
                            };

                            if ui.add(egui::Button::new(
                                egui::RichText::new("Local").size(10.0).color(local_color))
                                .fill(if !self.remote_mode { crate::ui::theme::accent_color().gamma_multiply(0.12) } else { egui::Color32::TRANSPARENT })
                                .corner_radius(4.0)
                                .min_size(egui::vec2(0.0, 20.0))
                            ).clicked() {
                                self.remote_mode = false;
                                crate::server::data::set_remote_backend(None);
                            }

                            if ui.add(egui::Button::new(
                                egui::RichText::new("Remote").size(10.0).color(remote_color))
                                .fill(if self.remote_mode { egui::Color32::from_rgb(168, 85, 247).gamma_multiply(0.12) } else { egui::Color32::TRANSPARENT })
                                .corner_radius(4.0)
                                .min_size(egui::vec2(0.0, 20.0))
                            ).clicked() {
                                if let Some(inst) = instances.first() {
                                    let mut url = inst.url.clone();
                                    if !url.starts_with("http://") && !url.starts_with("https://") {
                                        url = format!("http://{}", url);
                                    }
                                    self.remote_mode = true;
                                    crate::server::data::set_remote_backend(Some(
                                        crate::server::data::RemoteBackend {
                                            url,
                                            token: inst.token.clone(),
                                        }
                                    ));
                                }
                            }
                        });
                        ui.add_space(2.0);
                    }
                }

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

        self.settings_view
            .show(ctx, &self.vm_manager, &self.runtime);
        self.setup_view.show(ctx, &self.vm_manager, &self.runtime);

        // Central panel - main content. Transparent fill: the ambient gradient
        // painted on the background layer is the visible backdrop.
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::same(0)),
            )
            .show(ctx, |ui| {
                // Handle navigation from Projects -> Chat
                if let Some((project_id, session_id)) =
                    self.projects_view.navigate_to_chat_session.take()
                {
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
                        let effective_state = if self.remote_mode {
                            VmState::Running
                        } else {
                            snap.state
                        };
                        self.terminal_view.show(ui, &self.runtime, effective_state);
                    }
                    Tab::RemoteServer => self.remote_view.show(ui, &self.runtime),
                    Tab::Console => {
                        console_view(ui, &snap.console_output, &self.vm_manager, &self.runtime)
                    }
                    Tab::Folders => shared_folders_view(
                        ui,
                        &snap.shared_folders,
                        &self.vm_manager,
                        &self.runtime,
                    ),
                }
            });
    }
}
