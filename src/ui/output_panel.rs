use std::collections::HashMap;
use std::path::PathBuf;
use eframe::egui;

// ---------------------------------------------------------------------------
// OutputPanel — right-side panel showing AI-generated output files
// Supports: PNG/JPG/GIF images, PDF, Word, Markdown, HTML, other files
// ---------------------------------------------------------------------------

pub struct OutputPanel {
    pub open: bool,
    pub width: f32,
    /// Cached textures keyed by relative path
    textures: HashMap<String, egui::TextureHandle>,
    /// Cached markdown content keyed by relative path
    md_cache: HashMap<String, String>,
    /// Track which image is expanded
    expanded_image: Option<String>,
    /// Track which markdown/text file is expanded
    expanded_text: Option<String>,
}

impl Default for OutputPanel {
    fn default() -> Self {
        Self {
            open: false,
            width: 500.0,
            textures: HashMap::new(),
            md_cache: HashMap::new(),
            expanded_image: None,
            expanded_text: None,
        }
    }
}

impl OutputPanel {
    // -----------------------------------------------------------------------
    // File type helpers
    // -----------------------------------------------------------------------

    fn ext(path: &str) -> &str {
        path.rsplit('.').next().unwrap_or("")
    }

    fn is_image(path: &str) -> bool {
        matches!(
            Self::ext(path).to_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg"
        )
    }

    fn is_pdf(path: &str) -> bool {
        Self::ext(path).eq_ignore_ascii_case("pdf")
    }

    fn is_word(path: &str) -> bool {
        matches!(Self::ext(path).to_lowercase().as_str(), "doc" | "docx")
    }

    fn is_markdown(path: &str) -> bool {
        matches!(Self::ext(path).to_lowercase().as_str(), "md" | "markdown")
    }

    fn is_html(path: &str) -> bool {
        matches!(Self::ext(path).to_lowercase().as_str(), "html" | "htm")
    }

    fn is_text(path: &str) -> bool {
        matches!(
            Self::ext(path).to_lowercase().as_str(),
            "txt" | "yaml" | "yml" | "xml" | "py" | "js" | "jsx" | "ts" | "tsx" | "rs" | "css" | "scss"
        )
    }

    fn is_csv(path: &str) -> bool {
        Self::ext(path).eq_ignore_ascii_case("csv")
    }

    fn is_json(path: &str) -> bool {
        Self::ext(path).eq_ignore_ascii_case("json")
    }

    fn filename(path: &str) -> &str {
        path.split('/').last().unwrap_or(path)
    }

    /// Resolve relative path (e.g. "output_file/chart.png") to absolute.
    /// Tries the path as-is first, then resolves relative to common sandbox dirs.
    fn full_path(rel: &str) -> PathBuf {
        let p = PathBuf::from(rel);
        if p.exists() {
            return p;
        }
        // Try resolving relative to sandbox dir
        let sandbox_dir = crate::server::data::get_sandbox_dir_sync();
        let resolved = PathBuf::from(&sandbox_dir).join(rel);
        if resolved.exists() {
            return resolved;
        }
        // Try common fallback locations
        for prefix in &["sandbox", ".", "/tmp/tigrimos_sandbox"] {
            let candidate = PathBuf::from(prefix).join(rel);
            if candidate.exists() {
                return candidate;
            }
        }
        // Return original path even if not found
        p
    }

