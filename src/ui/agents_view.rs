use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, CornerRadius, Stroke, StrokeKind, Vec2};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Data types for the graph editor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct AgentNode {
    id: String,
    name: String,
    role: String,
    persona: String,
    responsibilities: Vec<String>,
    bus_enabled: bool,
    bus_topics: Vec<String>,
    mesh_enabled: bool,
    // Position on canvas
    pos: Pos2,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct Connection {
    from: String,
    to: String,
    label: String,
    protocol: String,
    topics: Vec<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct AgentSystemFile {
    filename: String,
    name: String,
    agent_count: usize,
}

// ---------------------------------------------------------------------------
// AgentsView
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct AgentsView {
    // File list
    agent_files: Vec<AgentSystemFile>,
    selected_file: Option<String>,
    needs_refresh: bool,

    // Graph data
    nodes: Vec<AgentNode>,
    connections: Vec<Connection>,
    system_name: String,
    orchestration_mode: String,

    // Interaction state
    selected_node_idx: Option<usize>,
    dragging_node_idx: Option<usize>,
    drag_offset: Vec2,
    canvas_offset: Vec2,

    // Connection drawing & selection
    connecting_from: Option<usize>,
    selected_connection_idx: Option<usize>,

    // Auto Architecture
    auto_arch_description: String,
    auto_arch_type: String,
    auto_arch_count: String,
    auto_arch_status: Option<(String, bool)>, // (message, is_error)
    auto_arch_loading: bool,

    // YAML editor
    show_yaml_editor: bool,
    yaml_content: String,

    // New node dialog
    show_add_node: bool,
    new_node_name: String,
    new_node_role: String,

    // Save status
    save_status: Option<(String, bool)>,

    // Async result from auto architecture
    auto_arch_result: Arc<Mutex<Option<Result<Value, String>>>>,

    // Live agent statuses from realtime session (agent_id -> status)
    agent_statuses: Arc<Mutex<std::collections::HashMap<String, String>>>,
    status_poll_timer: std::time::Instant,
}

impl Default for AgentsView {
    fn default() -> Self {
        Self {
            agent_files: Vec::new(),
            selected_file: None,
            needs_refresh: true,
            nodes: Vec::new(),
            connections: Vec::new(),
            system_name: "New Agent System".to_string(),
            orchestration_mode: "hierarchical".to_string(),
            selected_node_idx: None,
            dragging_node_idx: None,
            drag_offset: Vec2::ZERO,
            canvas_offset: Vec2::ZERO,
            connecting_from: None,
            selected_connection_idx: None,
            auto_arch_description: String::new(),
            auto_arch_type: "hierarchical".to_string(),
            auto_arch_count: "auto".to_string(),
            auto_arch_status: None,
            auto_arch_loading: false,
            show_yaml_editor: false,
            yaml_content: String::new(),
            show_add_node: false,
            new_node_name: String::new(),
            new_node_role: "worker".to_string(),
            save_status: None,
            auto_arch_result: Arc::new(Mutex::new(None)),
            agent_statuses: Arc::new(Mutex::new(std::collections::HashMap::new())),
            status_poll_timer: std::time::Instant::now(),
        }
    }
}

/// Distance from point `p` to line segment `a`-`b`.
fn point_to_segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let ap = p - a;
    let len_sq = ab.length_sq();
    if len_sq < 1e-6 {
        return ap.length();
    }
    let t = (ap.x * ab.x + ap.y * ab.y) / len_sq;
    let t = t.clamp(0.0, 1.0);
    let proj = Pos2::new(a.x + t * ab.x, a.y + t * ab.y);
    (p - proj).length()
}

impl AgentsView {
    pub fn show(&mut self, ui: &mut egui::Ui, rt: &tokio::runtime::Handle) {
        // Check if fully_auto created an architecture — auto-load it
        if let Some(filename) = crate::server::services::toolbox::take_pending_arch_file() {
            self.load_agent_file(&filename, rt);
            self.needs_refresh = true;
        }

        // Poll live agent statuses every 500ms
        if self.status_poll_timer.elapsed() > std::time::Duration::from_millis(500) {
            self.status_poll_timer = std::time::Instant::now();
            let statuses_arc = self.agent_statuses.clone();
            let ctx = ui.ctx().clone();
            rt.spawn(async move {
                let statuses = crate::server::services::toolbox::get_all_agent_statuses().await;
                if !statuses.is_empty() {
                    *statuses_arc.lock().unwrap() = statuses;
                    ctx.request_repaint();
                }
            });
        }

        // Poll async auto-architecture result
        if self.auto_arch_loading {
            let result = self.auto_arch_result.lock().unwrap().take();
            if let Some(result) = result {
                self.auto_arch_loading = false;
                match result {
                    Ok(system_val) => {
                        self.load_from_value(&system_val);
                        self.auto_arch_status =
                            Some((format!("Generated {} agents!", self.nodes.len()), false));
                    }
                    Err(e) => {
                        self.auto_arch_status = Some((e, true));
                    }
                }
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        }

        // Refresh file list
        if self.needs_refresh {
            self.needs_refresh = false;
            if let Ok(files) = rt.block_on(load_agent_files()) {
                self.agent_files = files;
            }
        }

        // Kimi-style light background
        ui.painter().rect_filled(
            ui.available_rect_before_wrap(),
            0.0,
            Color32::WHITE,
        );

        let text_normal = Color32::from_rgb(52, 48, 42);
        let text_dim = Color32::from_rgb(124, 115, 104);
        let _accent = Color32::from_rgb(18, 154, 145);

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Agent Swarm").size(18.0).strong().color(text_normal));
            ui.add_space(16.0);
            if ui
                .add(
                    egui::Button::new(RichText::new("+ New System").size(12.0).color(text_normal))
                        .corner_radius(6.0),
                )
                .clicked()
            {
                self.nodes.clear();
                self.connections.clear();
                self.system_name = "New Agent System".to_string();
                self.orchestration_mode = "hierarchical".to_string();
                self.selected_file = None;
                self.selected_node_idx = None;
                self.nodes.push(AgentNode {
                    id: "human".to_string(),
                    name: "Human User".to_string(),
                    role: "human".to_string(),
                    persona: "The end user who submits requests.".to_string(),
                    responsibilities: vec!["Provide instructions".to_string()],
                    bus_enabled: false,
                    bus_topics: vec![],
                    mesh_enabled: false,
                    pos: Pos2::new(100.0, 200.0),
                });
            }
            if ui
                .add(
                    egui::Button::new(RichText::new("YAML").size(12.0).color(text_dim))
                        .corner_radius(6.0),
                )
                .clicked()
            {
                self.yaml_content = self.generate_yaml();
                self.show_yaml_editor = !self.show_yaml_editor;
            }
        });

        ui.add_space(4.0);

        // Main layout: sidebar | canvas | properties
        let available = ui.available_size();

        ui.horizontal_top(|ui| {
            // --- Left sidebar ---
            let sidebar_w = 180.0_f32;
            ui.vertical(|ui| {
                ui.set_width(sidebar_w);
                ui.set_max_width(sidebar_w);
                ui.set_min_height(available.y - 10.0);
                let sidebar_rect = egui::Rect::from_min_size(ui.min_rect().min, egui::Vec2::new(sidebar_w, available.y - 10.0));
                ui.painter().rect_filled(sidebar_rect, 0.0, Color32::from_rgb(244, 238, 229));
                ui.add_space(4.0);

                // File list header
                ui.label(RichText::new("  Agent Systems").size(12.0).strong().color(text_normal));
                ui.add_space(2.0);

                egui::ScrollArea::vertical()
                    .id_salt("agent_files_list")
                    .max_height(160.0)
                    .show(ui, |ui| {
                        let files = self.agent_files.clone();
                        let mut load_file: Option<String> = None;
                        for f in &files {
                            let selected = self.selected_file.as_deref() == Some(&f.filename);
                            let mut label = format!("{} ({})", f.name, f.agent_count);
                            if label.len() > 28 {
                                label = format!("{}... ({})", &f.name[..24.min(f.name.len())], f.agent_count);
                            }
                            if ui.selectable_label(selected, RichText::new(&label).size(11.0)).clicked() {
                                load_file = Some(f.filename.clone());
                            }
                        }
                        if let Some(fname) = load_file {
                            self.load_agent_file(&fname, rt);
                        }
                        if self.agent_files.is_empty() {
                            ui.label(RichText::new("  No agent systems yet").size(11.0).color(text_dim));
                        }
                    });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // Auto Architecture
                ui.label(RichText::new("  Auto Architecture").size(12.0).strong().color(text_normal));
                ui.add_space(2.0);
                ui.label(RichText::new("  Describe your system:").size(11.0).color(text_dim));
                ui.add(
                    egui::TextEdit::multiline(&mut self.auto_arch_description)
                        .desired_rows(3)
                        .desired_width(168.0)
                        .hint_text("e.g., A web research team..."),
                );

                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Type:").size(11.0).color(text_dim));
                    egui::ComboBox::from_id_salt("arch_type")
                        .width(85.0)
                        .selected_text(&self.auto_arch_type)
                        .show_ui(ui, |ui| {
                            for t in &["hierarchical", "flat", "mesh", "hybrid", "pipeline", "p2p"] {
                                ui.selectable_value(&mut self.auto_arch_type, t.to_string(), *t);
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Agents:").size(11.0).color(text_dim));
                    egui::ComboBox::from_id_salt("arch_count")
                        .width(65.0)
                        .selected_text(&self.auto_arch_count)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.auto_arch_count, "auto".to_string(), "Auto");
                            for n in 3..=8 {
                                ui.selectable_value(&mut self.auto_arch_count, n.to_string(), format!("{}", n));
                            }
                        });
                });

                ui.add_space(4.0);
                let gen_enabled = !self.auto_arch_loading && !self.auto_arch_description.is_empty();
                if ui.add_enabled(gen_enabled,
                    egui::Button::new(RichText::new(if self.auto_arch_loading { "Generating..." } else { "Generate" }).size(12.0).color(Color32::WHITE))
                        .fill(Color32::from_rgb(18, 154, 145)).corner_radius(6.0),
                ).clicked() {
                    self.run_auto_architecture(rt);
                }

                if let Some((msg, is_err)) = &self.auto_arch_status {
                    let color = if *is_err { Color32::from_rgb(220, 50, 50) } else { Color32::from_rgb(50, 180, 50) };
                    ui.label(RichText::new(msg).size(10.0).color(color));
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // Add agent + Save + Apply
                if ui.add(egui::Button::new(RichText::new("+ Add Agent").size(12.0).color(text_normal)).corner_radius(6.0)).clicked() {
                    self.show_add_node = true;
                    self.new_node_name.clear();
                    self.new_node_role = "worker".to_string();
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Save").size(11.0).color(Color32::WHITE)).fill(Color32::from_rgb(34, 197, 94)).corner_radius(6.0)).clicked() {
                        self.save_current_system(rt);
                    }
                    let settings = rt.block_on(crate::server::data::get_settings());
                    let current_mode = settings.sub_agent_mode.clone().unwrap_or_default();
                    let is_manual = current_mode == "manual";
                    let fill = if is_manual { Color32::from_rgb(18, 154, 145) } else { Color32::from_rgb(180, 180, 185) };
                    let apply_resp = ui.add_enabled(is_manual,
                        egui::Button::new(RichText::new("Apply to Chat").size(11.0).color(Color32::WHITE)).fill(fill).corner_radius(6.0),
                    );
                    if !is_manual { apply_resp.clone().on_disabled_hover_text("Only available in Manual mode"); }
                    if apply_resp.clicked() {
                        if let Some(ref filename) = self.selected_file {
                            let mut s = settings.clone();
                            s.sub_agent_enabled = Some(true);
                            s.sub_agent_config_file = Some(filename.clone());
                            rt.block_on(crate::server::data::save_settings(&s));
                            self.save_status = Some((format!("Applied: {}", filename), false));
                        } else {
                            self.save_status = Some(("Save the system first".to_string(), true));
                        }
                    }
                });
                if let Some((msg, is_err)) = &self.save_status {
                    let color = if *is_err { Color32::from_rgb(220, 50, 50) } else { Color32::from_rgb(50, 180, 50) };
                    ui.label(RichText::new(msg).size(10.0).color(color));
                }
            });

            ui.separator();

            // --- Center: Graph Canvas (full remaining width) ---
            let canvas_width = available.x - sidebar_w - 20.0;
            let canvas_height = available.y - 40.0;

            ui.vertical(|ui| {
                ui.set_width(canvas_width.max(300.0));

                // System name bar
                ui.horizontal(|ui| {
                    ui.label(RichText::new("System:").size(12.0).color(text_dim));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.system_name)
                            .font(egui::FontId::proportional(12.0)),
                    );
                    ui.label(RichText::new("Mode:").size(12.0).color(text_dim));
                    egui::ComboBox::from_id_salt("orch_mode")
                        .width(100.0)
                        .selected_text(&self.orchestration_mode)
                        .show_ui(ui, |ui| {
                            for m in &[
                                "hierarchical", "flat", "mesh", "hybrid", "pipeline", "p2p",
                            ] {
                                ui.selectable_value(&mut self.orchestration_mode, m.to_string(), *m);
                            }
                        });
                    if self.connecting_from.is_some() {
                        ui.label(
                            RichText::new("Click target node to connect (Esc to cancel)")
                                .size(11.0)
                                .color(Color32::from_rgb(18, 154, 145)),
                        );
                    }
                });

                // Canvas
                let (response, painter) = ui.allocate_painter(
                    Vec2::new(canvas_width.max(300.0), canvas_height.max(200.0)),
                    egui::Sense::click_and_drag(),
                );

                let canvas_rect = response.rect;

                // Background — light canvas
                painter.rect_filled(
                    canvas_rect,
                    CornerRadius::same(8),
                    Color32::from_rgb(244, 238, 229),
                );
                painter.rect_stroke(
                    canvas_rect,
                    CornerRadius::same(8),
                    Stroke::new(1.0, Color32::from_rgb(230, 220, 204)),
                    StrokeKind::Outside,
                );

                // Grid — subtle dots pattern
                let grid_size = 24.0;
                let grid_color = Color32::from_rgba_premultiplied(200, 192, 178, 50);
                let min = canvas_rect.min;
                let max = canvas_rect.max;
                let mut x = min.x;
                while x < max.x {
                    painter.line_segment(
                        [Pos2::new(x, min.y), Pos2::new(x, max.y)],
                        Stroke::new(0.5, grid_color),
                    );
                    x += grid_size;
                }
                let mut y = min.y;
                while y < max.y {
                    painter.line_segment(
                        [Pos2::new(min.x, y), Pos2::new(max.x, y)],
                        Stroke::new(0.5, grid_color),
                    );
                    y += grid_size;
                }

                // Draw connections + hit-test for click
                let mut clicked_conn: Option<usize> = None;
                let click_pointer = if response.clicked() {
                    response.interact_pointer_pos()
                } else {
                    None
                };

                for (ci, conn) in self.connections.iter().enumerate() {
                    let from_pos = self
                        .nodes
                        .iter()
                        .find(|n| n.id == conn.from)
                        .map(|n| canvas_rect.min + n.pos.to_vec2());
                    let to_pos = self
                        .nodes
                        .iter()
                        .find(|n| n.id == conn.to)
                        .map(|n| canvas_rect.min + n.pos.to_vec2());

                    if let (Some(from), Some(to)) = (from_pos, to_pos) {
                        let is_selected_conn = self.selected_connection_idx == Some(ci);
                        let color = match conn.protocol.as_str() {
                            "tcp" => Color32::from_rgb(18, 154, 145),
                            "queue" => Color32::from_rgb(245, 158, 11),
                            "bus" => Color32::from_rgb(168, 85, 247),
                            "blackboard" => Color32::from_rgb(34, 197, 94),
                            _ => Color32::GRAY,
                        };
                        let line_width = if is_selected_conn { 4.0 } else { 2.0 };

                        // Draw line
                        painter.line_segment([from, to], Stroke::new(line_width, color));

                        // Arrowhead
                        let dir = (to - from).normalized();
                        let arrow_len = 10.0;
                        let perp = Vec2::new(-dir.y, dir.x) * 5.0;
                        let tip = to - dir * 40.0; // stop before node center
                        painter.add(egui::Shape::convex_polygon(
                            vec![tip, tip - dir * arrow_len + perp, tip - dir * arrow_len - perp],
                            color,
                            Stroke::NONE,
                        ));

                        // Label
                        let mid = Pos2::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
                        let label = format!("{} ({})", conn.label, conn.protocol);
                        painter.text(
                            mid + Vec2::new(0.0, -10.0),
                            egui::Align2::CENTER_BOTTOM,
                            &label,
                            FontId::proportional(10.0),
                            if is_selected_conn {
                                Color32::from_rgb(52, 48, 42)
                            } else {
                                Color32::from_rgb(168, 158, 144)
                            },
                        );

                        // Hit-test: check if click is near this line segment
                        if let Some(ptr) = click_pointer {
                            let dist = point_to_segment_distance(ptr, from, to);
                            if dist < 12.0 {
                                clicked_conn = Some(ci);
                            }
                        }
                    }
                }

                // Draw connection-in-progress line
                if let Some(from_idx) = self.connecting_from {
                    if let Some(node) = self.nodes.get(from_idx) {
                        let from = canvas_rect.min + node.pos.to_vec2();
                        if let Some(pointer) = ui.ctx().pointer_latest_pos() {
                            painter.line_segment(
                                [from, pointer],
                                Stroke::new(2.0, Color32::from_rgb(18, 154, 145).linear_multiply(0.6)),
                            );
                        }
                        ui.ctx().request_repaint();
                    }
                }

                // Draw nodes
                let node_size = Vec2::new(140.0, 60.0);
                let mut clicked_node = None;

                for (idx, node) in self.nodes.iter().enumerate() {
                    let center = canvas_rect.min + node.pos.to_vec2();
                    let node_rect = Rect::from_center_size(center, node_size);

                    if !canvas_rect.intersects(node_rect) {
                        continue;
                    }

                    let role_color = role_color(&node.role);
                    let is_selected = self.selected_node_idx == Some(idx);
                    let is_hovered = ui.ctx().pointer_latest_pos()
                        .map(|p| node_rect.contains(p))
                        .unwrap_or(false);

                    // Hover shadow/glow
                    if is_hovered && !is_selected {
                        painter.rect_filled(
                            node_rect.expand(4.0),
                            CornerRadius::same(12),
                            Color32::from_rgba_premultiplied(18, 154, 145, 30),
                        );
                        painter.rect_stroke(
                            node_rect.expand(2.0),
                            CornerRadius::same(10),
                            Stroke::new(1.5, Color32::from_rgba_premultiplied(18, 154, 145, 100)),
                            StrokeKind::Outside,
                        );
                    }

                    // Node background
                    painter.rect_filled(
                        node_rect,
                        CornerRadius::same(10),
                        role_color,
                    );

                    // Selection border
                    if is_selected {
                        painter.rect_stroke(
                            node_rect.expand(2.0),
                            CornerRadius::same(12),
                            Stroke::new(3.0, Color32::from_rgb(18, 154, 145)),
                            StrokeKind::Outside,
                        );
                    }

                    // Name
                    painter.text(
                        center + Vec2::new(0.0, -8.0),
                        egui::Align2::CENTER_CENTER,
                        &node.name,
                        FontId::proportional(13.0),
                        Color32::WHITE,
                    );

                    // Role badge
                    painter.text(
                        center + Vec2::new(0.0, 12.0),
                        egui::Align2::CENTER_CENTER,
                        &node.role,
                        FontId::proportional(10.0),
                        Color32::from_rgb(230, 220, 204),
                    );

                    // Live status indicator (working = pulsing green, idle = gray dot)
                    {
                        let statuses = self.agent_statuses.lock().unwrap();
                        if let Some(status) = statuses.get(&node.id) {
                            let dot_center = Pos2::new(node_rect.left() + 8.0, node_rect.top() + 8.0);
                            let (dot_color, label) = match status.as_str() {
                                "working" => (Color32::from_rgb(34, 197, 94), "working"),
                                _ => (Color32::from_rgb(120, 120, 120), "idle"),
                            };
                            painter.circle_filled(dot_center, 4.0, dot_color);
                            if status == "working" {
                                // Pulsing ring for working agents
                                painter.circle_stroke(
                                    dot_center, 7.0,
                                    Stroke::new(1.5, Color32::from_rgba_premultiplied(34, 197, 94, 120)),
                                    // StrokeKind not needed for circle_stroke
                                );
                            }
                            painter.text(
                                Pos2::new(node_rect.left() + 16.0, node_rect.top() + 3.0),
                                egui::Align2::LEFT_TOP,
                                label,
                                FontId::proportional(8.0),
                                dot_color,
                            );
                        }
                    }

                    // Bus/Mesh indicators
                    if node.bus_enabled {
                        painter.text(
                            Pos2::new(node_rect.right() - 4.0, node_rect.top() + 4.0),
                            egui::Align2::RIGHT_TOP,
                            "B",
                            FontId::proportional(9.0),
                            Color32::from_rgb(168, 85, 247),
                        );
                    }
                    if node.mesh_enabled {
                        painter.text(
                            Pos2::new(node_rect.right() - 14.0, node_rect.top() + 4.0),
                            egui::Align2::RIGHT_TOP,
                            "M",
                            FontId::proportional(9.0),
                            Color32::from_rgb(34, 211, 238),
                        );
                    }

                    // Hover tooltip — paint a card next to the node
                    if is_hovered {
                        let tip_x = node_rect.right() + 8.0;
                        let tip_y = node_rect.min.y;
                        let tip_w = 200.0_f32;
                        let persona_preview = if node.persona.len() > 60 {
                            format!("{}...", &node.persona[..60])
                        } else {
                            node.persona.clone()
                        };
                        let line_count = 2 + if persona_preview.is_empty() { 0 } else { 1 } + if node.bus_enabled { 1 } else { 0 };
                        let tip_h = line_count as f32 * 16.0 + 16.0;
                        let tip_rect = Rect::from_min_size(Pos2::new(tip_x, tip_y), Vec2::new(tip_w, tip_h));

                        // Shadow
                        painter.rect_filled(tip_rect.expand(2.0), CornerRadius::same(10), Color32::from_rgba_premultiplied(0, 0, 0, 20));
                        // Card background
                        painter.rect_filled(tip_rect, CornerRadius::same(8), Color32::WHITE);
                        painter.rect_stroke(tip_rect, CornerRadius::same(8), Stroke::new(0.5, Color32::from_rgb(230, 220, 204)), StrokeKind::Outside);

                        let mut ty = tip_y + 10.0;
                        painter.text(Pos2::new(tip_x + 10.0, ty), egui::Align2::LEFT_TOP, &node.name, FontId::proportional(12.0), Color32::from_rgb(52, 48, 42));
                        ty += 16.0;
                        painter.text(Pos2::new(tip_x + 10.0, ty), egui::Align2::LEFT_TOP, &format!("Role: {}", node.role), FontId::proportional(10.0), Color32::from_rgb(124, 115, 104));
                        ty += 16.0;
                        if !persona_preview.is_empty() {
                            painter.text(Pos2::new(tip_x + 10.0, ty), egui::Align2::LEFT_TOP, &persona_preview, FontId::proportional(10.0), Color32::from_rgb(168, 158, 144));
                            // ty += 16.0;
                        }

                        ui.ctx().request_repaint();
                    }

                    // Check click/drag on this node
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if node_rect.contains(pointer) {
                                clicked_node = Some(idx);
                            }
                        }
                    }

                    if response.drag_started() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if node_rect.contains(pointer) {
                                self.dragging_node_idx = Some(idx);
                                self.drag_offset = center - pointer;
                            }
                        }
                    }
                }

                // Handle drag
                if let Some(drag_idx) = self.dragging_node_idx {
                    if response.dragged() {
                        if let Some(pointer) = ui.ctx().pointer_latest_pos() {
                            let new_pos = pointer + self.drag_offset - canvas_rect.min.to_vec2();
                            if let Some(node) = self.nodes.get_mut(drag_idx) {
                                node.pos = Pos2::new(new_pos.x, new_pos.y);
                            }
                        }
                    }
                    if response.drag_stopped() {
                        self.dragging_node_idx = None;
                    }
                }

                // Handle node click
                if let Some(idx) = clicked_node {
                    if let Some(from_idx) = self.connecting_from {
                        // Complete connection
                        if from_idx != idx {
                            let from_id = self.nodes[from_idx].id.clone();
                            let to_id = self.nodes[idx].id.clone();
                            self.connections.push(Connection {
                                from: from_id,
                                to: to_id,
                                label: "task".to_string(),
                                protocol: "tcp".to_string(),
                                topics: vec![],
                            });
                        }
                        self.connecting_from = None;
                    } else {
                        self.selected_node_idx = Some(idx);
                        self.selected_connection_idx = None;
                    }
                } else if let Some(ci) = clicked_conn {
                    // Clicked a connection line
                    self.selected_connection_idx = Some(ci);
                    self.selected_node_idx = None;
                } else if response.clicked() {
                    // Clicked empty space
                    if self.connecting_from.is_some() {
                        self.connecting_from = None;
                    } else {
                        self.selected_node_idx = None;
                        self.selected_connection_idx = None;
                    }
                }

                // ESC cancels connection mode
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.connecting_from = None;
                }

                // Delete key removes selected node or connection
                if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
                {
                    if let Some(idx) = self.selected_node_idx {
                        if self.nodes[idx].role != "human" {
                            let id = self.nodes[idx].id.clone();
                            self.connections.retain(|c| c.from != id && c.to != id);
                            self.nodes.remove(idx);
                            self.selected_node_idx = None;
                        }
                    } else if let Some(ci) = self.selected_connection_idx {
                        if ci < self.connections.len() {
                            self.connections.remove(ci);
                            self.selected_connection_idx = None;
                        }
                    }
                }
            });

        });

        // --- Node Properties floating window ---
        if let Some(idx) = self.selected_node_idx {
            if idx < self.nodes.len() {
                let mut open = true;
                egui::Window::new("Node Properties")
                    .open(&mut open)
                    .resizable(true)
                    .default_width(280.0)
                    .default_height(400.0)
                    .anchor(egui::Align2::RIGHT_TOP, [-12.0, 60.0])
                    .show(ui.ctx(), |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                self.show_node_properties(ui, idx);
                            });
                    });
                if !open {
                    self.selected_node_idx = None;
                }
            }
        }

        // --- Connection Properties floating window ---
        if let Some(ci) = self.selected_connection_idx {
            if ci < self.connections.len() {
                let mut open = true;
                egui::Window::new("Connection Properties")
                    .open(&mut open)
                    .resizable(true)
                    .default_width(260.0)
                    .default_height(300.0)
                    .anchor(egui::Align2::RIGHT_TOP, [-12.0, 60.0])
                    .show(ui.ctx(), |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                self.show_connection_properties(ui, ci);
                            });
                    });
                if !open {
                    self.selected_connection_idx = None;
                }
            }
        }

        // --- Add node dialog ---
        if self.show_add_node {
            self.show_add_node_dialog(ui);
        }

        // --- YAML editor overlay ---
        if self.show_yaml_editor {
            self.show_yaml_editor_dialog(ui, rt);
        }
    }

    fn show_node_properties(&mut self, ui: &mut egui::Ui, idx: usize) {
        let text_normal = Color32::from_rgb(52, 48, 42);
        let text_dim = Color32::from_rgb(124, 115, 104);
        ui.label(RichText::new("Node Properties").size(14.0).strong().color(text_normal));
        ui.add_space(4.0);

        let node = &mut self.nodes[idx];

        ui.horizontal(|ui| {
            ui.label("ID:");
            ui.add(egui::TextEdit::singleline(&mut node.id).desired_width(150.0));
        });
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.add(egui::TextEdit::singleline(&mut node.name).desired_width(150.0));
        });

        ui.horizontal(|ui| {
            ui.label("Role:");
            egui::ComboBox::from_id_salt("node_role")
                .width(120.0)
                .selected_text(&node.role)
                .show_ui(ui, |ui| {
                    for r in &[
                        "human",
                        "orchestrator",
                        "worker",
                        "checker",
                        "reporter",
                        "researcher",
                        "peer",
                    ] {
                        ui.selectable_value(&mut node.role, r.to_string(), *r);
                    }
                });
        });

        // Role color preview
        let color = role_color(&node.role);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(220.0, 6.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(3), color);

        ui.add_space(4.0);
        ui.label("Persona:");
        ui.add(
            egui::TextEdit::multiline(&mut node.persona)
                .desired_rows(3)
                .desired_width(220.0),
        );

        ui.add_space(4.0);
        ui.label("Responsibilities:");
        let mut to_remove = None;
        for (i, r) in node.responsibilities.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(r).desired_width(170.0));
                if ui.small_button("x").clicked() {
                    to_remove = Some(i);
                }
            });
        }
        if let Some(i) = to_remove {
            node.responsibilities.remove(i);
        }
        if ui.small_button("+ Add").clicked() {
            node.responsibilities.push(String::new());
        }

        ui.add_space(4.0);
        ui.separator();

        // Communication
        ui.checkbox(&mut node.bus_enabled, "Bus enabled");
        if node.bus_enabled {
            ui.label("Bus topics (comma-sep):");
            let mut topics_str = node.bus_topics.join(", ");
            if ui
                .add(egui::TextEdit::singleline(&mut topics_str).desired_width(210.0))
                .changed()
            {
                node.bus_topics = topics_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        ui.checkbox(&mut node.mesh_enabled, "Mesh enabled");

        ui.add_space(8.0);
        ui.separator();

        // Connection actions
        ui.label(RichText::new("Connections").strong());
        if ui.button("Draw connection from this node").clicked() {
            self.connecting_from = Some(idx);
        }

        // List connections from/to this node
        let node_id = self.nodes[idx].id.clone();
        let mut conn_to_remove = None;
        let mut conn_to_select = None;
        for (ci, conn) in self.connections.iter_mut().enumerate() {
            if conn.from == node_id || conn.to == node_id {
                ui.horizontal(|ui| {
                    let label = format!("{} -> {}", conn.from, conn.to);
                    if ui.small_button(RichText::new(&label).small()).clicked() {
                        conn_to_select = Some(ci);
                    }
                    let proto_color = match conn.protocol.as_str() {
                        "tcp" => Color32::from_rgb(18, 154, 145),
                        "queue" => Color32::from_rgb(245, 158, 11),
                        "bus" => Color32::from_rgb(168, 85, 247),
                        "blackboard" => Color32::from_rgb(34, 197, 94),
                        _ => Color32::GRAY,
                    };
                    ui.label(RichText::new(format!("[{}]", conn.protocol)).small().color(proto_color));
                    if ui.small_button("x").clicked() {
                        conn_to_remove = Some(ci);
                    }
                });
            }
        }
        if let Some(ci) = conn_to_remove {
            self.connections.remove(ci);
        }
        if let Some(ci) = conn_to_select {
            self.selected_connection_idx = Some(ci);
            self.selected_node_idx = None;
        }
        ui.add_space(4.0);
        if ui.small_button("Delete this node").clicked() && node_id != "human" {
            self.connections
                .retain(|c| c.from != node_id && c.to != node_id);
            self.nodes.remove(idx);
            self.selected_node_idx = None;
        }
    }

    fn show_connection_properties(&mut self, ui: &mut egui::Ui, ci: usize) {
        let text_normal = Color32::from_rgb(52, 48, 42);
        let _text_dim = Color32::from_rgb(124, 115, 104);
        ui.label(RichText::new("Connection Properties").size(14.0).strong().color(text_normal));
        ui.add_space(4.0);

        let conn = &mut self.connections[ci];

        // From / To (read-only)
        egui::Grid::new("conn_props_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("From:");
                ui.label(RichText::new(&conn.from).monospace());
                ui.end_row();

                ui.label("To:");
                ui.label(RichText::new(&conn.to).monospace());
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        // Protocol selector
        ui.label(RichText::new("Protocol:").strong());
        let protocol_color = match conn.protocol.as_str() {
            "tcp" => Color32::from_rgb(18, 154, 145),
            "queue" => Color32::from_rgb(245, 158, 11),
            "bus" => Color32::from_rgb(168, 85, 247),
            "blackboard" => Color32::from_rgb(34, 197, 94),
            _ => Color32::GRAY,
        };
        egui::ComboBox::from_id_salt(format!("conn_protocol_{}", ci))
            .selected_text(RichText::new(&conn.protocol).color(protocol_color))
            .width(150.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut conn.protocol,
                    "tcp".to_string(),
                    RichText::new("tcp").color(Color32::from_rgb(18, 154, 145)),
                );
                ui.selectable_value(
                    &mut conn.protocol,
                    "queue".to_string(),
                    RichText::new("queue").color(Color32::from_rgb(245, 158, 11)),
                );
                ui.selectable_value(
                    &mut conn.protocol,
                    "bus".to_string(),
                    RichText::new("bus").color(Color32::from_rgb(168, 85, 247)),
                );
                ui.selectable_value(
                    &mut conn.protocol,
                    "blackboard".to_string(),
                    RichText::new("blackboard").color(Color32::from_rgb(34, 197, 94)),
                );
            });

        ui.add_space(8.0);

        // Label
        ui.label(RichText::new("Label:").strong());
        ui.add(egui::TextEdit::singleline(&mut conn.label).desired_width(180.0));

        ui.add_space(8.0);

        // Topics (for bus/queue)
        if conn.protocol == "bus" || conn.protocol == "queue" {
            ui.label(RichText::new("Topics (comma-sep):").strong());
            let mut topics_str = conn.topics.join(", ");
            if ui
                .add(egui::TextEdit::singleline(&mut topics_str).desired_width(180.0))
                .changed()
            {
                conn.topics = topics_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(4.0);

        // Delete connection
        if ui
            .add(
                egui::Button::new(
                    RichText::new("Delete connection")
                        .color(Color32::from_rgb(239, 68, 68)),
                ),
            )
            .clicked()
        {
            self.connections.remove(ci);
            self.selected_connection_idx = None;
        }
    }

    fn show_add_node_dialog(&mut self, ui: &mut egui::Ui) {
        egui::Window::new("Add Agent Node")
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut self.new_node_name);
                });
                ui.horizontal(|ui| {
                    ui.label("Role:");
                    egui::ComboBox::from_id_salt("new_node_role")
                        .selected_text(&self.new_node_role)
                        .show_ui(ui, |ui| {
                            for r in &[
                                "orchestrator",
                                "worker",
                                "checker",
                                "reporter",
                                "researcher",
                                "peer",
                            ] {
                                ui.selectable_value(&mut self.new_node_role, r.to_string(), *r);
                            }
                        });
                });

                ui.horizontal(|ui| {
                    if ui.button("Add").clicked() && !self.new_node_name.is_empty() {
                        let id = self
                            .new_node_name
                            .to_lowercase()
                            .replace(' ', "_")
                            .chars()
                            .filter(|c| c.is_alphanumeric() || *c == '_')
                            .collect::<String>();

                        // Place new node at a reasonable position
                        let x = 200.0 + (self.nodes.len() as f32 * 80.0) % 400.0;
                        let y = 100.0 + (self.nodes.len() as f32 * 60.0) % 300.0;

                        self.nodes.push(AgentNode {
                            id,
                            name: self.new_node_name.clone(),
                            role: self.new_node_role.clone(),
                            persona: String::new(),
                            responsibilities: vec![],
                            bus_enabled: false,
                            bus_topics: vec![],
                            mesh_enabled: false,
                            pos: Pos2::new(x, y),
                        });
                        self.show_add_node = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_add_node = false;
                    }
                });
            });
    }

    fn show_yaml_editor_dialog(&mut self, ui: &mut egui::Ui, _rt: &tokio::runtime::Handle) {
        egui::Window::new("YAML Editor")
            .default_width(600.0)
            .default_height(500.0)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Apply YAML to Graph").clicked() {
                        self.parse_yaml_to_graph();
                    }
                    if ui.button("Refresh from Graph").clicked() {
                        self.yaml_content = self.generate_yaml();
                    }
                    if ui.button("Close").clicked() {
                        self.show_yaml_editor = false;
                    }
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("yaml_editor_scroll")
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.yaml_content)
                                .desired_width(f32::INFINITY)
                                .desired_rows(30)
                                .font(FontId::monospace(12.0)),
                        );
                    });
            });
    }

    // -------------------------------------------------------------------
    // Data operations
    // -------------------------------------------------------------------

    fn load_agent_file(&mut self, filename: &str, rt: &tokio::runtime::Handle) {
        let content = if let Some(rb) = crate::server::data::get_remote_backend() {
            let url = format!("{}/api/agents/{}", rb.url, urlencoding::encode(filename));
            let token = rb.token.clone();
            rt.block_on(async {
                let client = reqwest::Client::new();
                match client.get(&url).bearer_auth(&token).send().await {
                    Ok(resp) => {
                        if let Ok(val) = resp.json::<Value>().await {
                            val.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string()
                        } else {
                            String::new()
                        }
                    }
                    Err(_) => String::new(),
                }
            })
        } else {
            let dir = crate::server::data::data_dir().join("agents");
            let fp = dir.join(filename);
            std::fs::read_to_string(&fp).unwrap_or_default()
        };

        if !content.is_empty() {
            if let Ok(parsed) = serde_yaml::from_str::<Value>(&content) {
                self.load_from_value(&parsed);
                self.selected_file = Some(filename.to_string());
                self.yaml_content = content;
            }
        }
    }

    fn load_from_value(&mut self, val: &Value) {
        self.nodes.clear();
        self.connections.clear();
        self.selected_node_idx = None;

        if let Some(sys) = val.get("system") {
            self.system_name = sys
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unnamed")
                .to_string();
            self.orchestration_mode = sys
                .get("orchestration_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("hierarchical")
                .to_string();
        }

        // Layout: arrange nodes in a circle, ensuring all fit on screen
        let agents = val
            .get("agents")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        let count = agents.len();

        // Node size ~160x60, use padding so nodes don't clip edges
        let node_pad = 100.0;
        let radius = (120.0 + count as f32 * 18.0).min(280.0);
        let center_x = radius + node_pad;
        let center_y = radius + node_pad;

        for (i, agent) in agents.iter().enumerate() {
            let angle = (i as f32 / count.max(1) as f32) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let cx = center_x + angle.cos() * radius;
            let cy = center_y + angle.sin() * radius;

            let bus = agent.get("bus");
            let mesh = agent.get("mesh");

            self.nodes.push(AgentNode {
                id: agent
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                name: agent
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Agent")
                    .to_string(),
                role: agent
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("worker")
                    .to_string(),
                persona: agent
                    .get("persona")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                responsibilities: agent
                    .get("responsibilities")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                bus_enabled: bus
                    .and_then(|b| b.get("enabled"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                bus_topics: bus
                    .and_then(|b| b.get("topics"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                mesh_enabled: mesh
                    .and_then(|m| m.get("enabled"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                pos: Pos2::new(cx, cy),
            });
        }

        // Connections
        if let Some(conns) = val.get("connections").and_then(|c| c.as_array()) {
            for conn in conns {
                self.connections.push(Connection {
                    from: conn
                        .get("from")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    to: conn
                        .get("to")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    label: conn
                        .get("label")
                        .and_then(|v| v.as_str())
                        .unwrap_or("task")
                        .to_string(),
                    protocol: conn
                        .get("protocol")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tcp")
                        .to_string(),
                    topics: conn
                        .get("topics")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
        }
    }

    fn generate_yaml(&self) -> String {
        let mut lines = Vec::new();

        lines.push("system:".to_string());
        lines.push(format!("  name: {}", self.system_name));
        lines.push(format!(
            "  orchestration_mode: {}",
            self.orchestration_mode
        ));
        lines.push("  communication_protocol: structured_handoff".to_string());
        lines.push("  context_passing: full_chain".to_string());

        lines.push("agents:".to_string());
        for node in &self.nodes {
            lines.push(format!("  - id: {}", node.id));
            lines.push(format!("    name: {}", node.name));
            lines.push(format!("    role: {}", node.role));
            lines.push(format!("    persona: >-"));
            lines.push(format!("      {}", node.persona));
            lines.push("    responsibilities:".to_string());
            for r in &node.responsibilities {
                lines.push(format!("      - {}", r));
            }
            if node.bus_enabled {
                lines.push("    bus:".to_string());
                lines.push("      enabled: true".to_string());
                if !node.bus_topics.is_empty() {
                    lines.push("      topics:".to_string());
                    for t in &node.bus_topics {
                        lines.push(format!("        - {}", t));
                    }
                }
            }
            if node.mesh_enabled {
                lines.push("    mesh:".to_string());
                lines.push("      enabled: true".to_string());
            }
        }

        if !self.connections.is_empty() {
            lines.push("connections:".to_string());
            for conn in &self.connections {
                lines.push(format!("  - from: {}", conn.from));
                lines.push(format!("    to: {}", conn.to));
                lines.push(format!("    label: {}", conn.label));
                lines.push(format!("    protocol: {}", conn.protocol));
                if !conn.topics.is_empty() {
                    lines.push("    topics:".to_string());
                    for t in &conn.topics {
                        lines.push(format!("      - {}", t));
                    }
                }
            }
        }

        lines.join("\n") + "\n"
    }

    fn parse_yaml_to_graph(&mut self) {
        if let Ok(val) = serde_yaml::from_str::<Value>(&self.yaml_content) {
            self.load_from_value(&val);
        }
    }

    fn save_current_system(&mut self, rt: &tokio::runtime::Handle) {
        let yaml = self.generate_yaml();
        let filename = self
            .selected_file
            .clone()
            .unwrap_or_else(|| {
                let safe: String = self
                    .system_name
                    .to_lowercase()
                    .replace(' ', "_")
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                format!("{}.yaml", safe)
            });

        if let Some(rb) = crate::server::data::get_remote_backend() {
            let body = serde_json::json!({ "filename": filename, "content": yaml });
            let url = format!("{}/api/agents", rb.url);
            let token = rb.token.clone();
            let ok = rt.block_on(async {
                let client = reqwest::Client::new();
                client.post(&url).bearer_auth(&token).json(&body).send().await.is_ok()
            });
            if ok {
                self.selected_file = Some(filename);
                self.save_status = Some(("Saved!".to_string(), false));
                self.needs_refresh = true;
            } else {
                self.save_status = Some(("Error saving to remote".to_string(), true));
            }
        } else {
            let dir = crate::server::data::data_dir().join("agents");
            let _ = std::fs::create_dir_all(&dir);
            let fp = dir.join(&filename);

            match std::fs::write(&fp, &yaml) {
                Ok(_) => {
                    self.selected_file = Some(filename);
                    self.save_status = Some(("Saved!".to_string(), false));
                    self.needs_refresh = true;
                }
                Err(e) => {
                    self.save_status = Some((format!("Error: {}", e), true));
                }
            }
        }
    }

    fn run_auto_architecture(&mut self, rt: &tokio::runtime::Handle) {
        self.auto_arch_loading = true;
        self.auto_arch_status = Some(("Generating... (may take 10-30s)".to_string(), false));

        let description = self.auto_arch_description.clone();
        let arch_type = self.auto_arch_type.clone();
        let count = self.auto_arch_count.clone();
        let result_slot = self.auto_arch_result.clone();

        // Get settings synchronously (fast local file read)
        let settings = rt.block_on(crate::server::data::get_settings());
        let api_key = settings.tiger_bot_api_key.clone();
        let api_url_raw = settings
            .tiger_bot_api_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1/chat/completions".to_string());
        let api_url = if api_url_raw.ends_with("/chat/completions") {
            api_url_raw
        } else {
            format!("{}/chat/completions", api_url_raw.trim_end_matches('/'))
        };
        let model = if settings.tiger_bot_model.is_empty() {
            "gpt-4o-mini".to_string()
        } else {
            settings.tiger_bot_model.clone()
        };

        if api_key.is_empty() {
            self.auto_arch_loading = false;
            self.auto_arch_status = Some(("API key not configured. Set it in Settings > AI / API".to_string(), true));
            return;
        }

        // Spawn the LLM call as a background task
        rt.spawn(async move {
            let count_instruction = if count == "auto" {
                "Determine the optimal number based on the task".to_string()
            } else {
                count.clone()
            };

            let user_msg = format!(
                r#"Based on this description, generate a complete multi-agent system configuration as a JSON object.

User Request: {description}
Architecture Type: {arch_type}
Number of Agents: {count_instruction}

Return ONLY a valid JSON object (no markdown, no code fences) with this structure:
{{
  "system": {{ "name": "System Name", "orchestration_mode": "{arch_type}", "communication_protocol": "structured_handoff", "context_passing": "full_chain" }},
  "agents": [ {{ "id": "snake_case_id", "name": "Display Name", "role": "human|orchestrator|worker|checker|reporter|researcher|peer", "persona": "2-3 sentence description", "responsibilities": ["resp1", "resp2"], "bus": {{ "enabled": false }}, "mesh": {{ "enabled": false }} }} ],
  "connections": [ {{ "from": "source_id", "to": "target_id", "label": "label", "protocol": "tcp|queue" }} ],
  "workflow": {{ "sequence": [ {{ "step": 1, "agent": "agent_id", "action": "what this agent does", "outputs_to": ["next_agent_id"] }} ] }}
}}

Rules:
- Always include ONE agent with role "human" and id "human"
- Connections use only "tcp" or "queue" protocol
- Every non-human agent needs at least one incoming connection
- For hierarchical: human -> orchestrator -> workers
- For flat: human -> all agents directly
- For mesh: no connections needed (mesh bypasses access control)
- For hybrid: human -> orchestrator -> workers (workers have mesh.enabled: true)
- For pipeline: agents form a LINEAR SEQUENTIAL CHAIN. human -> agent1 -> agent2 -> agent3 -> ... -> final_agent. Each agent connects to exactly ONE next agent. Do NOT use an orchestrator role. Do NOT create star topology. Connections MUST form a strict linear chain. workflow.sequence MUST list agents in order with outputs_to pointing to the next agent. Last agent has outputs_to: [].
- For p2p: all non-human agents use role "peer", no connections
- Agent IDs must be snake_case, 3-8 agents total
- Always include workflow.sequence listing the processing order"#
            );

            let client = reqwest::Client::new();
            let body = json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": "You are an expert multi-agent system architect. Return ONLY valid JSON, nothing else."},
                    {"role": "user", "content": user_msg}
                ],
                "temperature": 0.3,
                "max_tokens": 16384,
            });

            let result = async {
                let mut req = client
                    .post(&api_url)
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Content-Type", "application/json");
                // Kimi Code API requires Claude Code identity headers
                if api_url.contains("api.kimi.com") {
                    req = req
                        .header("User-Agent", "claude-code/1.0.6")
                        .header("X-Client-Name", "claude-code")
                        .header("X-Client-Version", "1.0.6")
                        .header("HTTP-Referer", "https://claude.ai")
                        .header("X-Traffic-Source", "claude-code");
                }
                let resp = req
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| format!("Request failed: {e}"))?;

                let resp_json: Value = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

                if let Some(err) = resp_json.get("error") {
                    return Err(format!("API error: {}", err));
                }

                // Support OpenAI, Anthropic, and reasoning model response formats
                let raw = resp_json["choices"][0]["message"]["content"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        // Reasoning models (Kimi, DeepSeek): content may be empty, actual output in reasoning_content
                        resp_json["choices"][0]["message"]["reasoning_content"].as_str()
                    })
                    .or_else(|| {
                        // Anthropic format: content[0].text
                        resp_json["content"][0]["text"].as_str()
                    })
                    .unwrap_or("")
                    .to_string();

                if raw.is_empty() {
                    return Err(format!(
                        "Empty LLM response. API returned: {}",
                        serde_json::to_string(&resp_json)
                            .unwrap_or_default()
                            .chars()
                            .take(500)
                            .collect::<String>()
                    ));
                }

                // Strip <think> tags
                let mut s = raw;
                while let Some(start) = s.find("<think>") {
                    if let Some(end) = s.find("</think>") {
                        s = format!("{}{}", &s[..start], &s[end + 8..]);
                    } else {
                        break;
                    }
                }

                // Strip markdown code fences (```json ... ``` or ``` ... ```)
                let s = s.trim();
                let s = if s.starts_with("```") {
                    let inner = if let Some(rest) = s.strip_prefix("```json") {
                        rest
                    } else if let Some(rest) = s.strip_prefix("```") {
                        rest
                    } else {
                        s
                    };
                    inner.trim_end_matches("```").trim()
                } else {
                    s
                };

                // Extract JSON object
                let parsed: Value = if let Ok(v) = serde_json::from_str(s) {
                    v
                } else if let (Some(start), Some(end)) = (s.find('{'), s.rfind('}')) {
                    serde_json::from_str(&s[start..=end])
                        .map_err(|e| format!("JSON parse error: {e}"))?
                } else {
                    return Err(format!(
                        "No JSON found in LLM response. Raw: {}",
                        s.chars().take(300).collect::<String>()
                    ));
                };

                if parsed.get("system").is_none() || parsed.get("agents").is_none() {
                    return Err("Invalid structure: missing system or agents".to_string());
                }

                Ok(parsed)
            }
            .await;

            *result_slot.lock().unwrap() = Some(result);
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn role_color(role: &str) -> Color32 {
    match role {
        "human" => Color32::from_rgb(107, 114, 128),    // gray
        "orchestrator" => Color32::from_rgb(18, 154, 145), // blue
        "worker" => Color32::from_rgb(34, 197, 94),      // green
        "checker" => Color32::from_rgb(245, 158, 11),    // amber
        "reporter" => Color32::from_rgb(168, 85, 247),   // purple
        "researcher" => Color32::from_rgb(6, 182, 212),  // cyan
        "peer" => Color32::from_rgb(236, 72, 153),       // pink
        _ => Color32::from_rgb(100, 100, 100),
    }
}

async fn load_agent_files() -> Result<Vec<AgentSystemFile>, String> {
    // Remote mode: GET /api/agents
    if let Some(rb) = crate::server::data::get_remote_backend() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        let resp = client
            .get(format!("{}/api/agents", rb.url))
            .bearer_auth(&rb.token)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let items: Vec<Value> = resp.json().await.unwrap_or_default();
        let mut result = Vec::new();
        for item in &items {
            let filename = item.get("filename").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or(&filename).to_string();
            let agent_count = item.get("agentCount").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if !filename.is_empty() {
                result.push(AgentSystemFile { filename, name, agent_count });
            }
        }
        return Ok(result);
    }

    let dir = crate::server::data::data_dir().join("agents");
    let _ = tokio::fs::create_dir_all(&dir).await;

    let mut result = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| e.to_string())?;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.ends_with(".yaml") || name.ends_with(".yml")) {
            continue;
        }
        let path = dir.join(&name);
        let content = tokio::fs::read_to_string(&path)
            .await
            .unwrap_or_default();
        let parsed: Value = serde_yaml::from_str(&content).unwrap_or(Value::Null);
        let display = parsed
            .get("system")
            .and_then(|s| s.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or(&name)
            .to_string();
        let agent_count = parsed
            .get("agents")
            .and_then(|a| a.as_array())
            .map(|a| a.len())
            .unwrap_or(0);

        result.push(AgentSystemFile {
            filename: name,
            name: display,
            agent_count,
        });
    }

    Ok(result)
}
