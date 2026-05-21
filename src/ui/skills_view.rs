use eframe::egui;
use crate::server::data::{self, Skill};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

// Simple percent-encoding for query parameters
fn url_encode(input: &str) -> String {
    let mut result = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push('%');
                result.push_str(&format!("{:02X}", byte));
            }
        }
    }
    result
}

// Thread-local channels for Clawhub async results
thread_local! {
    static CLAWHUB_PENDING: RefCell<Option<Arc<Mutex<Option<Result<Vec<ClawhubResult>, String>>>>>> =
        RefCell::new(None);
    static CLAWHUB_INSTALL_PENDING: RefCell<Option<Arc<Mutex<Option<Result<Skill, String>>>>>> =
        RefCell::new(None);
}

// ---------------------------------------------------------------------------
// Sub-tab enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillsTab {
    Installed,
    Catalog,
    Clawhub,
}

// ---------------------------------------------------------------------------
// Clawhub marketplace result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ClawhubResult {
    rank: usize,
    slug: String,
    title: String,
    description: String,
    score: f32,
    installed: bool,
}

pub struct SkillsView {
    skills: Vec<Skill>,
    show_install: bool,
    new_name: String,
    new_description: String,
    needs_refresh: bool,
    // Search / filter
    search_query: String,
    // Selection for detail panel
    selected_skill_id: Option<String>,
    // Delete confirmation
    confirm_delete_id: Option<String>,
    // Content editing mode
    editing_content: bool,
    // Description editing in detail panel
    editing_description: bool,
    // Upload preview state
    upload_preview: Option<UploadPreview>,
    // Sub-tab
    current_tab: SkillsTab,
    // Clawhub marketplace
    clawhub_query: String,
    clawhub_results: Vec<ClawhubResult>,
    clawhub_searching: bool,
    clawhub_status_msg: Option<String>,
    clawhub_installing: Option<String>,
}

/// Temporary state while previewing an uploaded .md file before installing.
struct UploadPreview {
    name: String,
    description: String,
    version: String,
    author: String,
    allowed_tools: Vec<String>,
    content: String,
    file_path: String,
}

/// Built-in catalog entries displayed at the bottom of the view.
struct CatalogEntry {
    name: &'static str,
    description: &'static str,
    category: &'static str,
}

const BUILTIN_CATALOG: &[CatalogEntry] = &[
    // --- Core developer skills ---
    CatalogEntry {
        name: "web-search",
        description: "Search the web using DuckDuckGo — text, news, images, videos with filtering options",
        category: "Research",
    },
    CatalogEntry {
        name: "code-review",
        description: "Analyze code for bugs, security issues, style violations, and improvement suggestions",
        category: "Code Quality",
    },
    CatalogEntry {
        name: "doc-generator",
        description: "Generate README, API docs, module summaries, and inline documentation from source code",
        category: "Documentation",
    },
    CatalogEntry {
        name: "test-scaffold",
        description: "Scaffold unit and integration tests with proper assertions and edge case coverage",
        category: "Testing",
    },
    CatalogEntry {
        name: "debug-assist",
        description: "Analyze errors, stack traces, and logs to identify root causes and suggest fixes",
        category: "Debugging",
    },
    CatalogEntry {
        name: "refactor-bot",
        description: "Suggest and apply refactoring — simplify, extract functions, deduplicate, modernize syntax",
        category: "Code Quality",
    },
    CatalogEntry {
        name: "file-search",
        description: "Find files by name, extension, content, or size across large codebases",
        category: "Utilities",
    },
    CatalogEntry {
        name: "git-summarize",
        description: "Summarize git history into changelogs, release notes, and activity reports",
        category: "Documentation",
    },
    CatalogEntry {
        name: "env-check",
        description: "Validate environment variables, dependencies, and system requirements for a project",
        category: "Debugging",
    },
    // --- Document & data skills ---
    CatalogEntry {
        name: "pdf",
        description: "PDF manipulation toolkit — extract text/tables, create, merge/split, fill forms, and analyze PDF documents",
        category: "Documents",
    },
    CatalogEntry {
        name: "excel---xlsx",
        description: "Create, inspect, and edit Excel workbooks with reliable formulas, dates, formatting, and template preservation",
        category: "Documents",
    },
    CatalogEntry {
        name: "literature-review",
        description: "Search academic sources via Semantic Scholar, OpenAlex, Crossref and PubMed for literature reviews",
        category: "Research",
    },
    CatalogEntry {
        name: "twitter-search",
        description: "Advanced Twitter/X search and social media data analysis — fetch tweets, trend analysis, sentiment",
        category: "Research",
    },
];

impl SkillsView {
    pub fn new() -> Self {
        Self {
            skills: Vec::new(),
            show_install: false,
            new_name: String::new(),
            new_description: String::new(),
            needs_refresh: true,
            search_query: String::new(),
            selected_skill_id: None,
            confirm_delete_id: None,
            editing_content: false,
            editing_description: false,
            upload_preview: None,
            current_tab: SkillsTab::Installed,
            clawhub_query: String::new(),
            clawhub_results: Vec::new(),
            clawhub_searching: false,
            clawhub_status_msg: None,
            clawhub_installing: None,
        }
    }

