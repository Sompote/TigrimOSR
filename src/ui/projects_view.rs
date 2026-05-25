use eframe::egui;
use crate::server::data::{
    self, get_chat_history, get_projects, get_skills, save_chat_history, save_projects,
    AgentOverride, ChatSession, Project, Skill,
};

// ---------------------------------------------------------------------------
// Detail panel tab enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Overview,
    Memory,
    Files,
    Skills,
    Chat,
}

// ---------------------------------------------------------------------------
// Inline file entry for the per-project file browser
// ---------------------------------------------------------------------------

struct ProjectFileEntry {
    name: String,
    path: String,
    is_directory: bool,
    size: u64,
}

// ---------------------------------------------------------------------------
// ProjectsView
// ---------------------------------------------------------------------------

pub struct ProjectsView {
    projects: Vec<Project>,
    selected_project_id: Option<String>,
    show_create_dialog: bool,
    new_name: String,
    new_description: String,
    new_working_folder: String,
    needs_refresh: bool,
    confirm_delete_id: Option<String>,

    // Detail panel tabs
    detail_tab: DetailTab,

    // Memory editor state
    memory_text: String,
    memory_loaded_for: Option<String>, // project id whose memory is loaded
    memory_dirty: bool,
    memory_status: Option<(String, bool)>, // (message, is_error)

    // System prompt editor state (stored in-line on the Project, but we keep an editing buffer)
    system_prompt_text: String,

    // Agent override state
    agent_override_enabled: bool,
    agent_override_mode: String,
    agent_override_config_file: String,
    agent_config_files: Vec<String>, // discovered from data/agents/

    // Skills picker
    available_skills: Vec<Skill>,
    skills_loaded: bool,
    show_add_skill_picker: bool,

    // Per-project file browser
    file_browser_path: String,
    file_entries: Vec<ProjectFileEntry>,
    file_browser_needs_refresh: bool,
    file_selected: Option<String>,
    file_content: String,
    file_is_binary: bool,
    file_image_texture: Option<egui::TextureHandle>,
    file_editing: bool,
    file_new_dir_name: String,
    file_show_new_dir: bool,
    file_status: Option<(String, bool)>,

    // Chat sessions linked to this project
    linked_sessions: Vec<ChatSession>,
    unlinked_sessions: Vec<ChatSession>,
    sessions_loaded_for: Option<String>,
    chat_search_query: String,

    // Signal to switch to Chat tab externally (project_id, session_id)
    pub navigate_to_chat_session: Option<(String, String)>,
}

impl Default for ProjectsView {
    fn default() -> Self {
        Self {
            projects: Vec::new(),
            selected_project_id: None,
            show_create_dialog: false,
            new_name: String::new(),
            new_description: String::new(),
            new_working_folder: String::new(),
            needs_refresh: true,
            confirm_delete_id: None,

            detail_tab: DetailTab::Overview,

            memory_text: String::new(),
            memory_loaded_for: None,
            memory_dirty: false,
            memory_status: None,

            system_prompt_text: String::new(),

            agent_override_enabled: false,
            agent_override_mode: String::new(),
            agent_override_config_file: String::new(),
            agent_config_files: Vec::new(),

            available_skills: Vec::new(),
            skills_loaded: false,
            show_add_skill_picker: false,

            file_browser_path: String::new(),
            file_entries: Vec::new(),
            file_browser_needs_refresh: true,
            file_selected: None,
            file_content: String::new(),
            file_is_binary: false,
            file_image_texture: None,
            file_editing: false,
            file_new_dir_name: String::new(),
            file_show_new_dir: false,
            file_status: None,

            linked_sessions: Vec::new(),
            unlinked_sessions: Vec::new(),
            sessions_loaded_for: None,
            chat_search_query: String::new(),

            navigate_to_chat_session: None,
        }
    }
}

impl ProjectsView {
    pub fn new() -> Self {
        Self::default()
    }

    // ── helpers ────────────────────────────────────────────────────────────

    fn memory_file_path(project_id: &str) -> std::path::PathBuf {
        data::data_dir()
            .join("projects")
            .join(project_id)
            .join("memory.md")
    }