    fn file_icon(path: &str) -> &'static str {
        if Self::is_image(path)       { "\u{1F5BC}" }  // 🖼
        else if Self::is_pdf(path)    { "\u{1F4C4}" }  // 📄
        else if Self::is_word(path)   { "\u{1F4DD}" }  // 📝
        else if Self::is_markdown(path){ "\u{1F4CB}" } // 📋
        else if Self::is_html(path)   { "\u{1F310}" }  // 🌐
        else if Self::is_csv(path)    { "\u{1F4CA}" }  // 📊
        else if Self::is_json(path)   { "\u{1F4BE}" }  // 💾
        else                          { "\u{1F4C1}" }  // 📁
    }

    // -----------------------------------------------------------------------
    // Top-level show — called from chat.rs with the right-column Ui
    // -----------------------------------------------------------------------

    pub fn show(&mut self, ui: &mut egui::Ui, files: &[String]) {
        let panel_bg   = egui::Color32::from_rgb(248, 249, 250);
        let border     = egui::Color32::from_rgb(225, 228, 232);
        let text_dark  = egui::Color32::from_rgb(31, 35, 40);
        let text_muted = egui::Color32::from_rgb(101, 109, 118);
        let accent     = egui::Color32::from_rgb(88, 166, 255);

        egui::Frame::new()
            .fill(panel_bg)
            .stroke(egui::Stroke::new(1.0, border))
            .inner_margin(egui::Margin::same(0))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());

                // ── Header ──────────────────────────────────────────
                egui::Frame::new()
                    .fill(egui::Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .stroke(egui::Stroke::new(0.0, egui::Color32::TRANSPARENT))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("\u{1F4E4} Outputs")
                                    .size(14.0)
                                    .strong()
                                    .color(text_dark),
                            );
                            // File count badge
                            if !files.is_empty() {
                                egui::Frame::new()
                                    .fill(egui::Color32::from_rgba_premultiplied(88, 166, 255, 25))
                                    .corner_radius(10.0)
                                    .inner_margin(egui::Margin::symmetric(6, 2))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new(files.len().to_string())
                                                .size(11.0)
                                                .strong()
                                                .color(accent),
                                        );
                                    });
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let close_btn = egui::Button::new(
                                        egui::RichText::new("\u{2715}")
                                            .size(12.0)
                                            .color(text_muted),
                                    )
                                    .fill(egui::Color32::TRANSPARENT);
                                    if ui.add(close_btn).clicked() {
                                        self.open = false;
                                    }
                                },
                            );
                        });
                    });

                ui.add(egui::Separator::default().spacing(0.0));

                if files.is_empty() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("\u{1F4C2}")
                                .size(32.0),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("No output files yet")
                                .size(13.0)
                                .color(text_muted),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Files created by AI tools\nwill appear here.")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(150, 160, 170)),
                        );
                    });
                    return;
                }

                // ── File list ────────────────────────────────────────
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt("output_panel_scroll")
                    .show(ui, |ui| {
                        ui.add_space(8.0);
                        ui.set_width(ui.available_width());

                        // Group files by type
                        let images: Vec<&String> = files.iter().filter(|f| Self::is_image(f)).collect();
                        let docs: Vec<&String>   = files.iter().filter(|f| Self::is_pdf(f) || Self::is_word(f)).collect();
                        let mds: Vec<&String>    = files.iter().filter(|f| Self::is_markdown(f)).collect();
                        let csvs: Vec<&String>   = files.iter().filter(|f| Self::is_csv(f)).collect();
                        let jsons: Vec<&String>  = files.iter().filter(|f| Self::is_json(f)).collect();
                        let htmls: Vec<&String>  = files.iter().filter(|f| Self::is_html(f)).collect();
                        let texts: Vec<&String>  = files.iter().filter(|f| Self::is_text(f)).collect();
                        let others: Vec<&String> = files.iter().filter(|f| {
                            !Self::is_image(f) && !Self::is_pdf(f) && !Self::is_word(f)
                            && !Self::is_markdown(f) && !Self::is_html(f)
                            && !Self::is_csv(f) && !Self::is_json(f) && !Self::is_text(f)
                        }).collect();

                        if !images.is_empty() {
                            Self::section_label(ui, "\u{1F5BC} Images", accent);
                            for f in &images {
                                self.render_image_card(ui, f, border);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

                        if !mds.is_empty() {
                            Self::section_label(ui, "\u{1F4CB} Markdown", accent);
                            for f in &mds {
                                self.render_text_card(ui, f, border, true);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

                        if !csvs.is_empty() {
                            Self::section_label(ui, "\u{1F4CA} CSV Data", accent);
                            for f in &csvs {
                                self.render_csv_card(ui, f, border);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

                        if !jsons.is_empty() {
                            Self::section_label(ui, "\u{1F4BE} JSON", accent);
                            for f in &jsons {
                                self.render_text_card(ui, f, border, false);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

                        if !docs.is_empty() {
                            Self::section_label(ui, "\u{1F4C4} Documents", accent);
                            for f in &docs {
                                Self::render_doc_card(ui, f, border);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

                        if !htmls.is_empty() {
                            Self::section_label(ui, "\u{1F310} HTML", accent);
                            for f in &htmls {
                                Self::render_open_card(ui, f, border);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

                        if !texts.is_empty() {
                            Self::section_label(ui, "\u{1F4C3} Text Files", accent);
                            for f in &texts {
                                self.render_text_card(ui, f, border, false);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

                        if !others.is_empty() {
                            Self::section_label(ui, "\u{1F4C1} Other Files", accent);
                            for f in &others {
                                Self::render_open_card(ui, f, border);
                                ui.add_space(6.0);
                            }
                        }

                        ui.add_space(16.0);
                    });
            });
    }

    // -----------------------------------------------------------------------
    // Section label
    // -----------------------------------------------------------------------

    fn section_label(ui: &mut egui::Ui, label: &str, color: egui::Color32) {
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(label)
                    .size(11.0)
                    .strong()
                    .color(color),
            );
        });
        ui.add_space(4.0);
    }

    // -----------------------------------------------------------------------
    // Image card with inline preview
    // -----------------------------------------------------------------------

    fn render_image_card(&mut self, ui: &mut egui::Ui, rel_path: &str, border: egui::Color32) {
        let full = Self::full_path(rel_path);
        let filename = Self::filename(rel_path);

        // Load texture if not cached
        if !self.textures.contains_key(rel_path) && full.exists() {
            if let Ok(data) = std::fs::read(&full) {
                if let Ok(img) = image::load_from_memory(&data) {
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [w as usize, h as usize],
                        rgba.as_raw(),
                    );
                    let texture = ui.ctx().load_texture(
                        rel_path,
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.textures.insert(rel_path.to_string(), texture);
                }
            }
        }

        let is_expanded = self.expanded_image.as_deref() == Some(rel_path);

        egui::Frame::new()
            .fill(egui::Color32::WHITE)
            .corner_radius(8.0)
            .stroke(egui::Stroke::new(1.0, border))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                // Filename + buttons row
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(filename)
                            .size(12.0)
                            .strong()
                            .color(egui::Color32::from_rgb(31, 35, 40)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Open").clicked() {
                            let _ = open::that(&full);
                        }
                        let expand_label = if is_expanded { "▲" } else { "▼" };
                        if ui.small_button(expand_label).clicked() {
                            if is_expanded {
                                self.expanded_image = None;
                            } else {
                                self.expanded_image = Some(rel_path.to_string());
                            }
                        }
                    });
                });

                // Image preview
                if let Some(texture) = self.textures.get(rel_path) {
                    let available_w = ui.available_width();
                    let tex_size = texture.size_vec2();
                    let max_h = if is_expanded { 500.0_f32 } else { 180.0_f32 };
                    let scale = (available_w / tex_size.x).min(max_h / tex_size.y).min(1.0);
                    let display_size = egui::vec2(tex_size.x * scale, tex_size.y * scale);
                    ui.add_space(6.0);
                    ui.add(egui::Image::new(texture).fit_to_exact_size(display_size));
                } else if !full.exists() {
                    ui.label(
                        egui::RichText::new("File not found")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(200, 60, 60)),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("Loading...")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(150, 160, 170)),
                    );
                }
            });
    }

    // -----------------------------------------------------------------------
    // Text/Markdown card with styled rendering and expand toggle
    // -----------------------------------------------------------------------

    fn render_text_card(&mut self, ui: &mut egui::Ui, rel_path: &str, border: egui::Color32, is_markdown: bool) {
        let full = Self::full_path(rel_path);
        let filename = Self::filename(rel_path);
        let is_expanded = self.expanded_text.as_deref() == Some(rel_path);
        let max_h = if is_expanded { 520.0 } else { 220.0 };
        let content = std::fs::read_to_string(&full).unwrap_or_else(|_| "(file not found)".to_string());
        let line_count = content.lines().count();
        let char_count = content.len();

        egui::Frame::new()
            .fill(egui::Color32::WHITE)
            .corner_radius(8.0)
            .stroke(egui::Stroke::new(1.0, border))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                // Header row
                ui.horizontal(|ui| {
                    let icon = if is_markdown { "\u{1F4CB}" } else { "\u{1F4C3}" };
                    ui.label(egui::RichText::new(icon).size(14.0));
                    ui.label(
                        egui::RichText::new(filename)
                            .size(12.0)
                            .strong()
                            .color(egui::Color32::from_rgb(31, 35, 40)),
                    );
                    ui.label(
                        egui::RichText::new(format!("({} lines, {} chars)", line_count, char_count))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(150, 160, 170)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Open").clicked() {
                            let _ = open::that(&full);
                        }
                        let expand_label = if is_expanded { "\u{25B2}" } else { "\u{25BC}" };
                        if ui.small_button(expand_label).clicked() {
                            if is_expanded {
                                self.expanded_text = None;
                            } else {
                                self.expanded_text = Some(rel_path.to_string());
                            }
                        }
                    });
                });
                ui.add(egui::Separator::default().spacing(4.0));

                egui::ScrollArea::vertical()
                    .max_height(max_h)
                    .id_salt(rel_path)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if is_markdown {
                            Self::render_markdown_lines(ui, &content);
                        } else {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&content)
                                        .size(11.5)
                                        .monospace()
                                        .color(egui::Color32::from_rgb(50, 60, 70)),
                                )
                                .wrap(),
                            );
                        }
                    });
            });
    }

    /// Render markdown content with styled headings, bullets, bold, and code blocks
    fn render_markdown_lines(ui: &mut egui::Ui, content: &str) {
        let mut in_code_block = false;
        for line in content.lines() {
            if line.trim_start().starts_with("```") {
                in_code_block = !in_code_block;
                if in_code_block {
                    ui.add_space(2.0);
                }
                continue;
            }
            if in_code_block {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(246, 248, 250))
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(egui::Label::new(
                            egui::RichText::new(line)
                                .size(11.0)
                                .monospace()
                                .color(egui::Color32::from_rgb(180, 100, 40)),
                        ).wrap());
                    });
                continue;
            }
            if line.starts_with("# ") {
                ui.add_space(6.0);
                ui.add(egui::Label::new(
                    egui::RichText::new(&line[2..])
                        .size(16.0)
                        .strong()
                        .color(egui::Color32::from_rgb(13, 17, 23)),
                ).wrap());
                ui.add(egui::Separator::default().spacing(2.0));
            } else if line.starts_with("## ") {
                ui.add_space(4.0);
                ui.add(egui::Label::new(
                    egui::RichText::new(&line[3..])
                        .size(14.0)
                        .strong()
                        .color(egui::Color32::from_rgb(31, 35, 40)),
                ).wrap());
            } else if line.starts_with("### ") {
                ui.add_space(2.0);
                ui.add(egui::Label::new(
                    egui::RichText::new(&line[4..])
                        .size(13.0)
                        .strong()
                        .color(egui::Color32::from_rgb(56, 139, 253)),
                ).wrap());
            } else if line.starts_with("#### ") {
                ui.add(egui::Label::new(
                    egui::RichText::new(&line[5..])
                        .size(12.0)
                        .strong()
                        .color(egui::Color32::from_rgb(100, 116, 139)),
                ).wrap());
            } else if line.starts_with("- ") || line.starts_with("* ") {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("\u{2022}").size(12.0).color(egui::Color32::from_rgb(88, 166, 255)));
                    ui.add_space(2.0);
                    ui.add(egui::Label::new(
                        egui::RichText::new(&line[2..])
                            .size(12.0)
                            .color(egui::Color32::from_rgb(50, 60, 70)),
                    ).wrap());
                });
            } else if line.trim_start_matches(|c: char| c.is_ascii_digit()).starts_with(". ") {
                let trimmed = line.trim_start();
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let num_end = trimmed.find(". ").unwrap_or(0);
                    let num = &trimmed[..num_end];
                    ui.label(egui::RichText::new(format!("{}.", num)).size(12.0).color(egui::Color32::from_rgb(88, 166, 255)));
                    ui.add_space(2.0);
                    ui.add(egui::Label::new(
                        egui::RichText::new(&trimmed[num_end + 2..])
                            .size(12.0)
                            .color(egui::Color32::from_rgb(50, 60, 70)),
                    ).wrap());
                });
            } else if line.starts_with("---") || line.starts_with("===") {
                ui.add(egui::Separator::default().spacing(4.0));
            } else if line.starts_with("> ") {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(240, 246, 255))
                    .inner_margin(egui::Margin::symmetric(8, 2))
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(
                            egui::RichText::new(&line[2..])
                                .size(12.0)
                                .italics()
                                .color(egui::Color32::from_rgb(80, 100, 130)),
                        ).wrap());
                    });
            } else if line.is_empty() {
                ui.add_space(4.0);
            } else {
                // Normal paragraph — strip **bold** markers for display
                let display = line
                    .replace("**", "")
                    .replace("__", "")
                    .replace('`', "");
                let is_bold = line.contains("**") || line.contains("__");
                let text = if is_bold {
                    egui::RichText::new(display).size(12.0).strong().color(egui::Color32::from_rgb(31, 35, 40))
                } else {
                    egui::RichText::new(display).size(12.0).color(egui::Color32::from_rgb(50, 60, 70))
                };
                ui.add(egui::Label::new(text).wrap());
            }
        }
    }

    // -----------------------------------------------------------------------
    // CSV card with table preview
    // -----------------------------------------------------------------------

    fn render_csv_card(&mut self, ui: &mut egui::Ui, rel_path: &str, border: egui::Color32) {
        let full = Self::full_path(rel_path);
        let filename = Self::filename(rel_path);
        let is_expanded = self.expanded_text.as_deref() == Some(rel_path);
        let content = std::fs::read_to_string(&full).unwrap_or_default();
        let rows: Vec<Vec<&str>> = content.lines()
            .take(if is_expanded { 50 } else { 8 })
            .map(|line| line.split(',').collect())
            .collect();
        let col_count = rows.first().map(|r| r.len()).unwrap_or(0);
        let total_rows = content.lines().count();

        egui::Frame::new()
            .fill(egui::Color32::WHITE)
            .corner_radius(8.0)
            .stroke(egui::Stroke::new(1.0, border))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("\u{1F4CA}").size(14.0));
                    ui.label(
                        egui::RichText::new(filename)
                            .size(12.0)
                            .strong()
                            .color(egui::Color32::from_rgb(31, 35, 40)),
                    );
                    ui.label(
                        egui::RichText::new(format!("({} rows \u{00D7} {} cols)", total_rows.saturating_sub(1), col_count))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(150, 160, 170)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Open").clicked() {
                            let _ = open::that(&full);
                        }
                        let expand_label = if is_expanded { "\u{25B2}" } else { "\u{25BC}" };
                        if ui.small_button(expand_label).clicked() {
                            if is_expanded {
                                self.expanded_text = None;
                            } else {
                                self.expanded_text = Some(rel_path.to_string());
                            }
                        }
                    });
                });
                ui.add(egui::Separator::default().spacing(4.0));

                egui::ScrollArea::both()
                    .max_height(if is_expanded { 400.0 } else { 160.0 })
                    .id_salt(rel_path)
                    .show(ui, |ui| {
                        egui::Grid::new(rel_path)
                            .striped(true)
                            .min_col_width(60.0)
                            .max_col_width(180.0)
                            .show(ui, |ui| {
                                for (i, row) in rows.iter().enumerate() {
                                    for cell in row {
                                        let text = cell.trim().trim_matches('"');
                                        if i == 0 {
                                            // Header row
                                            ui.label(
                                                egui::RichText::new(text)
                                                    .size(11.0)
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(56, 139, 253)),
                                            );
                                        } else {
                                            ui.label(
                                                egui::RichText::new(text)
                                                    .size(11.0)
                                                    .color(egui::Color32::from_rgb(50, 60, 70)),
                                            );
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                        if total_rows > (if is_expanded { 50 } else { 8 }) {
                            ui.label(
                                egui::RichText::new(format!("... {} more rows", total_rows - (if is_expanded { 50 } else { 8 })))
                                    .size(10.0)
                                    .color(egui::Color32::GRAY),
                            );
                        }
                    });
            });
    }

    // -----------------------------------------------------------------------
    // Document card (PDF / Word)
    // -----------------------------------------------------------------------

    fn render_doc_card(ui: &mut egui::Ui, rel_path: &str, border: egui::Color32) {
        let full = Self::full_path(rel_path);
        let filename = Self::filename(rel_path);
        let icon = Self::file_icon(rel_path);
        let ext = Self::ext(rel_path).to_uppercase();

        egui::Frame::new()
            .fill(egui::Color32::WHITE)
            .corner_radius(8.0)
            .stroke(egui::Stroke::new(1.0, border))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Icon badge
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_premultiplied(88, 166, 255, 20))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} {}", icon, ext))
                                    .size(11.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(56, 139, 253)),
                            );
                        });

                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(filename)
                                .size(13.0)
                                .strong()
                                .color(egui::Color32::from_rgb(31, 35, 40)),
                        );
                        if full.exists() {
                            if let Ok(meta) = std::fs::metadata(&full) {
                                let kb = meta.len() / 1024;
                                ui.label(
                                    egui::RichText::new(format!("{} KB", kb))
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(150, 160, 170)),
                                );
                            }
                        }
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let open_btn = egui::Button::new(
                            egui::RichText::new("Open")
                                .size(12.0)
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(88, 166, 255))
                        .corner_radius(6.0);
                        if ui.add(open_btn).clicked() {
                            let _ = open::that(&full);
                        }
                    });
                });
            });
    }

    // -----------------------------------------------------------------------
    // Generic open card (HTML, text, other)
    // -----------------------------------------------------------------------

    fn render_open_card(ui: &mut egui::Ui, rel_path: &str, border: egui::Color32) {
        let full = Self::full_path(rel_path);
        let filename = Self::filename(rel_path);
        let icon = Self::file_icon(rel_path);

        egui::Frame::new()
            .fill(egui::Color32::WHITE)
            .corner_radius(8.0)
            .stroke(egui::Stroke::new(1.0, border))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(icon).size(18.0),
                    );
                    ui.label(
                        egui::RichText::new(filename)
                            .size(13.0)
                            .strong()
                            .color(egui::Color32::from_rgb(31, 35, 40)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let open_btn = egui::Button::new(
                            egui::RichText::new("Open")
                                .size(12.0)
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(88, 166, 255))
                        .corner_radius(6.0);
                        if ui.add(open_btn).clicked() {
                            let _ = open::that(&full);
                        }
                    });
                });
            });
    }

    // -----------------------------------------------------------------------
    // Toggle button — shown when panel is closed but files exist
    // -----------------------------------------------------------------------

    pub fn show_toggle_button(&mut self, ui: &mut egui::Ui, file_count: usize) {
        let accent = egui::Color32::from_rgb(88, 166, 255);
        let btn = egui::Button::new(
            egui::RichText::new(format!("\u{1F4E4} {} output{}", file_count, if file_count == 1 { "" } else { "s" }))
                .size(12.0)
                .color(egui::Color32::WHITE),
        )
        .fill(accent)
        .corner_radius(6.0);
        if ui.add(btn).on_hover_text("Show output panel").clicked() {
            self.open = true;
        }
    }
}