    // ------------------------------------------------------------------
    // Parse YAML frontmatter from a markdown file
    // ------------------------------------------------------------------
    fn parse_md_frontmatter(content: &str) -> (String, String, String, String, Vec<String>, String) {
        let mut name = String::new();
        let mut description = String::new();
        let mut version = String::new();
        let mut author = String::new();
        let mut allowed_tools: Vec<String> = Vec::new();
        let mut body = content.to_string();

        if content.starts_with("---") {
            if let Some(end) = content[3..].find("---") {
                let frontmatter = &content[3..3 + end];
                body = content[3 + end + 3..].trim_start().to_string();

                let mut in_allowed_tools = false;
                for line in frontmatter.lines() {
                    let line = line.trim();
                    if let Some(val) = line.strip_prefix("name:") {
                        in_allowed_tools = false;
                        name = val.trim().trim_matches('"').trim_matches('\'').to_string();
                    } else if let Some(val) = line.strip_prefix("description:") {
                        in_allowed_tools = false;
                        description = val.trim().trim_matches('"').trim_matches('\'').to_string();
                    } else if let Some(val) = line.strip_prefix("version:") {
                        in_allowed_tools = false;
                        version = val.trim().trim_matches('"').trim_matches('\'').to_string();
                    } else if let Some(val) = line.strip_prefix("author:") {
                        in_allowed_tools = false;
                        author = val.trim().trim_matches('"').trim_matches('\'').to_string();
                    } else if let Some(val) = line.strip_prefix("allowed_tools:") {
                        in_allowed_tools = true;
                        let val = val.trim();
                        if val.starts_with('[') {
                            // Inline array: [tool1, tool2]
                            let inner = val.trim_start_matches('[').trim_end_matches(']');
                            allowed_tools = inner
                                .split(',')
                                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            in_allowed_tools = false;
                        }
                    } else if line.starts_with("- ") && in_allowed_tools {
                        // YAML list items for allowed_tools
                        allowed_tools.push(
                            line.trim_start_matches("- ")
                                .trim_matches('"')
                                .trim_matches('\'')
                                .to_string(),
                        );
                    } else if !line.starts_with("- ") && !line.is_empty() {
                        in_allowed_tools = false;
                    }
                }
            }
        }

        (name, description, version, author, allowed_tools, body)
    }

