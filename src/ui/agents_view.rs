use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Rounding, Stroke, StrokeKind, Vec2};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Data types for the graph editor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
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
struct Connection {
    from: String,
    to: String,
    label: String,
    protocol: String,
    topics: Vec<String>,
}

#[derive(Debug, Clone)]
struct AgentSystemFile {
    filename: String,
    name: String,
    agent_count: usize,
}

// ---------------------------------------------------------------------------
// AgentsView
// ---------------------------------------------------------------------------

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

        ui.horizontal(|ui| {
            ui.heading("Agent System Editor");
            ui.separator();
            if ui.button("New System").clicked() {
                self.nodes.clear();
                self.connections.clear();
                self.system_name = "New Agent System".to_string();
                self.orchestration_mode = "hierarchical".to_string();
                self.selected_file = None;
                self.selected_node_idx = None;
                // Add default human node
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
            if ui.button("YAML").clicked() {
                self.yaml_content = self.generate_yaml();
                self.show_yaml_editor = !self.show_yaml_editor;
            }
        });

        ui.add_space(4.0);

        // Main layout: sidebar | canvas | properties
        let available = ui.available_size();

        ui.horizontal_top(|ui| {
            // --- Left sidebar: file list + auto architecture ---
            ui.vertical(|ui| {
                ui.set_width(200.0);
                ui.set_min_height(available.y - 10.0);

                // File list
                ui.label(RichText::new("Agent Systems").strong());
                egui::ScrollArea::vertical()
                    .id_salt("agent_files_list")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        let files = self.agent_files.clone();
                        for f in &files {
                            let selected = self.selected_file.as_deref() == Some(&f.filename);
                            let label = format!("{} ({})", f.name, f.agent_count);
                            if ui.selectable_label(selected, &label).clicked() {
                                self.load_agent_file(&f.filename, rt);
                            }
                        }
                        if self.agent_files.is_empty() {
                            ui.label(RichText::new("No agent systems yet").weak());
                        }
                    });

                ui.add_space(8.0);
                ui.separator();

                // Auto Architecture section
                ui.label(RichText::new("Auto Architecture").strong());
                ui.add_space(4.0);

                ui.label("Describe your system:");
                ui.add(
                    egui::TextEdit::multiline(&mut self.auto_arch_description)
                        .desired_rows(3)
                        .desired_width(190.0)
                        .hint_text("e.g., A web research team with 3 researchers and a synthesizer"),
                );

                ui.horizontal(|ui| {
                    ui.label("Type:");
                    egui::ComboBox::from_id_salt("arch_type")
                        .width(100.0)
                        .selected_text(&self.auto_arch_type)
                        .show_ui(ui, |ui| {
                            for t in &[
                                "hierarchical",
                                "flat",
                                "mesh",
                                "hybrid",
                                "pipeline",
                                "p2p",
                            ] {
                                ui.selectable_value(
                                    &mut self.auto_arch_type,
                                    t.to_string(),
                                    *t,
                                );
                            }
                        });
                });

                ui.horizontal(|ui| {
                    ui.label("Agents:");
                    egui::ComboBox::from_id_salt("arch_count")
                        .width(80.0)
                        .selected_text(&self.auto_arch_count)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.auto_arch_count,
                                "auto".to_string(),
                                "Auto",
                            );
                            for n in 3..=8 {
                                ui.selectable_value(
                                    &mut self.auto_arch_count,
                                    n.to_string(),
                                    format!("{}", n),
                                );
                            }
                        });
                });

                ui.add_space(4.0);

                let generate_btn = ui.add_enabled(
                    !self.auto_arch_loading && !self.auto_arch_description.is_empty(),
                    egui::Button::new(
                        RichText::new(if self.auto_arch_loading {
                            "Generating..."
                        } else {
                            "Generate System"
                        })
                        .color(Color32::WHITE),
                    )
                    .fill(Color32::from_rgb(59, 130, 246)),
                );

                if generate_btn.clicked() {
                    self.run_auto_architecture(rt);
                }

                if let Some((msg, is_err)) = &self.auto_arch_status {
                    let color = if *is_err {
                        Color32::from_rgb(220, 50, 50)
                    } else {
                        Color32::from_rgb(50, 180, 50)
                    };
                    ui.colored_label(color, msg);
                }

                ui.add_space(8.0);
                ui.separator();

                // Add node button
                if ui.button("+ Add Agent Node").clicked() {
                    self.show_add_node = true;
                    self.new_node_name.clear();
                    self.new_node_role = "worker".to_string();
                }

                // Save button
                ui.add_space(8.0);
                let save_btn = ui.add(
                    egui::Button::new(RichText::new("Save System").color(Color32::WHITE))
                        .fill(Color32::from_rgb(34, 197, 94)),
                );
                if save_btn.clicked() {
                    self.save_current_system(rt);
                }

                if let Some((msg, is_err)) = &self.save_status {
                    let color = if *is_err {
                        Color32::from_rgb(220, 50, 50)
                    } else {
                        Color32::from_rgb(50, 180, 50)
                    };
                    ui.colored_label(color, msg);
                }
            });

            ui.separator();

            // --- Center: Graph Canvas ---
            let has_right_panel = self.selected_node_idx.is_some()
                || self.selected_connection_idx.is_some();
            let canvas_width = if has_right_panel {
                available.x - 200.0 - 240.0 - 20.0
            } else {
                available.x - 200.0 - 20.0
            };
            let canvas_height = available.y - 40.0;

            ui.vertical(|ui| {
                ui.set_width(canvas_width.max(300.0));

                // System name
                ui.horizontal(|ui| {
                    ui.label("System:");
                    ui.text_edit_singleline(&mut self.system_name);
                    ui.label("Mode:");
                    egui::ComboBox::from_id_salt("orch_mode")
                        .width(100.0)
                        .selected_text(&self.orchestration_mode)
                        .show_ui(ui, |ui| {
                            for m in &[
                                "hierarchical",
                                "flat",
                                "mesh",
                                "hybrid",
                                "pipeline",
                                "p2p",
                            ] {
                                ui.selectable_value(
                                    &mut self.orchestration_mode,
                                    m.to_string(),
                                    *m,
                                );
                            }
                        });
                    if self.connecting_from.is_some() {
                        ui.colored_label(
                            Color32::from_rgb(59, 130, 246),
                            "Click target node to connect (Esc to cancel)",
                        );
                    }
                });

                // Canvas
                let (response, painter) = ui.allocate_painter(
                    Vec2::new(canvas_width.max(300.0), canvas_height.max(200.0)),
                    egui::Sense::click_and_drag(),
                );

                let canvas_rect = response.rect;

                // Background
                painter.rect_filled(
                    canvas_rect,
                    Rounding::same(4),
                    Color32::from_rgb(30, 30, 40),
                );

                // Grid
                let grid_size = 30.0;
                let grid_color = Color32::from_rgba_premultiplied(60, 60, 80, 40);
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
                            "tcp" => Color32::from_rgb(59, 130, 246),
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
                                Color32::WHITE
                            } else {
                                Color32::from_rgb(180, 180, 200)
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
                                Stroke::new(2.0, Color32::from_rgb(59, 130, 246).linear_multiply(0.6)),
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

                    // Node background
                    painter.rect_filled(
                        node_rect,
                        Rounding::same(8),
                        role_color,
                    );

                    // Selection border
                    if is_selected {
                        painter.rect_stroke(
                            node_rect.expand(2.0),
                            Rounding::same(10),
                            Stroke::new(3.0, Color32::WHITE),
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
                        Color32::from_rgb(220, 220, 240),
                    );

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

            // --- Right panel: Node or Connection properties ---
            if let Some(idx) = self.selected_node_idx {
                if idx < self.nodes.len() {
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(230.0);
                        self.show_node_properties(ui, idx);
                    });
                }
            } else if let Some(ci) = self.selected_connection_idx {
                if ci < self.connections.len() {
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(230.0);
                        self.show_connection_properties(ui, ci);
                    });
                }
            }
        });

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
        ui.label(RichText::new("Node Properties").strong());
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
            .rect_filled(rect, Rounding::same(3), color);

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
                        "tcp" => Color32::from_rgb(59, 130, 246),
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
        ui.label(RichText::new("Connection Properties").strong());
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
            "tcp" => Color32::from_rgb(59, 130, 246),
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
                    RichText::new("tcp").color(Color32::from_rgb(59, 130, 246)),
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

    fn show_yaml_editor_dialog(&mut self, ui: &mut egui::Ui, rt: &tokio::runtime::Handle) {
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
        let dir = std::path::PathBuf::from("data/agents");
        let fp = dir.join(filename);
        if let Ok(content) = std::fs::read_to_string(&fp) {
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

        // Layout: arrange nodes in a circle
        let agents = val
            .get("agents")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        let count = agents.len();

        for (i, agent) in agents.iter().enumerate() {
            let angle = (i as f32 / count.max(1) as f32) * std::f32::consts::TAU;
            let radius = 150.0 + count as f32 * 15.0;
            let cx = 350.0 + angle.cos() * radius;
            let cy = 250.0 + angle.sin() * radius;

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

        let dir = std::path::PathBuf::from("data/agents");
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
  "connections": [ {{ "from": "source_id", "to": "target_id", "label": "label", "protocol": "tcp|queue" }} ]
}}

Rules:
- Always include ONE agent with role "human" and id "human"
- Connections use only "tcp" or "queue" protocol
- Every non-human agent needs at least one incoming connection
- For hierarchical: human -> orchestrator -> workers
- For flat: human -> all agents directly
- For mesh: no connections needed (mesh bypasses access control)
- For hybrid: human -> orchestrator -> workers (workers have mesh.enabled: true)
- For pipeline: sequential chain
- Agent IDs must be snake_case, 3-8 agents total"#
            );

            let client = reqwest::Client::new();
            let body = json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": "You are an expert multi-agent system architect. Return ONLY valid JSON, nothing else."},
                    {"role": "user", "content": user_msg}
                ],
                "temperature": 0.3,
                "max_tokens": 4096,
            });

            let result = async {
                let resp = client
                    .post(&api_url)
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| format!("Request failed: {e}"))?;

                let resp_json: Value = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

                if let Some(err) = resp_json.get("error") {
                    return Err(format!("API error: {}", err));
                }

                let raw = resp_json["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();

                // Strip <think> tags
                let mut s = raw;
                while let Some(start) = s.find("<think>") {
                    if let Some(end) = s.find("</think>") {
                        s = format!("{}{}", &s[..start], &s[end + 8..]);
                    } else {
                        break;
                    }
                }
                let s = s.trim();

                // Extract JSON object
                let parsed: Value = if let Ok(v) = serde_json::from_str(s) {
                    v
                } else if let (Some(start), Some(end)) = (s.find('{'), s.rfind('}')) {
                    serde_json::from_str(&s[start..=end])
                        .map_err(|e| format!("JSON parse error: {e}"))?
                } else {
                    return Err("No JSON found in LLM response".to_string());
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
        "orchestrator" => Color32::from_rgb(59, 130, 246), // blue
        "worker" => Color32::from_rgb(34, 197, 94),      // green
        "checker" => Color32::from_rgb(245, 158, 11),    // amber
        "reporter" => Color32::from_rgb(168, 85, 247),   // purple
        "researcher" => Color32::from_rgb(6, 182, 212),  // cyan
        "peer" => Color32::from_rgb(236, 72, 153),       // pink
        _ => Color32::from_rgb(100, 100, 100),
    }
}

async fn load_agent_files() -> Result<Vec<AgentSystemFile>, String> {
    let dir = std::path::PathBuf::from("data/agents");
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