    fn load_memory(&mut self, runtime: &tokio::runtime::Handle, project_id: &str) {
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let url = format!("{}/api/projects/{}/memory", rb.url, project_id);
            let token = rb.token.clone();
            self.memory_text = runtime.block_on(async {
                let client = reqwest::Client::new();
                match client.get(&url).bearer_auth(&token).send().await {
                    Ok(resp) => {
                        if let Ok(val) = resp.json::<serde_json::Value>().await {
                            val.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string()
                        } else {
                            String::new()
                        }
                    }
                    Err(_) => String::new(),
                }
            });
        } else {
            let fp = Self::memory_file_path(project_id);
            self.memory_text = runtime.block_on(async {
                match tokio::fs::read_to_string(&fp).await {
                    Ok(c) => c,
                    Err(_) => String::new(),
                }
            });
        }
        self.memory_loaded_for = Some(project_id.to_string());
        self.memory_dirty = false;
        self.memory_status = None;
    }

    fn save_memory(&mut self, runtime: &tokio::runtime::Handle, project_id: &str) {
        if let Some(rb) = crate::server::data::get_remote_backend() {
            let url = format!("{}/api/projects/{}/memory", rb.url, project_id);
            let token = rb.token.clone();
            let content = self.memory_text.clone();
            let ok = runtime.block_on(async {
                let client = reqwest::Client::new();
                let body = serde_json::json!({ "content": content });
                client.put(&url).bearer_auth(&token).json(&body).send().await.is_ok()
            });
            if ok {
                self.memory_dirty = false;
                self.memory_status = Some(("Memory saved.".to_string(), false));
            } else {
                self.memory_status = Some(("Failed to save to remote".to_string(), true));
            }
        } else {
            let fp = Self::memory_file_path(project_id);
            let content = self.memory_text.clone();
            let result = runtime.block_on(async {
                if let Some(parent) = fp.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                tokio::fs::write(&fp, &content).await
            });
            match result {
                Ok(()) => {
                    self.memory_dirty = false;
                    self.memory_status = Some(("Memory saved.".to_string(), false));
                }
                Err(e) => {
                    self.memory_status = Some((format!("Failed to save: {}", e), true));
                }
            }
        }
    }

    fn load_agent_config_files(&mut self, runtime: &tokio::runtime::Handle) {
        let agents_dir = data::data_dir().join("agents");
        self.agent_config_files = runtime.block_on(async {
            let mut files = Vec::new();
            if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.ends_with(".json") || name.ends_with(".yaml") || name.ends_with(".yml")
                    {
                        files.push(name);
                    }
                }
            }
            files.sort();
            files
        });
    }

    fn load_linked_sessions(&mut self, runtime: &tokio::runtime::Handle, project_id: &str) {
        let all_sessions = runtime.block_on(get_chat_history());
        self.linked_sessions = Vec::new();
        self.unlinked_sessions = Vec::new();
        for s in all_sessions {
            if s.project_id.as_deref() == Some(project_id) {
                self.linked_sessions.push(s);
            } else if !s.messages.is_empty() {
                self.unlinked_sessions.push(s);
            }
        }
        self.sessions_loaded_for = Some(project_id.to_string());
    }

    fn refresh_file_browser(&mut self, runtime: &tokio::runtime::Handle, working_folder: &str) {
        if working_folder.is_empty() {
            self.file_entries.clear();
            self.file_browser_needs_refresh = false;
            return;
        }
        let path = self.file_browser_path.clone();
        match runtime.block_on(data::list_files(working_folder, &path)) {
            Ok(entries) => {
                self.file_entries = entries
                    .into_iter()
                    .filter_map(|v| {
                        Some(ProjectFileEntry {
                            name: v.get("name")?.as_str()?.to_string(),
                            path: v.get("path")?.as_str()?.to_string(),
                            is_directory: v.get("isDirectory")?.as_bool()?,
                            size: v.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                        })
                    })
                    .collect();
                self.file_entries.sort_by(|a, b| {
                    b.is_directory
                        .cmp(&a.is_directory)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                self.file_status = None;
            }
            Err(e) => {
                self.file_entries.clear();
                self.file_status = Some((format!("Failed to list: {}", e), true));
            }
        }
        self.file_browser_needs_refresh = false;
    }

    fn format_size(bytes: u64) -> String {
        if bytes < 1024 {
            format!("{} B", bytes)
        } else if bytes < 1024 * 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else if bytes < 1024 * 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        }
    }

    // ── main show ─────────────────────────────────────────────────────────

    pub fn show(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // Refresh projects from data layer when needed
        if self.needs_refresh {
            self.needs_refresh = false;
            self.projects = runtime.block_on(get_projects());
        }

        // ── Top bar: heading + create button ──────────────────────────
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading("Projects");
                ui.label(
                    egui::RichText::new("Manage your project workspaces")
                        .size(12.0)
                        .color(egui::Color32::GRAY),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("+ Create Project")
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(74, 144, 226)),
                    )
                    .clicked()
                {
                    self.new_name.clear();
                    self.new_description.clear();
                    self.new_working_folder.clear();
                    self.show_create_dialog = true;
                }
            });
        });

        ui.separator();

        if self.projects.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(
                    egui::RichText::new("\u{1F4C2}")
                        .size(48.0)
                        .color(egui::Color32::GRAY),
                );
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("No projects yet")
                        .size(18.0)
                        .color(egui::Color32::GRAY),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Click \"+ Create Project\" to set up a new workspace.",
                    )
                    .size(12.0)
                    .color(egui::Color32::GRAY),
                );
            });
        } else {
            // ── Two-panel layout: list on left, detail on right ───────
            let panel_width = ui.available_width();
            let has_selection = self.selected_project_id.is_some();
            let list_width = if has_selection {
                (250.0_f32).max(panel_width * 0.2).min(panel_width * 0.3)
            } else {
                panel_width
            };

            ui.horizontal_top(|ui| {
                // ── Left: project list ────────────────────────────────
                ui.allocate_ui(egui::vec2(list_width, ui.available_height()), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("project_list_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.set_min_width(list_width - 16.0);
                                self.show_project_list(ui);
                            });
                        });
                });

                // ── Right: detail panel ───────────────────────────────
                if has_selection {
                    ui.separator();
                    let detail_width = ui.available_width();
                    ui.allocate_ui_with_layout(
                        egui::vec2(detail_width, ui.available_height()),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("project_detail_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(detail_width - 16.0, ui.available_height()),
                                        egui::Layout::top_down(egui::Align::LEFT),
                                        |ui| {
                                            self.show_detail_panel(ui, runtime);
                                        },
                                    );
                                });
                        },
                    );
                }
            });
        }

        // ── Create-project dialog ─────────────────────────────────────
        if self.show_create_dialog {
            self.show_create_project_dialog(ui, runtime);
        }
    }

    // ── Project list rows ─────────────────────────────────────────────
    fn show_project_list(&mut self, ui: &mut egui::Ui) {
        let mut clicked_id: Option<String> = None;
        ui.set_min_width(ui.available_width());

        for project in &self.projects {
            let is_selected = self.selected_project_id.as_deref() == Some(&project.id);

            let frame = egui::Frame::default()
                .inner_margin(egui::Margin::same(10))
                .corner_radius(egui::CornerRadius::same(6))
                .fill(if is_selected {
                    egui::Color32::from_rgb(37, 99, 235).gamma_multiply(0.18)
                } else {
                    egui::Color32::TRANSPARENT
                })
                .stroke(egui::Stroke::new(
                    1.0,
                    if is_selected {
                        egui::Color32::from_rgb(59, 130, 246)
                    } else {
                        egui::Color32::from_gray(60)
                    },
                ));

            let resp = frame
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("\u{1F4C1}")
                                .size(20.0)
                                .color(egui::Color32::from_rgb(59, 130, 246)),
                        );
                        ui.vertical(|ui| {
                            ui.add(egui::Label::new(egui::RichText::new(&project.name).strong().size(14.0)).wrap());
                            if !project.description.is_empty() {
                                ui.add(egui::Label::new(
                                    egui::RichText::new(&project.description)
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                ).wrap());
                            }
                            ui.horizontal_wrapped(|ui| {
                                if !project.working_folder.is_empty() {
                                    // Show only last path component to save space
                                    let folder_display = std::path::Path::new(&project.working_folder)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or(&project.working_folder);
                                    ui.add(egui::Label::new(
                                        egui::RichText::new(format!(
                                            "\u{1F4C2} {}",
                                            folder_display
                                        ))
                                        .size(10.0)
                                        .color(egui::Color32::from_gray(120)),
                                    ).wrap());
                                }
                                let skill_count = project.skills.len();
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} skill{}",
                                        skill_count,
                                        if skill_count == 1 { "" } else { "s" }
                                    ))
                                    .size(10.0)
                                    .color(egui::Color32::from_gray(120)),
                                );
                            });
                        });
                    });
                })
                .response;

            if resp.interact(egui::Sense::click()).clicked() {
                clicked_id = Some(project.id.clone());
            }

            ui.add_space(4.0);
        }

        if let Some(id) = clicked_id {
            // Reset tab-specific state when selecting a new project
            if self.selected_project_id.as_deref() != Some(&id) {
                self.detail_tab = DetailTab::Overview;
                self.memory_loaded_for = None;
                self.sessions_loaded_for = None;
                self.file_browser_path.clear();
                self.file_browser_needs_refresh = true;
                self.file_selected = None;
                self.file_content.clear();
                self.file_editing = false;
                self.skills_loaded = false;
                self.show_add_skill_picker = false;
            }
            self.selected_project_id = Some(id);
        }
    }

    // ── Detail panel for the selected project ─────────────────────────
    fn show_detail_panel(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let sel_id = match &self.selected_project_id {
            Some(id) => id.clone(),
            None => return,
        };

        let Some(idx) = self.projects.iter().position(|p| p.id == sel_id) else {
            self.selected_project_id = None;
            return;
        };

        // Project heading
        ui.add_space(4.0);
        ui.heading(self.projects[idx].name.clone());
        ui.add_space(4.0);

        // ── Tab bar ───────────────────────────────────────────────────
        ui.horizontal(|ui| {
            let tab_btn = |ui: &mut egui::Ui, label: &str, tab: DetailTab, current: &mut DetailTab| {
                let is_active = *current == tab;
                let text = if is_active {
                    egui::RichText::new(label).strong().color(egui::Color32::WHITE)
                } else {
                    egui::RichText::new(label).color(egui::Color32::GRAY)
                };
                let btn = if is_active {
                    egui::Button::new(text)
                        .fill(egui::Color32::from_rgb(59, 130, 246))
                        .corner_radius(egui::CornerRadius::same(4))
                } else {
                    egui::Button::new(text)
                        .fill(egui::Color32::TRANSPARENT)
                        .corner_radius(egui::CornerRadius::same(4))
                };
                if ui.add(btn).clicked() {
                    *current = tab;
                }
            };
            tab_btn(ui, "Overview", DetailTab::Overview, &mut self.detail_tab);
            tab_btn(ui, "Memory", DetailTab::Memory, &mut self.detail_tab);
            tab_btn(ui, "Files", DetailTab::Files, &mut self.detail_tab);
            tab_btn(ui, "Skills", DetailTab::Skills, &mut self.detail_tab);
            tab_btn(ui, "Chat", DetailTab::Chat, &mut self.detail_tab);
        });

        ui.separator();
        ui.add_space(4.0);

        match self.detail_tab {
            DetailTab::Overview => self.show_overview_tab(ui, runtime, idx, &sel_id),
            DetailTab::Memory => self.show_memory_tab(ui, runtime, &sel_id),
            DetailTab::Files => {
                let wf = self.projects[idx].working_folder.clone();
                self.show_files_tab(ui, runtime, &wf);
            }
            DetailTab::Skills => self.show_skills_tab(ui, runtime, idx),
            DetailTab::Chat => self.show_chat_tab(ui, runtime, &sel_id),
        }
    }

    // =====================================================================
    // TAB: Overview
    // =====================================================================
    fn show_overview_tab(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &tokio::runtime::Handle,
        idx: usize,
        sel_id: &str,
    ) {
        egui::Grid::new("project_detail_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut self.projects[idx].name);
                ui.end_row();

                ui.label("Description:");
                ui.text_edit_multiline(&mut self.projects[idx].description);
                ui.end_row();

                ui.label("Working Folder:");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.projects[idx].working_folder);
                    if ui.button("Browse...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Select working folder")
                            .pick_folder()
                        {
                            self.projects[idx].working_folder =
                                path.to_string_lossy().to_string();
                        }
                    }
                });
                ui.end_row();

                ui.label("Skills:");
                ui.label(
                    egui::RichText::new(format!("{}", self.projects[idx].skills.len()))
                        .size(12.0)
                        .color(egui::Color32::GRAY),
                );
                ui.end_row();

                ui.label("Created:");
                ui.label(
                    egui::RichText::new(&self.projects[idx].created_at)
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                ui.end_row();

                ui.label("Updated:");
                ui.label(
                    egui::RichText::new(&self.projects[idx].updated_at)
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                ui.end_row();
            });

        ui.add_space(12.0);

        // ── System Prompt ─────────────────────────────────────────────
        ui.collapsing("System Prompt", |ui| {
            // Sync buffer from project on first open
            let current = self.projects[idx]
                .system_prompt
                .clone()
                .unwrap_or_default();
            if self.system_prompt_text != current
                && self.system_prompt_text.is_empty()
            {
                self.system_prompt_text = current;
            }
            ui.label(
                egui::RichText::new("Custom system prompt override for this project")
                    .size(11.0)
                    .color(egui::Color32::GRAY),
            );
            ui.add(
                egui::TextEdit::multiline(&mut self.system_prompt_text)
                    .desired_width(f32::INFINITY)
                    .desired_rows(5)
                    .hint_text("Enter a custom system prompt..."),
            );
            if ui.button("Apply System Prompt").clicked() {
                let val = self.system_prompt_text.trim().to_string();
                self.projects[idx].system_prompt = if val.is_empty() {
                    None
                } else {
                    Some(val)
                };
            }
        });

        ui.add_space(8.0);

        // ── Agent Override ────────────────────────────────────────────
        ui.collapsing("Agent Override", |ui| {
            // Load agent config files list if not loaded
            if self.agent_config_files.is_empty() {
                self.load_agent_config_files(runtime);
            }

            // Sync from project
            if let Some(ref ao) = self.projects[idx].agent_override.clone() {
                self.agent_override_enabled = ao.enabled.unwrap_or(false);
                self.agent_override_mode = ao.sub_agent_mode.clone().unwrap_or_default();
                self.agent_override_config_file =
                    ao.sub_agent_config_file.clone().unwrap_or_default();
            }

            ui.checkbox(&mut self.agent_override_enabled, "Enable Agent Override");

            if self.agent_override_enabled {
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    egui::ComboBox::from_id_salt("agent_mode_combo")
                        .selected_text(if self.agent_override_mode.is_empty() {
                            "Select mode..."
                        } else {
                            &self.agent_override_mode
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.agent_override_mode,
                                "single".to_string(),
                                "Single Agent",
                            );
                            ui.selectable_value(
                                &mut self.agent_override_mode,
                                "multi".to_string(),
                                "Multi Agent",
                            );
                            ui.selectable_value(
                                &mut self.agent_override_mode,
                                "auto".to_string(),
                                "Auto",
                            );
                        });
                });

                ui.horizontal(|ui| {
                    ui.label("Config File:");
                    let files = self.agent_config_files.clone();
                    egui::ComboBox::from_id_salt("agent_config_combo")
                        .selected_text(if self.agent_override_config_file.is_empty() {
                            "Select config..."
                        } else {
                            &self.agent_override_config_file
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.agent_override_config_file,
                                String::new(),
                                "(none)",
                            );
                            for f in &files {
                                ui.selectable_value(
                                    &mut self.agent_override_config_file,
                                    f.clone(),
                                    f,
                                );
                            }
                        });
                });
            }

            if ui.button("Apply Agent Override").clicked() {
                if self.agent_override_enabled {
                    self.projects[idx].agent_override = Some(AgentOverride {
                        enabled: Some(true),
                        sub_agent_mode: if self.agent_override_mode.is_empty() {
                            None
                        } else {
                            Some(self.agent_override_mode.clone())
                        },
                        sub_agent_config_file: if self.agent_override_config_file.is_empty() {
                            None
                        } else {
                            Some(self.agent_override_config_file.clone())
                        },
                        auto_architecture_type: None,
                        auto_agent_count: None,
                        auto_protocols: None,
                    });
                } else {
                    self.projects[idx].agent_override = None;
                }
            }
        });

        ui.add_space(16.0);

        // ── Action buttons ────────────────────────────────────────────
        let working_folder = self.projects[idx].working_folder.clone();

        ui.horizontal(|ui| {
            // Save button
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Save Changes").color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(34, 197, 94)),
                )
                .clicked()
            {
                self.projects[idx].updated_at = chrono::Utc::now().to_rfc3339();
                // Resolve relative working folder to absolute path
                let wf = &self.projects[idx].working_folder;
                if !wf.is_empty() && !std::path::Path::new(wf).is_absolute() {
                    let sandbox = crate::server::data::get_sandbox_dir_sync();
                    self.projects[idx].working_folder = std::path::PathBuf::from(&sandbox)
                        .join(wf)
                        .to_string_lossy()
                        .to_string();
                }
                let to_save = self.projects.clone();
                runtime.block_on(save_projects(&to_save));
                self.needs_refresh = true;
            }

            // Open folder button
            if !working_folder.is_empty() {
                if ui.button("Open Folder").clicked() {
                    let _ = open::that(&working_folder);
                }
            }

            // Delete button
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Delete")
                            .color(egui::Color32::from_rgb(239, 68, 68)),
                    ),
                )
                .clicked()
            {
                self.confirm_delete_id = Some(sel_id.to_string());
            }
        });

        // ── Delete confirmation ───────────────────────────────────────
        if self.confirm_delete_id.as_deref() == Some(sel_id) {
            ui.add_space(12.0);
            egui::Frame::default()
                .inner_margin(egui::Margin::same(10))
                .corner_radius(egui::CornerRadius::same(6))
                .fill(egui::Color32::from_rgb(50, 20, 20))
                .stroke(egui::Stroke::new(
                    1.0,
                    egui::Color32::from_rgb(239, 68, 68),
                ))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("Are you sure you want to delete this project?")
                            .color(egui::Color32::from_rgb(239, 68, 68)),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.confirm_delete_id = None;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Confirm Delete")
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(220, 38, 38)),
                            )
                            .clicked()
                        {
                            let sel = sel_id.to_string();
                            let remaining: Vec<Project> = self
                                .projects
                                .iter()
                                .filter(|p| p.id != sel)
                                .cloned()
                                .collect();
                            runtime.block_on(save_projects(&remaining));
                            self.selected_project_id = None;
                            self.confirm_delete_id = None;
                            self.needs_refresh = true;
                        }
                    });
                });
        }
    }

    // =====================================================================
    // TAB: Memory
    // =====================================================================
    fn show_memory_tab(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &tokio::runtime::Handle,
        project_id: &str,
    ) {
        // Load memory if not yet loaded for this project
        if self.memory_loaded_for.as_deref() != Some(project_id) {
            self.load_memory(runtime, project_id);
        }

        ui.label(
            egui::RichText::new("Project Memory (Markdown)")
                .size(14.0)
                .strong(),
        );
        ui.label(
            egui::RichText::new(format!(
                "Stored at: data/projects/{}/memory.md",
                project_id
            ))
            .size(10.0)
            .color(egui::Color32::GRAY),
        );
        ui.add_space(4.0);

        // Status message
        if let Some((ref msg, is_err)) = self.memory_status {
            let color = if is_err {
                egui::Color32::from_rgb(239, 68, 68)
            } else {
                egui::Color32::from_rgb(34, 197, 94)
            };
            ui.label(egui::RichText::new(msg).size(12.0).color(color));
        }

        // Toolbar
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Save Memory").color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(34, 197, 94)),
                )
                .clicked()
            {
                let pid = project_id.to_string();
                self.save_memory(runtime, &pid);
            }

            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Reload").color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(74, 144, 226)),
                )
                .clicked()
            {
                let pid = project_id.to_string();
                self.load_memory(runtime, &pid);
            }

            if ui.button("Generate Memory").clicked() {
                self.memory_status = Some((
                    "Memory generation is a placeholder -- coming soon with AI integration."
                        .to_string(),
                    false,
                ));
            }

            if self.memory_dirty {
                ui.label(
                    egui::RichText::new("(unsaved changes)")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(250, 204, 21)),
                );
            }
        });

        ui.add_space(4.0);

        // Text editor
        let response = ui.add(
            egui::TextEdit::multiline(&mut self.memory_text)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY)
                .desired_rows(20)
                .code_editor(),
        );
        if response.changed() {
            self.memory_dirty = true;
        }
    }

    // =====================================================================
    // TAB: Files
    // =====================================================================
    fn show_files_tab(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &tokio::runtime::Handle,
        working_folder: &str,
    ) {
        if working_folder.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(
                    egui::RichText::new("No working folder set")
                        .size(16.0)
                        .color(egui::Color32::GRAY),
                );
                ui.label(
                    egui::RichText::new("Set a working folder in the Overview tab first.")
                        .size(12.0)
                        .color(egui::Color32::GRAY),
                );
            });
            return;
        }

        if self.file_browser_needs_refresh {
            self.refresh_file_browser(runtime, working_folder);
        }

        // Toolbar
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Files").size(14.0).strong());
            ui.separator();

            // Breadcrumb
            if ui
                .add(egui::Button::new(egui::RichText::new("root").size(12.0)).frame(false))
                .clicked()
            {
                self.file_browser_path.clear();
                self.file_selected = None;
                self.file_content.clear();
                self.file_editing = false;
                self.file_browser_needs_refresh = true;
            }
            if !self.file_browser_path.is_empty() {
                let path_clone = self.file_browser_path.clone();
                let segments: Vec<&str> = path_clone.split('/').collect();
                let mut nav: Option<String> = None;
                for (i, seg) in segments.iter().enumerate() {
                    ui.label(
                        egui::RichText::new("/").size(12.0).color(egui::Color32::GRAY),
                    );
                    let partial: String = segments[..=i].join("/");
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new(*seg).size(12.0)).frame(false),
                        )
                        .clicked()
                    {
                        nav = Some(partial);
                    }
                }
                if let Some(target) = nav {
                    self.file_browser_path = target;
                    self.file_selected = None;
                    self.file_content.clear();
                    self.file_editing = false;
                    self.file_browser_needs_refresh = true;
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Upload button
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Upload").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(74, 144, 226)),
                    )
                    .clicked()
                {
                    if let Some(src) = rfd::FileDialog::new()
                        .set_title("Select file to upload")
                        .pick_file()
                    {
                        let file_name = src
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "uploaded_file".to_string());
                        let dest_rel = if self.file_browser_path.is_empty() {
                            file_name.clone()
                        } else {
                            format!("{}/{}", self.file_browser_path, file_name)
                        };
                        // Try text first, fall back to binary
                        let wf = working_folder.to_string();
                        match std::fs::read_to_string(&src) {
                            Ok(content) => {
                                match runtime
                                    .block_on(data::write_file_content(&wf, &dest_rel, &content))
                                {
                                    Ok(()) => {
                                        self.file_browser_needs_refresh = true;
                                        self.file_status =
                                            Some((format!("Uploaded: {}", file_name), false));
                                    }
                                    Err(e) => {
                                        self.file_status =
                                            Some((format!("Upload failed: {}", e), true));
                                    }
                                }
                            }
                            Err(_) => {
                                if let Ok(bytes) = std::fs::read(&src) {
                                    let resolved = data::validate_path(&wf, &dest_rel);
                                    match resolved {
                                        Ok(full_path) => {
                                            if let Some(parent) = full_path.parent() {
                                                let _ = std::fs::create_dir_all(parent);
                                            }
                                            match std::fs::write(&full_path, &bytes) {
                                                Ok(()) => {
                                                    self.file_browser_needs_refresh = true;
                                                    self.file_status = Some((
                                                        format!("Uploaded: {}", file_name),
                                                        false,
                                                    ));
                                                }
                                                Err(e) => {
                                                    self.file_status = Some((
                                                        format!("Upload failed: {}", e),
                                                        true,
                                                    ));
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            self.file_status =
                                                Some((format!("Path error: {}", e), true));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // New Folder
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("New Folder").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(34, 197, 94)),
                    )
                    .clicked()
                {
                    self.file_show_new_dir = !self.file_show_new_dir;
                    self.file_new_dir_name.clear();
                }
            });
        });

        // New dir input
        if self.file_show_new_dir {
            ui.horizontal(|ui| {
                ui.label("Folder name:");
                let resp = ui.text_edit_singleline(&mut self.file_new_dir_name);
                if ui.button("Create").clicked()
                    || (resp.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    let dir_name = self.file_new_dir_name.trim().to_string();
                    if !dir_name.is_empty() {
                        let rel = if self.file_browser_path.is_empty() {
                            dir_name.clone()
                        } else {
                            format!("{}/{}", self.file_browser_path, dir_name)
                        };
                        let full =
                            std::path::Path::new(working_folder).join(&rel);
                        match runtime.block_on(tokio::fs::create_dir_all(&full)) {
                            Ok(()) => {
                                self.file_new_dir_name.clear();
                                self.file_show_new_dir = false;
                                self.file_browser_needs_refresh = true;
                                self.file_status =
                                    Some((format!("Created folder: {}", dir_name), false));
                            }
                            Err(e) => {
                                self.file_status =
                                    Some((format!("Failed: {}", e), true));
                            }
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.file_show_new_dir = false;
                    self.file_new_dir_name.clear();
                }
            });
        }

        // Status message
        if let Some((ref msg, is_err)) = self.file_status {
            let color = if is_err {
                egui::Color32::from_rgb(239, 68, 68)
            } else {
                egui::Color32::from_rgb(34, 197, 94)
            };
            ui.label(egui::RichText::new(msg).size(11.0).color(color));
        }

        ui.separator();

        // Two-column: file list left, viewer right
        let avail = ui.available_size();
        let left_w = (avail.x * 0.38).max(180.0);
        // Use screen height minus current position for robust sizing
        // (avail.y can be tiny inside a parent ScrollArea)
        let screen_h = ui.ctx().screen_rect().height();
        let cursor_y = ui.cursor().top();
        let panel_h = (screen_h - cursor_y - 30.0).max(400.0);

        ui.horizontal_top(|ui| {
            ui.set_min_height(panel_h);
            // Left: file list
            ui.vertical(|ui| {
                ui.set_width(left_w);
                egui::ScrollArea::vertical()
                    .id_salt("project_files_list")
                    .auto_shrink([false, false])
                    .max_height(panel_h - 8.0)
                    .show(ui, |ui| {
                        // ".." go-back
                        if !self.file_browser_path.is_empty() {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("\u{1F4C2} ..")
                                            .size(13.0)
                                            .color(egui::Color32::from_gray(160)),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                if let Some(pos) = self.file_browser_path.rfind('/') {
                                    self.file_browser_path =
                                        self.file_browser_path[..pos].to_string();
                                } else {
                                    self.file_browser_path.clear();
                                }
                                self.file_selected = None;
                                self.file_content.clear();
                                self.file_editing = false;
                                self.file_browser_needs_refresh = true;
                            }
                            ui.separator();
                        }

                        let mut nav_target: Option<String> = None;
                        let mut sel_target: Option<String> = None;

                        for entry in &self.file_entries {
                            let is_sel = self
                                .file_selected
                                .as_ref()
                                .map(|s| s == &entry.path)
                                .unwrap_or(false);
                            let icon = if entry.is_directory {
                                "\u{1F4C1}"
                            } else {
                                "\u{1F4C4}"
                            };
                            let label_text = if entry.is_directory {
                                format!("{} {}", icon, entry.name)
                            } else {
                                format!(
                                    "{} {} ({})",
                                    icon,
                                    entry.name,
                                    Self::format_size(entry.size)
                                )
                            };

                            let text = if is_sel {
                                egui::RichText::new(&label_text)
                                    .size(12.0)
                                    .color(egui::Color32::WHITE)
                            } else {
                                egui::RichText::new(&label_text).size(12.0)
                            };
                            let btn = if is_sel {
                                egui::Button::new(text)
                                    .fill(egui::Color32::from_rgb(59, 130, 246))
                            } else {
                                egui::Button::new(text).frame(false)
                            };
                            let resp = ui.add_sized([left_w - 8.0, 22.0], btn);
                            if resp.clicked() {
                                if entry.is_directory {
                                    nav_target = Some(entry.path.clone());
                                } else {
                                    sel_target = Some(entry.path.clone());
                                }
                            }
                        }

                        if let Some(path) = nav_target {
                            self.file_browser_path = path;
                            self.file_selected = None;
                            self.file_content.clear();
                            self.file_editing = false;
                            self.file_browser_needs_refresh = true;
                        } else if let Some(path) = sel_target {
                            let wf = working_folder.to_string();
                            let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                            let image_exts = ["png", "jpg", "jpeg", "gif", "bmp", "webp"];
                            if image_exts.contains(&ext.as_str()) {
                                // Load as image texture
                                self.file_is_binary = true;
                                self.file_content = format!("[Image: {}]", path);
                                self.file_editing = false;
                                self.file_status = None;
                                let full_path = std::path::Path::new(&wf).join(&path);
                                match image::open(&full_path) {
                                    Ok(img) => {
                                        let rgba = img.to_rgba8();
                                        let size = [rgba.width() as usize, rgba.height() as usize];
                                        let pixels = rgba.into_raw();
                                        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                                        self.file_image_texture = Some(ui.ctx().load_texture(
                                            format!("file_preview_{}", path),
                                            color_image,
                                            egui::TextureOptions::LINEAR,
                                        ));
                                    }
                                    Err(e) => {
                                        self.file_image_texture = None;
                                        self.file_status = Some((format!("Failed to load image: {}", e), true));
                                    }
                                }
                            } else {
                                self.file_is_binary = false;
                                self.file_image_texture = None;
                                match runtime.block_on(data::read_file_content(&wf, &path)) {
                                    Ok(content) => {
                                        self.file_content = content;
                                        self.file_editing = false;
                                        self.file_status = None;
                                    }
                                    Err(e) => {
                                        self.file_content.clear();
                                        self.file_status =
                                            Some((format!("Failed to read: {}", e), true));
                                    }
                                }
                            }
                            self.file_selected = Some(path);
                        }

                        if self.file_entries.is_empty() {
                            ui.add_space(20.0);
                            ui.label(
                                egui::RichText::new("Empty directory")
                                    .size(12.0)
                                    .color(egui::Color32::GRAY),
                            );
                        }
                    });
            });

            ui.separator();

            // Right: file viewer
            ui.vertical(|ui| {
                if let Some(ref selected) = self.file_selected.clone() {
                    let file_name = selected
                        .rsplit('/')
                        .next()
                        .unwrap_or(selected)
                        .to_string();

                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&file_name).size(13.0).strong(),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                // Delete
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new("Delete")
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(239, 68, 68)),
                                    ))
                                    .clicked()
                                {
                                    let wf = working_folder.to_string();
                                    let p = selected.clone();
                                    match runtime
                                        .block_on(data::delete_file_or_dir(&wf, &p))
                                    {
                                        Ok(()) => {
                                            self.file_selected = None;
                                            self.file_content.clear();
                                            self.file_editing = false;
                                            self.file_browser_needs_refresh = true;
                                            self.file_status =
                                                Some(("Deleted.".to_string(), false));
                                        }
                                        Err(e) => {
                                            self.file_status = Some((
                                                format!("Delete failed: {}", e),
                                                true,
                                            ));
                                        }
                                    }
                                }

                                // Save (if editing)
                                if self.file_editing {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("Save")
                                                    .size(11.0)
                                                    .color(egui::Color32::WHITE),
                                            )
                                            .fill(egui::Color32::from_rgb(34, 197, 94)),
                                        )
                                        .clicked()
                                    {
                                        let wf = working_folder.to_string();
                                        let p = selected.clone();
                                        let c = self.file_content.clone();
                                        match runtime.block_on(data::write_file_content(
                                            &wf, &p, &c,
                                        )) {
                                            Ok(()) => {
                                                self.file_editing = false;
                                                self.file_status =
                                                    Some(("Saved.".to_string(), false));
                                                self.file_browser_needs_refresh = true;
                                            }
                                            Err(e) => {
                                                self.file_status = Some((
                                                    format!("Save failed: {}", e),
                                                    true,
                                                ));
                                            }
                                        }
                                    }
                                }

                                // Edit toggle (not for binary files)
                                if !self.file_is_binary {
                                    let edit_label = if self.file_editing {
                                        "View"
                                    } else {
                                        "Edit"
                                    };
                                    if ui.button(edit_label).clicked() {
                                        self.file_editing = !self.file_editing;
                                    }
                                }
                            },
                        );
                    });
                    ui.separator();

                    let content_h = (panel_h - 40.0).max(200.0);
                    egui::ScrollArea::both()
                        .id_salt("project_file_content")
                        .auto_shrink([false, false])
                        .max_height(content_h)
                        .show(ui, |ui| {
                            if self.file_is_binary {
                                // Image preview
                                if let Some(ref tex) = self.file_image_texture {
                                    let tex_size = tex.size_vec2();
                                    let avail_w = ui.available_width().min(tex_size.x);
                                    let scale = avail_w / tex_size.x;
                                    let display_size = egui::vec2(tex_size.x * scale, tex_size.y * scale);
                                    ui.image(egui::load::SizedTexture::new(tex.id(), display_size));
                                } else {
                                    ui.label(
                                        egui::RichText::new(&self.file_content)
                                            .color(egui::Color32::GRAY),
                                    );
                                }
                            } else {
                                let rows = (content_h / 14.0) as usize;
                                if self.file_editing {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut self.file_content)
                                            .font(egui::TextStyle::Monospace)
                                            .desired_width(f32::INFINITY)
                                            .desired_rows(rows)
                                            .code_editor(),
                                    );
                                } else {
                                    ui.add(
                                        egui::TextEdit::multiline(
                                            &mut self.file_content.as_str(),
                                        )
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(rows)
                                        .code_editor(),
                                    );
                                }
                            }
                        });
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.label(
                            egui::RichText::new("Select a file to view")
                                .size(13.0)
                                .color(egui::Color32::GRAY),
                        );
                    });
                }
            });
        });
    }

    // =====================================================================
    // TAB: Skills
    // =====================================================================
    fn show_skills_tab(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &tokio::runtime::Handle,
        idx: usize,
    ) {
        // Always reload skills when tab is shown
        self.available_skills = runtime.block_on(get_skills());
        self.skills_loaded = true;

        let full_width = ui.available_width();

        // ── Assigned Skills ──
        let project_skills = self.projects[idx].skills.clone();
        let mut remove_idx: Option<usize> = None;
        let mut skill_to_add: Option<String> = None;

        ui.vertical(|ui| {
        ui.set_min_width(full_width);

        ui.label(
            egui::RichText::new(format!("Assigned Skills ({})", project_skills.len()))
                .size(14.0)
                .strong(),
        );
        ui.add_space(4.0);

        if project_skills.is_empty() {
            ui.label(
                egui::RichText::new("No skills assigned yet")
                    .size(12.0)
                    .color(egui::Color32::GRAY),
            );
        } else {
            for (si, skill_id) in project_skills.iter().enumerate() {
                let skill_info = self
                    .available_skills
                    .iter()
                    .find(|s| &s.id == skill_id);

                egui::Frame::default()
                    .inner_margin(egui::Margin::same(10))
                    .corner_radius(egui::CornerRadius::same(6))
                    .fill(egui::Color32::from_rgb(220, 238, 255))
                    .stroke(egui::Stroke::new(1.5, egui::Color32::from_rgb(59, 130, 246)))
                    .show(ui, |ui| {
                        if let Some(info) = skill_info {
                            let status = if info.enabled { "enabled" } else { "disabled" };
                            let status_color = if info.enabled {
                                egui::Color32::from_rgb(34, 197, 94)
                            } else {
                                egui::Color32::from_rgb(156, 163, 175)
                            };
                            ui.label(
                                egui::RichText::new(&info.name)
                                    .size(13.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(20, 50, 100)),
                            );
                            ui.label(
                                egui::RichText::new(&info.description)
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(80, 80, 100)),
                            );
                            ui.label(
                                egui::RichText::new(status)
                                    .size(10.0)
                                    .color(status_color),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(format!("Skill: {}", skill_id))
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(20, 50, 100)),
                            );
                        }
                        ui.add_space(4.0);
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Remove")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            ))
                            .clicked()
                        {
                            remove_idx = Some(si);
                        }
                    });
                ui.add_space(4.0);
            }
        }

        if let Some(ri) = remove_idx {
            self.projects[idx].skills.remove(ri);
            self.projects[idx].updated_at = chrono::Utc::now().to_rfc3339();
            let to_save = self.projects.clone();
            runtime.block_on(save_projects(&to_save));
        }

        // ── Available Skills to Add ──
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Available Skills")
                .size(14.0)
                .strong(),
        );
        ui.add_space(4.0);

        let avail = self.available_skills.clone();
        let unassigned: Vec<_> = avail.iter()
            .filter(|s| !self.projects[idx].skills.contains(&s.id))
            .collect();

        if unassigned.is_empty() && avail.is_empty() {
            ui.label(
                egui::RichText::new("No skills in the system. Create skills in the Skills tab.")
                    .size(12.0)
                    .color(egui::Color32::GRAY),
            );
        } else if unassigned.is_empty() {
            ui.label(
                egui::RichText::new("All skills are already assigned")
                    .size(12.0)
                    .color(egui::Color32::from_rgb(34, 197, 94)),
            );
        } else {
            for skill in &unassigned {
                egui::Frame::default()
                    .inner_margin(egui::Margin::same(10))
                    .corner_radius(egui::CornerRadius::same(6))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(160)))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&skill.name)
                                .size(13.0)
                                .strong(),
                        );
                        ui.label(
                            egui::RichText::new(&skill.description)
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                        ui.label(
                            egui::RichText::new(format!("source: {}", skill.source))
                                .size(9.0)
                                .color(egui::Color32::from_gray(140)),
                        );
                        ui.add_space(4.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("+ Add")
                                        .size(11.0)
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(74, 144, 226)),
                            )
                            .clicked()
                        {
                            skill_to_add = Some(skill.id.clone());
                        }
                    });
                ui.add_space(4.0);
            }
        }

        }); // end ui.vertical

        if let Some(sid) = skill_to_add {
            self.projects[idx].skills.push(sid);
            self.projects[idx].updated_at = chrono::Utc::now().to_rfc3339();
            let to_save = self.projects.clone();
            runtime.block_on(save_projects(&to_save));
            self.needs_refresh = true;
        }
    }

    // =====================================================================
    // TAB: Chat
    // =====================================================================
    fn show_chat_tab(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &tokio::runtime::Handle,
        project_id: &str,
    ) {
        // Load sessions if not yet loaded for this project
        if self.sessions_loaded_for.as_deref() != Some(project_id) {
            self.load_linked_sessions(runtime, project_id);
        }

        let _full_width = ui.available_width();

        // Header buttons
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Linked Chat Sessions").size(14.0).strong());
            ui.label(
                egui::RichText::new(format!("({})", self.linked_sessions.len()))
                    .size(12.0)
                    .color(egui::Color32::GRAY),
            );
            if ui.button("+ New Chat").clicked() {
                let project_name = self.projects.iter()
                    .find(|p| p.id == project_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                let new_id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();
                let new_session = ChatSession {
                    id: new_id.clone(),
                    title: format!("[{}] New chat", project_name),
                    messages: Vec::new(),
                    created_at: now.clone(),
                    updated_at: now,
                    skill_candidate: None,
                    skill_feedback: None,
                    project_id: Some(project_id.to_string()),
                };
                let pid = project_id.to_string();
                runtime.block_on(async {
                    let mut sessions = get_chat_history().await;
                    sessions.insert(0, new_session);
                    save_chat_history(&sessions).await;
                });
                self.navigate_to_chat_session = Some((pid.clone(), new_id));
                self.load_linked_sessions(runtime, &pid);
            }
            if ui.button("Refresh").clicked() {
                let pid = project_id.to_string();
                self.load_linked_sessions(runtime, &pid);
            }
        });

        ui.separator();

        let linked = self.linked_sessions.clone();
        let unlinked = self.unlinked_sessions.clone();
        let mut open_session_id: Option<String> = None;
        let mut link_session_id: Option<String> = None;
        let mut unlink_session_id: Option<String> = None;

        // No nested ScrollArea — parent detail panel already scrolls

        // ── Linked sessions ──
        if linked.is_empty() {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("No chat sessions linked yet")
                    .size(12.0)
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(10.0);
        }
        for session in &linked {
            egui::Frame::default()
                .inner_margin(egui::Margin::same(10))
                .corner_radius(egui::CornerRadius::same(6))
                .fill(egui::Color32::from_rgba_premultiplied(74, 144, 226, 25))
                .stroke(egui::Stroke::new(1.5, egui::Color32::from_rgb(74, 144, 226)))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(&session.title)
                            .size(13.0)
                            .strong()
                            .color(egui::Color32::from_rgb(20, 50, 100)),
                    );
                    ui.label(
                        egui::RichText::new(format!("{} messages", session.messages.len()))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(80, 80, 100)),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.add(
                            egui::Button::new(
                                egui::RichText::new("▶ Open Chat")
                                    .size(11.0)
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(74, 144, 226)),
                        ).clicked() {
                            open_session_id = Some(session.id.clone());
                        }
                        if ui.button("Unlink").clicked() {
                            unlink_session_id = Some(session.id.clone());
                        }
                    });
                });
            ui.add_space(4.0);
        }

        // ── Link existing chat (search-based) ──
        if !unlinked.is_empty() {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Link Existing Chat").size(13.0).strong());
                ui.label(
                    egui::RichText::new(format!("({} available)", unlinked.len()))
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
            });
            ui.add_space(4.0);

            // Search box
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("\u{1F50D}").size(13.0));
                ui.add(
                    egui::TextEdit::singleline(&mut self.chat_search_query)
                        .hint_text("Search chats by title...")
                        .desired_width(300.0),
                );
            });
            ui.add_space(4.0);

            // Only show results when there's a search query
            if !self.chat_search_query.trim().is_empty() {
                let query_lower = self.chat_search_query.trim().to_lowercase();
                let filtered: Vec<_> = unlinked.iter()
                    .filter(|s| s.title.to_lowercase().contains(&query_lower))
                    .take(10)
                    .collect();

                if filtered.is_empty() {
                    ui.label(
                        egui::RichText::new("No matching chats found")
                            .size(11.0)
                            .color(egui::Color32::GRAY)
                            .italics(),
                    );
                } else {
                    for session in &filtered {
                        egui::Frame::default()
                            .inner_margin(egui::Margin::same(8))
                            .corner_radius(egui::CornerRadius::same(4))
                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(210, 218, 230)))
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(&session.title).size(12.0));
                                ui.label(
                                    egui::RichText::new(format!("{} messages", session.messages.len()))
                                        .size(10.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.add_space(2.0);
                                if ui.add(
                                    egui::Button::new(
                                        egui::RichText::new("+ Link to Project")
                                            .size(11.0)
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(74, 144, 226)),
                                ).clicked() {
                                    link_session_id = Some(session.id.clone());
                                }
                            });
                        ui.add_space(3.0);
                    }
                    if unlinked.iter().filter(|s| s.title.to_lowercase().contains(&query_lower)).count() > 10 {
                        ui.label(
                            egui::RichText::new("...and more. Refine your search.")
                                .size(11.0)
                                .color(egui::Color32::GRAY)
                                .italics(),
                        );
                    }
                }
            }
        }

        // Handle actions
        if let Some(sid) = open_session_id {
            self.navigate_to_chat_session = Some((project_id.to_string(), sid));
        }
        if let Some(sid) = link_session_id {
            let pid = project_id.to_string();
            runtime.block_on(async {
                let mut sessions = get_chat_history().await;
                if let Some(s) = sessions.iter_mut().find(|s| s.id == sid) {
                    s.project_id = Some(pid.clone());
                }
                save_chat_history(&sessions).await;
            });
            self.load_linked_sessions(runtime, project_id);
        }
        if let Some(sid) = unlink_session_id {
            runtime.block_on(async {
                let mut sessions = get_chat_history().await;
                if let Some(s) = sessions.iter_mut().find(|s| s.id == sid) {
                    s.project_id = None;
                }
                save_chat_history(&sessions).await;
            });
            self.load_linked_sessions(runtime, project_id);
        }
    }

    // ── Create-project dialog window ──────────────────────────────────
    fn show_create_project_dialog(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &tokio::runtime::Handle,
    ) {
        let mut still_open = true;

        egui::Window::new("Create Project")
            .open(&mut still_open)
            .resizable(false)
            .collapsible(false)
            .default_size([400.0, 260.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.add_space(8.0);

                egui::Grid::new("create_project_grid")
                    .num_columns(2)
                    .spacing([12.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut self.new_name);
                        ui.end_row();

                        ui.label("Description:");
                        ui.text_edit_multiline(&mut self.new_description);
                        ui.end_row();

                        ui.label("Working Folder:");
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut self.new_working_folder);
                            if ui.button("Browse...").clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_title("Select working folder")
                                    .pick_folder()
                                {
                                    self.new_working_folder =
                                        path.to_string_lossy().to_string();
                                }
                            }
                        });
                        ui.end_row();
                    });

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    let name_empty = self.new_name.trim().is_empty();

                    if ui
                        .add_enabled(
                            !name_empty,
                            egui::Button::new(
                                egui::RichText::new("Create").color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(74, 144, 226)),
                        )
                        .clicked()
                    {
                        let now = chrono::Utc::now().to_rfc3339();
                        // Resolve relative working folder to absolute path
                        let wf_raw = self.new_working_folder.trim().to_string();
                        let resolved_wf = if wf_raw.is_empty() || std::path::Path::new(&wf_raw).is_absolute() {
                            wf_raw
                        } else {
                            let sandbox = crate::server::data::get_sandbox_dir_sync();
                            std::path::PathBuf::from(&sandbox)
                                .join(&wf_raw)
                                .to_string_lossy()
                                .to_string()
                        };
                        let new_project = Project {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: self.new_name.trim().to_string(),
                            description: self.new_description.trim().to_string(),
                            working_folder: resolved_wf,
                            memory: String::new(),
                            skills: Vec::new(),
                            system_prompt: None,
                            agent_override: None,
                            created_at: now.clone(),
                            updated_at: now,
                        };

                        let mut projects = self.projects.clone();
                        projects.push(new_project);
                        runtime.block_on(save_projects(&projects));
                        self.show_create_dialog = false;
                        self.needs_refresh = true;
                    }

                    if ui.button("Cancel").clicked() {
                        self.show_create_dialog = false;
                    }
                });
            });

        if !still_open {
            self.show_create_dialog = false;
        }
    }
}