    // ------------------------------------------------------------------
    // Source badge helper
    // ------------------------------------------------------------------
    fn source_badge(ui: &mut egui::Ui, source: &str) {
        let (badge_text, badge_color) = match source {
            "upload" => ("upload", egui::Color32::from_rgb(59, 130, 246)),
            "builtin" => ("built-in", egui::Color32::from_rgb(34, 197, 94)),
            "auto" => ("auto", egui::Color32::from_rgb(168, 85, 247)),
            "manual" => ("manual", egui::Color32::from_rgb(249, 115, 22)),
            "clawhub" => ("clawhub", egui::Color32::from_rgb(236, 72, 153)),
            _ => ("unknown", egui::Color32::from_rgb(156, 163, 175)),
        };

        egui::Frame::new()
            .fill(badge_color.gamma_multiply(0.15))
            .inner_margin(egui::Margin::symmetric(6, 2))
            .corner_radius(3.0)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(badge_text)
                        .small()
                        .color(badge_color)
                        .strong(),
                );
            });
    }

    fn category_badge(ui: &mut egui::Ui, category: &str) {
        let color = egui::Color32::from_rgb(100, 116, 139);
        egui::Frame::new()
            .fill(color.gamma_multiply(0.12))
            .inner_margin(egui::Margin::symmetric(5, 1))
            .corner_radius(3.0)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(category)
                        .size(10.0)
                        .color(color),
                );
            });
    }

    // ------------------------------------------------------------------
    // Main show
    // ------------------------------------------------------------------
    pub fn show(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // ---------- refresh skills from data layer ----------
        // Always reload skills to pick up auto-generated skills
        {
            let fresh = runtime.block_on(data::get_skills());
            if fresh.len() != self.skills.len() || self.needs_refresh {
                self.skills = fresh;
                self.needs_refresh = false;
            }
        }

        // ---------- header ----------
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading("Skills Management");
                ui.label(
                    egui::RichText::new("Install, configure and manage agent skills")
                        .size(12.0)
                        .color(egui::Color32::GRAY),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Upload .md button
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Upload Skill")
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(59, 130, 246)),
                    )
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Skill files", &["md", "zip"])
                        .pick_file()
                    {
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                        if ext == "zip" {
                            // Handle ZIP: extract SKILL.md from inside
                            if let Ok(bytes) = std::fs::read(&path) {
                                let cursor = std::io::Cursor::new(&bytes);
                                if let Ok(mut archive) = zip::ZipArchive::new(cursor) {
                                    let mut skill_content = String::new();
                                    for i in 0..archive.len() {
                                        if let Ok(mut file) = archive.by_index(i) {
                                            let fname = file.name().to_string();
                                            if fname.ends_with("SKILL.md") || fname.ends_with("skill.md") {
                                                use std::io::Read;
                                                let _ = file.read_to_string(&mut skill_content);
                                                break;
                                            }
                                        }
                                    }
                                    if skill_content.is_empty() {
                                        tracing::warn!("ZIP does not contain SKILL.md");
                                    } else {
                                        let (name, description, version, author, allowed_tools, body) =
                                            Self::parse_md_frontmatter(&skill_content);
                                        let file_name = if name.is_empty() {
                                            path.file_stem()
                                                .map(|s| s.to_string_lossy().to_string())
                                                .unwrap_or_else(|| "uploaded-skill".to_string())
                                        } else {
                                            name
                                        };
                                        self.upload_preview = Some(UploadPreview {
                                            name: file_name,
                                            description: if description.is_empty() {
                                                format!("Uploaded from {}", path.display())
                                            } else {
                                                description
                                            },
                                            version,
                                            author,
                                            allowed_tools,
                                            content: body,
                                            file_path: path.display().to_string(),
                                        });
                                    }
                                }
                            }
                        } else if let Ok(content) = std::fs::read_to_string(&path) {
                            let (name, description, version, author, allowed_tools, body) =
                                Self::parse_md_frontmatter(&content);

                            let file_name = if name.is_empty() {
                                path.file_stem()
                                    .map(|s| s.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "uploaded-skill".to_string())
                            } else {
                                name
                            };

                            self.upload_preview = Some(UploadPreview {
                                name: file_name,
                                description: if description.is_empty() {
                                    format!("Uploaded from {}", path.display())
                                } else {
                                    description
                                },
                                version,
                                author,
                                allowed_tools,
                                content: body,
                                file_path: path.display().to_string(),
                            });
                        }
                    }
                }

                // Manual install button
                if ui
                    .add(
                        egui::Button::new("+ Install Skill"),
                    )
                    .clicked()
                {
                    self.show_install = true;
                    self.new_name.clear();
                    self.new_description.clear();
                }
            });
        });

        ui.separator();

        // ---------- sub-tabs ----------
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.current_tab == SkillsTab::Installed, "Installed")
                .clicked()
            {
                self.current_tab = SkillsTab::Installed;
            }
            if ui
                .selectable_label(self.current_tab == SkillsTab::Catalog, "Catalog")
                .clicked()
            {
                self.current_tab = SkillsTab::Catalog;
            }
            if ui
                .selectable_label(self.current_tab == SkillsTab::Clawhub, "Clawhub")
                .clicked()
            {
                self.current_tab = SkillsTab::Clawhub;
            }
        });

        ui.add_space(4.0);

        // ---------- tab content ----------
        match self.current_tab {
            SkillsTab::Installed => self.show_installed_tab(ui, runtime),
            SkillsTab::Catalog => self.show_catalog_tab(ui, runtime),
            SkillsTab::Clawhub => self.show_clawhub_tab(ui, runtime),
        }
    }

    // ------------------------------------------------------------------
    // Installed tab (original two-panel layout)
    // ------------------------------------------------------------------
    fn show_installed_tab(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // ---------- search bar ----------
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .desired_width(300.0)
                    .hint_text("Filter skills by name..."),
            );
            if !self.search_query.is_empty() {
                if ui.small_button("Clear").clicked() {
                    self.search_query.clear();
                }
            }
        });

        ui.add_space(4.0);

        // ---------- upload preview dialog ----------
        if self.upload_preview.is_some() {
            self.show_upload_preview(ui, runtime);
        }

        // ---------- install dialog ----------
        if self.show_install {
            self.show_install_dialog(ui, runtime);
        }

        // ---------- layout: skill list left, detail right ----------
        let has_selection = self.selected_skill_id.is_some();
        let full_width = ui.available_width();
        let list_width = if has_selection {
            (250.0_f32).max(full_width * 0.2).min(full_width * 0.3)
        } else {
            full_width
        };

        ui.horizontal_top(|ui| {
            // Skill list
            ui.allocate_ui_with_layout(
                egui::vec2(list_width, ui.available_height()),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.strong("Installed Skills");
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .id_salt("skills_list_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.show_skill_list(ui, runtime);
                        });
                },
            );

            // Detail panel on right
            if has_selection {
                ui.separator();
                let detail_width = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(detail_width, ui.available_height()),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("skills_detail_scroll")
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

    // ------------------------------------------------------------------
    // Catalog tab
    // ------------------------------------------------------------------
    fn show_catalog_tab(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // ---------- search bar ----------
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .desired_width(300.0)
                    .hint_text("Filter catalog by name..."),
            );
            if !self.search_query.is_empty() {
                if ui.small_button("Clear").clicked() {
                    self.search_query.clear();
                }
            }
        });

        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .id_salt("catalog_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.show_catalog(ui, runtime);
            });
    }

    // ------------------------------------------------------------------
    // Clawhub marketplace tab
    // ------------------------------------------------------------------
    fn show_clawhub_tab(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        // --- Search bar ---
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.clawhub_query)
                    .desired_width(300.0)
                    .hint_text("Search skills on Clawhub..."),
            );

            let enter_pressed = response.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));

            let search_clicked = ui
                .add_enabled(
                    !self.clawhub_searching && !self.clawhub_query.is_empty(),
                    egui::Button::new("Search"),
                )
                .clicked();

            if (search_clicked || enter_pressed)
                && !self.clawhub_query.is_empty()
                && !self.clawhub_searching
            {
                self.clawhub_searching = true;
                self.clawhub_status_msg = Some("Searching...".to_string());
                self.clawhub_results.clear();

                let query = self.clawhub_query.clone();
                let ctx = ui.ctx().clone();

                // We need a shared way to pass results back. Use a simple
                // channel-like approach via Arc<Mutex<>>.
                let results_slot: std::sync::Arc<
                    std::sync::Mutex<Option<Result<Vec<ClawhubResult>, String>>>,
                > = std::sync::Arc::new(std::sync::Mutex::new(None));
                let slot = results_slot.clone();

                runtime.spawn(async move {
                    let url = format!(
                        "http://localhost:3001/api/clawhub/search?q={}",
                        url_encode(&query)
                    );

                    let outcome = match reqwest::get(&url).await {
                        Ok(resp) => {
                            if resp.status().is_success() {
                                match resp.json::<serde_json::Value>().await {
                                    Ok(json) => {
                                        let mut items = Vec::new();
                                        if let Some(arr) = json.as_array()
                                            .or_else(|| json.get("results").and_then(|v| v.as_array()))
                                        {
                                            for (i, entry) in arr.iter().enumerate() {
                                                items.push(ClawhubResult {
                                                    rank: i + 1,
                                                    slug: entry
                                                        .get("slug")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("")
                                                        .to_string(),
                                                    title: entry
                                                        .get("title")
                                                        .or_else(|| entry.get("name"))
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("Untitled")
                                                        .to_string(),
                                                    description: entry
                                                        .get("description")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("")
                                                        .to_string(),
                                                    score: entry
                                                        .get("score")
                                                        .and_then(|v| v.as_f64())
                                                        .unwrap_or(0.0)
                                                        as f32,
                                                    installed: false,
                                                });
                                            }
                                        }
                                        Ok(items)
                                    }
                                    Err(e) => Err(format!("Failed to parse response: {e}")),
                                }
                            } else {
                                Err(format!("Server returned {}", resp.status()))
                            }
                        }
                        Err(e) => Err(format!("Connection error: {e}")),
                    };

                    *slot.lock().unwrap() = Some(outcome);
                    ctx.request_repaint();
                });

                // Store the slot so we can poll it on subsequent frames.
                // We piggy-back on clawhub_installing as a sentinel — but
                // a cleaner approach is to store the Arc. Since we cannot
                // easily add generic Arc fields after the fact, we use a
                // static thread-local for the pending result channel.
                CLAWHUB_PENDING.with(|cell| {
                    *cell.borrow_mut() = Some(results_slot);
                });
            }

            if self.clawhub_searching {
                ui.spinner();
            }
        });

        // --- Poll for async results ---
        if self.clawhub_searching {
            let maybe_result = CLAWHUB_PENDING.with(|cell| {
                let borrow = cell.borrow();
                if let Some(ref slot) = *borrow {
                    let lock = slot.lock().unwrap();
                    lock.clone()
                } else {
                    None
                }
            });

            if let Some(result) = maybe_result {
                self.clawhub_searching = false;
                CLAWHUB_PENDING.with(|cell| {
                    *cell.borrow_mut() = None;
                });

                match result {
                    Ok(results) => {
                        // Mark already-installed skills
                        let installed_names: Vec<String> =
                            self.skills.iter().map(|s| s.name.clone()).collect();
                        self.clawhub_results = results
                            .into_iter()
                            .map(|mut r| {
                                r.installed = installed_names.contains(&r.slug)
                                    || installed_names.contains(&r.title);
                                r
                            })
                            .collect();
                        let count = self.clawhub_results.len();
                        self.clawhub_status_msg =
                            Some(format!("Found {} result(s)", count));
                    }
                    Err(e) => {
                        self.clawhub_status_msg = Some(format!("Error: {e}"));
                    }
                }
            }
        }

        // --- Status message ---
        if let Some(ref msg) = self.clawhub_status_msg {
            ui.add_space(4.0);
            let color = if msg.starts_with("Error") {
                egui::Color32::from_rgb(239, 68, 68)
            } else {
                egui::Color32::GRAY
            };
            ui.label(egui::RichText::new(msg).size(11.0).color(color));
        }

        ui.add_space(6.0);

        // --- Results list ---
        if self.clawhub_results.is_empty() && !self.clawhub_searching {
            if self.clawhub_status_msg.is_none() {
                ui.label(
                    egui::RichText::new(
                        "Search the Clawhub marketplace to discover community skills.",
                    )
                    .weak(),
                );
            }
        } else {
            egui::ScrollArea::vertical()
                .id_salt("clawhub_results_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let mut install_slug: Option<String> = None;

                    for result in &self.clawhub_results {
                        egui::Frame::new()
                            .fill(ui.visuals().faint_bg_color)
                            .inner_margin(egui::Margin::same(10))
                            .corner_radius(5.0)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    // Rank badge
                                    let rank_color = match result.rank {
                                        1 => egui::Color32::from_rgb(255, 215, 0),   // gold
                                        2 => egui::Color32::from_rgb(192, 192, 192), // silver
                                        3 => egui::Color32::from_rgb(205, 127, 50),  // bronze
                                        _ => egui::Color32::from_rgb(100, 116, 139),
                                    };
                                    egui::Frame::new()
                                        .fill(rank_color.gamma_multiply(0.2))
                                        .inner_margin(egui::Margin::symmetric(6, 2))
                                        .corner_radius(3.0)
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(format!("#{}", result.rank))
                                                    .strong()
                                                    .color(rank_color),
                                            );
                                        });

                                    // Title & description
                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.strong(&result.title);
                                            ui.label(
                                                egui::RichText::new(&result.slug)
                                                    .small()
                                                    .weak(),
                                            );
                                        });
                                        if !result.description.is_empty() {
                                            ui.label(
                                                egui::RichText::new(&result.description)
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                    });

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            // Install / Installed button
                                            if result.installed {
                                                egui::Frame::new()
                                                    .fill(
                                                        egui::Color32::from_rgb(34, 197, 94)
                                                            .gamma_multiply(0.15),
                                                    )
                                                    .inner_margin(egui::Margin::symmetric(6, 2))
                                                    .corner_radius(3.0)
                                                    .show(ui, |ui| {
                                                        ui.label(
                                                            egui::RichText::new("Installed")
                                                                .small()
                                                                .color(egui::Color32::from_rgb(
                                                                    34, 197, 94,
                                                                ))
                                                                .strong(),
                                                        );
                                                    });
                                            } else {
                                                let is_installing = self
                                                    .clawhub_installing
                                                    .as_deref()
                                                    == Some(&result.slug);

                                                if is_installing {
                                                    ui.spinner();
                                                } else if ui
                                                    .add(
                                                        egui::Button::new(
                                                            egui::RichText::new("Install")
                                                                .color(egui::Color32::WHITE),
                                                        )
                                                        .fill(egui::Color32::from_rgb(
                                                            59, 130, 246,
                                                        )),
                                                    )
                                                    .clicked()
                                                {
                                                    install_slug = Some(result.slug.clone());
                                                }
                                            }

                                            // Score badge
                                            let score_color = if result.score >= 3.5 {
                                                egui::Color32::from_rgb(34, 197, 94) // green
                                            } else if result.score >= 2.5 {
                                                egui::Color32::from_rgb(234, 179, 8) // yellow
                                            } else {
                                                egui::Color32::from_rgb(239, 68, 68) // red
                                            };
                                            egui::Frame::new()
                                                .fill(score_color.gamma_multiply(0.15))
                                                .inner_margin(egui::Margin::symmetric(6, 2))
                                                .corner_radius(3.0)
                                                .show(ui, |ui| {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "{:.1}",
                                                            result.score
                                                        ))
                                                        .small()
                                                        .color(score_color)
                                                        .strong(),
                                                    );
                                                });
                                        },
                                    );
                                });
                            });

                        ui.add_space(4.0);
                    }

                    // Handle install action
                    if let Some(slug) = install_slug {
                        self.clawhub_installing = Some(slug.clone());
                        let ctx = ui.ctx().clone();
                        let install_slot: std::sync::Arc<
                            std::sync::Mutex<Option<Result<Skill, String>>>,
                        > = std::sync::Arc::new(std::sync::Mutex::new(None));
                        let slot = install_slot.clone();

                        runtime.spawn(async move {
                            let client = reqwest::Client::new();
                            let outcome = match client
                                .post("http://localhost:3001/api/clawhub/install")
                                .json(&serde_json::json!({ "slug": slug }))
                                .send()
                                .await
                            {
                                Ok(resp) => {
                                    if resp.status().is_success() {
                                        match resp.json::<serde_json::Value>().await {
                                            Ok(json) => {
                                                let name = json
                                                    .get("name")
                                                    .or_else(|| json.get("slug"))
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or(&slug)
                                                    .to_string();
                                                let description = json
                                                    .get("description")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("Installed from Clawhub")
                                                    .to_string();
                                                let content = json
                                                    .get("content")
                                                    .or_else(|| json.get("script"))
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("")
                                                    .to_string();

                                                Ok(Skill {
                                                    id: uuid::Uuid::new_v4().to_string(),
                                                    name,
                                                    description,
                                                    source: "clawhub".to_string(),
                                                    script: content,
                                                    enabled: true,
                                                    installed_at: chrono::Utc::now().to_rfc3339(),
                                                    review_status: Some("pending".to_string()),
                                                    auto_meta: None,
                                                })
                                            }
                                            Err(e) => {
                                                Err(format!("Failed to parse install response: {e}"))
                                            }
                                        }
                                    } else {
                                        Err(format!("Install failed: HTTP {}", resp.status()))
                                    }
                                }
                                Err(e) => Err(format!("Connection error: {e}")),
                            };

                            *slot.lock().unwrap() = Some(outcome);
                            ctx.request_repaint();
                        });

                        CLAWHUB_INSTALL_PENDING.with(|cell| {
                            *cell.borrow_mut() = Some(install_slot);
                        });
                    }
                });
        }

        // --- Poll for install result ---
        if self.clawhub_installing.is_some() {
            let maybe_result = CLAWHUB_INSTALL_PENDING.with(|cell| {
                let borrow = cell.borrow();
                if let Some(ref slot) = *borrow {
                    let lock = slot.lock().unwrap();
                    lock.clone()
                } else {
                    None
                }
            });

            if let Some(result) = maybe_result {
                let installed_slug = self.clawhub_installing.take().unwrap_or_default();
                CLAWHUB_INSTALL_PENDING.with(|cell| {
                    *cell.borrow_mut() = None;
                });

                match result {
                    Ok(skill) => {
                        self.skills.push(skill);
                        let skills = self.skills.clone();
                        runtime.spawn(async move {
                            data::save_skills(&skills).await;
                        });

                        // Mark as installed in results
                        for r in &mut self.clawhub_results {
                            if r.slug == installed_slug {
                                r.installed = true;
                            }
                        }

                        self.clawhub_status_msg =
                            Some(format!("Installed '{installed_slug}' successfully"));
                    }
                    Err(e) => {
                        self.clawhub_status_msg = Some(format!("Install error: {e}"));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Upload preview window
    // ------------------------------------------------------------------
    fn show_upload_preview(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let mut still_open = true;
        let mut do_install = false;

        if let Some(preview) = &mut self.upload_preview {
            egui::Window::new("Upload Preview")
                .resizable(true)
                .collapsible(false)
                .default_size([500.0, 420.0])
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.heading("Skill Preview");
                    ui.add_space(4.0);

                    egui::Grid::new("upload_preview_grid")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Name:");
                            ui.text_edit_singleline(&mut preview.name);
                            ui.end_row();

                            ui.label("Description:");
                            ui.text_edit_singleline(&mut preview.description);
                            ui.end_row();

                            ui.label("Version:");
                            ui.label(if preview.version.is_empty() {
                                "-"
                            } else {
                                &preview.version
                            });
                            ui.end_row();

                            ui.label("Author:");
                            ui.label(if preview.author.is_empty() {
                                "-"
                            } else {
                                &preview.author
                            });
                            ui.end_row();

                            ui.label("Source:");
                            ui.label(&preview.file_path);
                            ui.end_row();

                            if !preview.allowed_tools.is_empty() {
                                ui.label("Allowed Tools:");
                                ui.label(preview.allowed_tools.join(", "));
                                ui.end_row();
                            }
                        });

                    ui.add_space(8.0);
                    ui.label("Content Preview:");
                    let preview_text = if preview.content.len() > 2000 {
                        format!("{}...\n\n(truncated)", &preview.content[..2000])
                    } else {
                        preview.content.clone()
                    };
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut preview_text.as_str())
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Monospace)
                                    .interactive(false),
                            );
                        });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Install Skill")
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(34, 197, 94)),
                            )
                            .clicked()
                        {
                            do_install = true;
                        }

                        if ui.button("Cancel").clicked() {
                            still_open = false;
                        }
                    });
                });
        }

        if do_install {
            if let Some(preview) = self.upload_preview.take() {
                let slug = preview.name.to_lowercase()
                    .chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect::<String>();
                let slug = slug.trim_matches('-').to_string();

                // Save SKILL.md and supporting files to data/skills/<slug>/
                let skill_dir = data::data_dir().join("skills").join(&slug);
                let content_for_disk = preview.content.clone();
                let file_path = preview.file_path.clone();
                let skill_dir_clone = skill_dir.clone();
                runtime.spawn(async move {
                    let _ = tokio::fs::create_dir_all(&skill_dir_clone).await;
                    // Write SKILL.md
                    let _ = tokio::fs::write(skill_dir_clone.join("SKILL.md"), &content_for_disk).await;
                    // If source was a ZIP, also extract supporting files
                    if file_path.ends_with(".zip") {
                        if let Ok(bytes) = tokio::fs::read(&file_path).await {
                            let cursor = std::io::Cursor::new(&bytes);
                            if let Ok(mut archive) = zip::ZipArchive::new(cursor) {
                                // Find root prefix
                                let mut root_prefix = String::new();
                                for i in 0..archive.len() {
                                    if let Ok(f) = archive.by_index(i) {
                                        let n = f.name().to_string();
                                        if n.ends_with("SKILL.md") || n.ends_with("skill.md") {
                                            if let Some(pos) = n.rfind('/') {
                                                root_prefix = n[..=pos].to_string();
                                            }
                                            break;
                                        }
                                    }
                                }
                                for i in 0..archive.len() {
                                    if let Ok(mut f) = archive.by_index(i) {
                                        if f.is_dir() { continue; }
                                        let raw = f.name().to_string();
                                        let rel = raw.strip_prefix(&root_prefix).unwrap_or(&raw);
                                        if rel.is_empty() || rel == "SKILL.md" { continue; }
                                        let dest = skill_dir_clone.join(rel);
                                        if let Some(parent) = dest.parent() {
                                            let _ = std::fs::create_dir_all(parent);
                                        }
                                        use std::io::Read;
                                        let mut buf = Vec::new();
                                        let _ = f.read_to_end(&mut buf);
                                        let _ = std::fs::write(&dest, &buf);
                                    }
                                }
                            }
                        }
                    }
                });

                let skill = Skill {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: preview.name,
                    description: preview.description,
                    source: "upload".to_string(),
                    script: slug.clone(),
                    enabled: true,
                    installed_at: chrono::Utc::now().to_rfc3339(),
                    review_status: Some("pending".to_string()),
                    auto_meta: None,
                };
                self.skills.push(skill);
                let skills = self.skills.clone();
                runtime.spawn(async move {
                    data::save_skills(&skills).await;
                });
            }
        } else if !still_open {
            self.upload_preview = None;
        }
    }

    // ------------------------------------------------------------------
    // Manual install dialog
    // ------------------------------------------------------------------
    fn show_install_dialog(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::same(12))
            .corner_radius(6.0)
            .show(ui, |ui| {
                ui.heading("Install New Skill");
                ui.add_space(4.0);

                egui::Grid::new("new_skill_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut self.new_name);
                        ui.end_row();

                        ui.label("Description:");
                        ui.text_edit_singleline(&mut self.new_description);
                        ui.end_row();
                    });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let can_save =
                        !self.new_name.is_empty() && !self.new_description.is_empty();

                    if ui
                        .add_enabled(can_save, egui::Button::new("Install"))
                        .clicked()
                    {
                        let skill = Skill {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: self.new_name.clone(),
                            description: self.new_description.clone(),
                            source: "manual".to_string(),
                            script: String::new(),
                            enabled: true,
                            installed_at: chrono::Utc::now().to_rfc3339(),
                            review_status: None,
                            auto_meta: None,
                        };
                        self.skills.push(skill);
                        let skills = self.skills.clone();
                        runtime.spawn(async move {
                            data::save_skills(&skills).await;
                        });
                        self.show_install = false;
                    }

                    if ui.button("Cancel").clicked() {
                        self.show_install = false;
                    }
                });
            });

        ui.add_space(8.0);
    }

    // ------------------------------------------------------------------
    // Left panel: installed skills list
    // ------------------------------------------------------------------
    fn show_skill_list(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        if self.skills.is_empty() {
            ui.label("No skills installed yet.");
            return;
        }

        let query_lower = self.search_query.to_lowercase();
        let mut clicked_id: Option<String> = None;
        let mut toggled = false;

        for (_i, skill) in self.skills.iter_mut().enumerate() {
            // Filter by search query
            if !query_lower.is_empty() && !skill.name.to_lowercase().contains(&query_lower) {
                continue;
            }

            let is_selected = self
                .selected_skill_id
                .as_deref()
                == Some(&skill.id);

            let frame = egui::Frame::new()
                .inner_margin(egui::Margin::same(10))
                .corner_radius(5.0)
                .fill(if is_selected {
                    egui::Color32::from_rgb(37, 99, 235).gamma_multiply(0.18)
                } else {
                    ui.visuals().faint_bg_color
                })
                .stroke(egui::Stroke::new(
                    1.0,
                    if is_selected {
                        egui::Color32::from_rgb(59, 130, 246)
                    } else {
                        egui::Color32::TRANSPARENT
                    },
                ));

            let resp = frame
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Enabled toggle
                        if ui.checkbox(&mut skill.enabled, "").changed() {
                            toggled = true;
                        }

                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong(&skill.name);
                                Self::source_badge(ui, &skill.source);
                                if !skill.enabled {
                                    ui.label(
                                        egui::RichText::new("disabled")
                                            .small()
                                            .color(egui::Color32::from_rgb(239, 68, 68)),
                                    );
                                }
                            });
                            ui.label(
                                egui::RichText::new(&skill.description)
                                    .small()
                                    .weak(),
                            );
                        });
                    });
                })
                .response;

            if resp.interact(egui::Sense::click()).clicked() {
                clicked_id = Some(skill.id.clone());
            }

            ui.add_space(3.0);
        }

        if toggled {
            let skills = self.skills.clone();
            runtime.spawn(async move {
                data::save_skills(&skills).await;
            });
        }

        if let Some(id) = clicked_id {
            if self.selected_skill_id.as_deref() == Some(&id) {
                // Deselect on second click
                self.selected_skill_id = None;
            } else {
                self.selected_skill_id = Some(id);
                self.editing_content = false;
                self.editing_description = false;
                self.confirm_delete_id = None;
            }
        }
    }

    // ------------------------------------------------------------------
    // Left panel: full catalog (built-in + all installed skills)
    // ------------------------------------------------------------------
    fn show_catalog(&mut self, ui: &mut egui::Ui, _runtime: &tokio::runtime::Handle) {
        let query_lower = self.search_query.to_lowercase();

        // Collect built-in names for dedup
        let builtin_names: Vec<&str> = BUILTIN_CATALOG.iter().map(|e| e.name).collect();

        // --- Built-in skills section ---
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Built-in Skills")
                .size(13.0)
                .strong()
                .color(egui::Color32::from_rgb(34, 197, 94)),
        );
        ui.add_space(4.0);

        for entry in BUILTIN_CATALOG {
            if !query_lower.is_empty()
                && !entry.name.to_lowercase().contains(&query_lower)
                && !entry.description.to_lowercase().contains(&query_lower)
            {
                continue;
            }
            Self::render_catalog_card(ui, entry.name, entry.description, "builtin", entry.category);
            ui.add_space(3.0);
        }

        // --- User-installed skills section ---
        let user_skills: Vec<&Skill> = self.skills.iter()
            .filter(|s| !builtin_names.contains(&s.name.as_str()))
            .collect();

        if !user_skills.is_empty() {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("User Skills")
                    .size(13.0)
                    .strong()
                    .color(egui::Color32::from_rgb(59, 130, 246)),
            );
            ui.add_space(4.0);

            for skill in &user_skills {
                if !query_lower.is_empty()
                    && !skill.name.to_lowercase().contains(&query_lower)
                    && !skill.description.to_lowercase().contains(&query_lower)
                {
                    continue;
                }
                let category = Self::infer_category(&skill.source);
                Self::render_catalog_card(ui, &skill.name, &skill.description, &skill.source, category);
                ui.add_space(3.0);
            }
        }
    }

    /// Render a single catalog card
    fn render_catalog_card(ui: &mut egui::Ui, name: &str, description: &str, source: &str, category: &str) {
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(egui::Margin::same(8))
            .corner_radius(4.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.strong(name);
                            Self::source_badge(ui, source);
                            Self::category_badge(ui, category);
                        });
                        // Show description truncated to ~120 chars
                        let desc = if description.len() > 120 {
                            format!("{}...", &description[..description.char_indices().nth(120).map(|(i,_)|i).unwrap_or(description.len())])
                        } else {
                            description.to_string()
                        };
                        ui.label(egui::RichText::new(desc).small().weak());
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(34, 197, 94).gamma_multiply(0.15))
                            .inner_margin(egui::Margin::symmetric(6, 2))
                            .corner_radius(3.0)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new("Installed")
                                        .small()
                                        .color(egui::Color32::from_rgb(34, 197, 94))
                                        .strong(),
                                );
                            });
                    });
                });
            });
    }

    /// Infer a display category from skill source
    fn infer_category(source: &str) -> &'static str {
        match source {
            "builtin" | "bundled" => "built-in",
            "auto" => "auto-created",
            "upload" => "uploaded",
            "clawhub" => "clawhub",
            "manual" => "manual",
            _ => "custom",
        }
    }

    // ------------------------------------------------------------------
    // Right panel: skill detail
    // ------------------------------------------------------------------
    fn show_detail_panel(&mut self, ui: &mut egui::Ui, runtime: &tokio::runtime::Handle) {
        let sel_id = match &self.selected_skill_id {
            Some(id) => id.clone(),
            None => return,
        };

        let Some(idx) = self.skills.iter().position(|s| s.id == sel_id) else {
            self.selected_skill_id = None;
            return;
        };

        // Close button
        ui.horizontal(|ui| {
            ui.heading(self.skills[idx].name.clone());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.selected_skill_id = None;
                    self.editing_content = false;
                    self.editing_description = false;
                    self.confirm_delete_id = None;
                }
            });
        });

        ui.add_space(8.0);

        // --- Detail fields grid ---
        egui::Grid::new("skill_detail_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Name:");
                ui.label(
                    egui::RichText::new(&self.skills[idx].name)
                        .strong()
                        .size(14.0),
                );
                ui.end_row();

                ui.label("Source:");
                ui.horizontal(|ui| {
                    Self::source_badge(ui, &self.skills[idx].source);
                });
                ui.end_row();

                ui.label("Installed At:");
                ui.label(
                    egui::RichText::new(&self.skills[idx].installed_at)
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                ui.end_row();

                if let Some(ref status) = self.skills[idx].review_status {
                    ui.label("Review Status:");
                    ui.label(
                        egui::RichText::new(status)
                            .size(12.0)
                            .color(egui::Color32::from_rgb(251, 191, 36)),
                    );
                    ui.end_row();
                }

                // Enabled toggle
                ui.label("Enabled:");
                let mut enabled = self.skills[idx].enabled;
                let enabled_label = if enabled { "Yes" } else { "No" };
                if ui.checkbox(&mut enabled, enabled_label).changed() {
                    self.skills[idx].enabled = enabled;
                    let skills = self.skills.clone();
                    runtime.spawn(async move {
                        data::save_skills(&skills).await;
                    });
                }
                ui.end_row();
            });

        ui.add_space(12.0);

        // --- Description section ---
        ui.horizontal(|ui| {
            ui.strong("Description");
            if ui
                .small_button(if self.editing_description { "Done" } else { "Edit" })
                .clicked()
            {
                if self.editing_description {
                    // Save on finish
                    let skills = self.skills.clone();
                    runtime.spawn(async move {
                        data::save_skills(&skills).await;
                    });
                }
                self.editing_description = !self.editing_description;
            }
        });
        ui.add_space(2.0);

        if self.editing_description {
            ui.add(
                egui::TextEdit::multiline(&mut self.skills[idx].description)
                    .desired_width(f32::INFINITY)
                    .desired_rows(3),
            );
        } else {
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::same(8))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    ui.label(&self.skills[idx].description);
                });
        }

        ui.add_space(12.0);

        // --- Auto meta info (if present) ---
        if let Some(ref meta) = self.skills[idx].auto_meta {
            ui.strong("Auto-Generation Metadata");
            ui.add_space(2.0);
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::same(8))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    egui::Grid::new("auto_meta_grid")
                        .num_columns(2)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Kind:");
                            ui.label(&meta.kind);
                            ui.end_row();

                            ui.label("Model:");
                            ui.label(&meta.model);
                            ui.end_row();

                            ui.label("Generated At:");
                            ui.label(&meta.generated_at);
                            ui.end_row();

                            if !meta.based_on.is_empty() {
                                ui.label("Based On:");
                                ui.label(meta.based_on.join(", "));
                                ui.end_row();
                            }

                            if let Some(ref rationale) = meta.rationale {
                                ui.label("Rationale:");
                                ui.label(rationale);
                                ui.end_row();
                            }
                        });
                });
            ui.add_space(12.0);
        }

        // --- SKILL.md Content viewer ---
        // Try to load SKILL.md from data/skills/{name}/ for folder-based skills
        let skill_name = self.skills[idx].name.clone();
        let skill_source = self.skills[idx].source.clone();
        let slug = skill_name.to_lowercase()
            .chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect::<String>();
        let skill_dir = data::data_dir().join("skills").join(slug.trim_matches('-'));
        let skill_md_path = skill_dir.join("SKILL.md");
        let skill_md_content = if skill_source != "built-in" {
            std::fs::read_to_string(&skill_md_path).ok()
        } else {
            None
        };
        // Collect all files in the skill subfolder
        let skill_files: Vec<(String, String)> = if skill_source != "built-in" && skill_dir.is_dir() {
            let mut files = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&skill_dir) {
                for entry in entries.flatten() {
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if fname == "SKILL.md" { continue; } // shown above already
                    let fpath = entry.path();
                    if fpath.is_file() {
                        files.push((fname, fpath.display().to_string()));
                    }
                }
            }
            files.sort_by(|a, b| a.0.cmp(&b.0));
            files
        } else {
            Vec::new()
        };

        let display_content = skill_md_content
            .clone()
            .unwrap_or_else(|| self.skills[idx].script.clone());

        ui.horizontal(|ui| {
            ui.strong("SKILL.md Content");
            if skill_md_content.is_some() {
                ui.label(
                    egui::RichText::new(format!("({})", skill_md_path.display()))
                        .size(10.0)
                        .color(egui::Color32::GRAY),
                );
            }
            if ui
                .small_button(if self.editing_content { "Save & Lock" } else { "Edit" })
                .clicked()
            {
                if self.editing_content {
                    // Save content — write to SKILL.md file if it's a folder-based skill
                    if skill_md_content.is_some() {
                        let path = skill_md_path.clone();
                        let content = self.skills[idx].script.clone();
                        runtime.spawn(async move {
                            let _ = tokio::fs::write(path, content).await;
                        });
                    }
                    let skills = self.skills.clone();
                    runtime.spawn(async move {
                        data::save_skills(&skills).await;
                    });
                } else {
                    // Load content into script field for editing
                    if let Some(ref content) = skill_md_content {
                        self.skills[idx].script = content.clone();
                    }
                }
                self.editing_content = !self.editing_content;
            }
        });
        ui.add_space(2.0);

        if display_content.is_empty() && !self.editing_content {
            ui.label(
                egui::RichText::new("No content / script defined.")
                    .small()
                    .weak(),
            );
        } else {
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::same(8))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("skill_content_scroll")
                        .max_height(400.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if self.editing_content {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.skills[idx].script)
                                        .desired_width(f32::INFINITY)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_rows(20),
                                );
                            } else {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(display_content)
                                            .size(13.0)
                                            .monospace()
                                            .color(egui::Color32::from_rgb(209, 213, 219)),
                                    )
                                    .wrap()
                                    .selectable(true),
                                );
                            }
                        });
                });
        }

        // --- Skill Files listing ---
        if !skill_files.is_empty() {
            ui.add_space(12.0);
            ui.strong(format!("\u{1F4C2} Skill Files ({})", skill_files.len()));
            ui.add_space(4.0);
            for (fname, fpath) in &skill_files {
                let is_md = fname.ends_with(".md");
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .inner_margin(egui::Margin::same(8))
                    .corner_radius(4.0)
                    .stroke(egui::Stroke::new(0.5, egui::Color32::from_rgb(200, 205, 210)))
                    .show(ui, |ui| {
                        let icon = if is_md { "\u{1F4CB}" }
                            else if fname.ends_with(".py") { "\u{1F40D}" }
                            else if fname.ends_with(".json") { "\u{1F4BE}" }
                            else { "\u{1F4C4}" };
                        let header_id = ui.make_persistent_id(format!("skill_file_{}", fname));
                        egui::collapsing_header::CollapsingState::load_with_default_open(
                            ui.ctx(), header_id, false,
                        ).show_header(ui, |ui| {
                            ui.label(egui::RichText::new(format!("{} {}", icon, fname)).size(12.0).strong());
                        }).body(|ui| {
                            if let Ok(content) = std::fs::read_to_string(fpath) {
                                egui::ScrollArea::vertical()
                                    .id_salt(format!("skill_file_scroll_{}", fname))
                                    .max_height(300.0)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&content)
                                                    .size(11.0)
                                                    .monospace()
                                                    .color(egui::Color32::from_rgb(180, 185, 195)),
                                            )
                                            .wrap()
                                            .selectable(true),
                                        );
                                    });
                            } else {
                                ui.label(
                                    egui::RichText::new("(unable to read file)")
                                        .small()
                                        .weak(),
                                );
                            }
                        });
                    });
                ui.add_space(4.0);
            }
        }

        ui.add_space(16.0);

        // --- Action buttons ---
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if self.confirm_delete_id.as_deref() == Some(&sel_id) {
                // Confirmation inline
                ui.label(
                    egui::RichText::new("Delete this skill?")
                        .color(egui::Color32::from_rgb(220, 38, 38)),
                );
                if ui.button("Cancel").clicked() {
                    self.confirm_delete_id = None;
                }
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Yes, Delete")
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(220, 38, 38)),
                    )
                    .clicked()
                {
                    // Also delete skill files from disk
                    let skill_name = self.skills.iter().find(|s| s.id == sel_id)
                        .map(|s| s.name.clone()).unwrap_or_default();
                    let slug = skill_name.to_lowercase()
                        .chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect::<String>();
                    let skill_dir = data::data_dir().join("skills").join(slug.trim_matches('-'));
                    let _ = std::fs::remove_dir_all(&skill_dir);

                    self.skills.retain(|s| s.id != sel_id);
                    let skills = self.skills.clone();
                    runtime.spawn(async move {
                        data::save_skills(&skills).await;
                    });
                    self.selected_skill_id = None;
                    self.confirm_delete_id = None;
                    self.editing_content = false;
                    self.editing_description = false;
                }
            } else {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Delete Skill")
                                .color(egui::Color32::from_rgb(239, 68, 68)),
                        ),
                    )
                    .clicked()
                {
                    self.confirm_delete_id = Some(sel_id.clone());
                }
            }
        });
    }
}
