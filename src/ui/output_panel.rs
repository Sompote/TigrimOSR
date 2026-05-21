use std::collections::HashMap;
use std::path::PathBuf;
use eframe::egui;

// ---------------------------------------------------------------------------
// OutputPanel — right-side panel showing AI-generated output files
// Supports: PNG/JPG/GIF images, PDF, Word, Markdown, HTML, React charts, other files
// ---------------------------------------------------------------------------

/// Parsed chart data from a React/Recharts JSX file
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct ParsedChart {
    title: String,
    datasets: Vec<ChartDataset>,
    chart_type: ChartType,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct ChartDataset {
    label: String,
    points: Vec<(String, f64)>, // (x_label, y_value)
    color: egui::Color32,
}

#[derive(Clone, Debug, PartialEq)]
enum ChartType {
    Line,
    Bar,
    Area,
    Pie,
}

/// Cached parsed chart data
#[allow(dead_code)]
struct ChartCache {
    charts: Vec<ParsedChart>,
}

#[allow(dead_code)]
pub struct OutputPanel {
    pub open: bool,
    pub width: f32,
    textures: HashMap<String, egui::TextureHandle>,
    md_cache: HashMap<String, String>,
    expanded_image: Option<String>,
    expanded_text: Option<String>,
    /// Cached parsed React chart data keyed by file path
    chart_cache: HashMap<String, ChartCache>,
    /// Track which React card is expanded
    expanded_react: Option<String>,
    /// Cached extracted PDF text keyed by file path
    pdf_cache: HashMap<String, String>,
    /// Cached Excel sheet data: path -> vec of (sheet_name, rows)
    excel_cache: HashMap<String, Vec<(String, Vec<Vec<String>>)>>,
    /// Track which doc card is expanded
    expanded_doc: Option<String>,
    /// PDF page rendering: path -> (total_pages, HashMap<page_num, texture>)
    pdf_pages: HashMap<String, (usize, HashMap<usize, egui::TextureHandle>)>,
    /// Current displayed page per PDF
    pdf_current_page: HashMap<String, usize>,
    /// PDF pages currently being rendered (to avoid duplicate spawns)
    pdf_rendering: HashMap<String, bool>,
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
            chart_cache: HashMap::new(),
            expanded_react: None,
            pdf_cache: HashMap::new(),
            excel_cache: HashMap::new(),
            expanded_doc: None,
            pdf_pages: HashMap::new(),
            pdf_current_page: HashMap::new(),
            pdf_rendering: HashMap::new(),
        }
    }
}

// Default chart color palette
const CHART_COLORS: &[(u8, u8, u8)] = &[
    (99, 102, 241),   // indigo
    (139, 92, 246),   // violet
    (168, 85, 247),   // purple
    (236, 72, 153),   // pink
    (34, 197, 94),    // green
    (59, 130, 246),   // blue
    (245, 158, 11),   // amber
    (239, 68, 68),    // red
    (20, 184, 166),   // teal
    (249, 115, 22),   // orange
];

fn chart_color(idx: usize) -> egui::Color32 {
    let (r, g, b) = CHART_COLORS[idx % CHART_COLORS.len()];
    egui::Color32::from_rgb(r, g, b)
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

    fn is_excel(path: &str) -> bool {
        matches!(Self::ext(path).to_lowercase().as_str(), "xls" | "xlsx" | "xlsm" | "xlsb" | "ods")
    }

    fn is_markdown(path: &str) -> bool {
        matches!(Self::ext(path).to_lowercase().as_str(), "md" | "markdown")
    }

    fn is_html(path: &str) -> bool {
        matches!(Self::ext(path).to_lowercase().as_str(), "html" | "htm")
    }

    fn is_react(path: &str) -> bool {
        let lower = path.to_lowercase();
        lower.ends_with(".jsx.js") || lower.ends_with(".jsx") || lower.ends_with(".tsx")
    }

    fn is_text(path: &str) -> bool {
        if Self::is_react(path) {
            return false;
        }
        matches!(
            Self::ext(path).to_lowercase().as_str(),
            "txt" | "yaml" | "yml" | "xml" | "py" | "js" | "ts" | "rs" | "css" | "scss"
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

    fn full_path(rel: &str) -> PathBuf {
        let p = PathBuf::from(rel);
        if p.exists() {
            return p;
        }
        let sandbox_dir = crate::server::data::get_sandbox_dir_sync();
        let resolved = PathBuf::from(&sandbox_dir).join(rel);
        if resolved.exists() {
            return resolved;
        }
        for prefix in &["sandbox", ".", "/tmp/tigrimos_sandbox"] {
            let candidate = PathBuf::from(prefix).join(rel);
            if candidate.exists() {
                return candidate;
            }
        }
        p
    }

    fn file_icon(path: &str) -> &'static str {
        if Self::is_image(path)       { "\u{1F5BC}" }
        else if Self::is_pdf(path)    { "\u{1F4C4}" }
        else if Self::is_word(path)   { "\u{1F4DD}" }
        else if Self::is_markdown(path){ "\u{1F4CB}" }
        else if Self::is_html(path)   { "\u{1F310}" }
        else if Self::is_csv(path)    { "\u{1F4CA}" }
        else if Self::is_json(path)   { "\u{1F4BE}" }
        else                          { "\u{1F4C1}" }
    }

    // -----------------------------------------------------------------------
    // Top-level show
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

                // -- Header --
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
                        ui.label(egui::RichText::new("\u{1F4C2}").size(32.0));
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

                // -- File list --
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt("output_panel_scroll")
                    .show(ui, |ui| {
                        ui.add_space(8.0);
                        ui.set_width(ui.available_width());

                        let images: Vec<&String> = files.iter().filter(|f| Self::is_image(f)).collect();
                        let pdfs: Vec<&String>   = files.iter().filter(|f| Self::is_pdf(f)).collect();
                        let excels: Vec<&String> = files.iter().filter(|f| Self::is_excel(f)).collect();
                        let docs: Vec<&String>   = files.iter().filter(|f| Self::is_word(f)).collect();
                        let mds: Vec<&String>    = files.iter().filter(|f| Self::is_markdown(f)).collect();
                        let csvs: Vec<&String>   = files.iter().filter(|f| Self::is_csv(f)).collect();
                        let jsons: Vec<&String>  = files.iter().filter(|f| Self::is_json(f)).collect();
                        let htmls: Vec<&String>  = files.iter().filter(|f| Self::is_html(f)).collect();
                        let reacts: Vec<&String> = files.iter().filter(|f| Self::is_react(f)).collect();
                        let texts: Vec<&String>  = files.iter().filter(|f| Self::is_text(f)).collect();
                        let others: Vec<&String> = files.iter().filter(|f| {
                            !Self::is_image(f) && !Self::is_pdf(f) && !Self::is_word(f)
                            && !Self::is_excel(f)
                            && !Self::is_markdown(f) && !Self::is_html(f)
                            && !Self::is_csv(f) && !Self::is_json(f) && !Self::is_text(f)
                            && !Self::is_react(f)
                        }).collect();

                        if !reacts.is_empty() {
                            Self::section_label(ui, "\u{1F4CA} Charts", accent);
                            for f in &reacts {
                                self.render_react_card(ui, f, border);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

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

                        if !pdfs.is_empty() {
                            Self::section_label(ui, "\u{1F4C4} PDF Documents", accent);
                            for f in &pdfs {
                                self.render_pdf_card(ui, f, border);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

                        if !excels.is_empty() {
                            Self::section_label(ui, "\u{1F4CA} Spreadsheets", accent);
                            for f in &excels {
                                self.render_excel_card(ui, f, border);
                                ui.add_space(6.0);
                            }
                            ui.add_space(4.0);
                        }

                        if !docs.is_empty() {
                            Self::section_label(ui, "\u{1F4DD} Word Documents", accent);
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

        if !self.textures.contains_key(rel_path) {
            if full.exists() {
                match std::fs::read(&full) {
                    Ok(data) => {
                        match image::load_from_memory(&data) {
                            Ok(img) => {
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
                            Err(e) => eprintln!("[OutputPanel] image decode error: {}", e),
                        }
                    }
                    Err(e) => eprintln!("[OutputPanel] file read error: {}", e),
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
                        let expand_label = if is_expanded { "\u{25B2}" } else { "\u{25BC}" };
                        if ui.small_button(expand_label).clicked() {
                            if is_expanded {
                                self.expanded_image = None;
                            } else {
                                self.expanded_image = Some(rel_path.to_string());
                            }
                        }
                    });
                });

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
    // React chart card — parses JSX data and renders native egui charts
    // -----------------------------------------------------------------------

    fn render_react_card(&mut self, ui: &mut egui::Ui, rel_path: &str, border: egui::Color32) {
        let full = Self::full_path(rel_path);
        let filename = Self::filename(rel_path);
        let content = std::fs::read_to_string(&full).unwrap_or_default();

        // Parse and cache chart data
        if !self.chart_cache.contains_key(rel_path) {
            let charts = Self::parse_react_charts(&content);
            self.chart_cache.insert(rel_path.to_string(), ChartCache { charts });
        }

        // Extract title from __REACT_META__
        let title = if let Some(meta_start) = content.find("__REACT_META__=") {
            let json_start = content[meta_start..].find('{').map(|i| meta_start + i);
            let json_end = json_start.and_then(|s| content[s..].find('}').map(|i| s + i + 1));
            if let (Some(s), Some(e)) = (json_start, json_end) {
                serde_json::from_str::<serde_json::Value>(&content[s..e])
                    .ok()
                    .and_then(|v| v.get("title").and_then(|t| t.as_str()).map(String::from))
                    .unwrap_or_else(|| filename.to_string())
            } else {
                filename.to_string()
            }
        } else {
            filename.to_string()
        };

        let is_expanded = self.expanded_react.as_deref() == Some(rel_path);
        let cached = self.chart_cache.get(rel_path);
        let chart_count = cached.map(|c| c.charts.len()).unwrap_or(0);

        egui::Frame::new()
            .fill(egui::Color32::from_rgb(22, 27, 34))
            .corner_radius(8.0)
            .stroke(egui::Stroke::new(1.0, border))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                // Header
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&title)
                            .size(13.0)
                            .strong()
                            .color(egui::Color32::from_rgb(230, 237, 243)),
                    );
                    ui.label(
                        egui::RichText::new(format!("({} chart{})", chart_count, if chart_count == 1 { "" } else { "s" }))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(125, 133, 144)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Open in browser button
                        let browser_btn = egui::Button::new(
                            egui::RichText::new("Browser")
                                .size(11.0)
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(88, 166, 255))
                        .corner_radius(4.0);
                        if ui.add(browser_btn).on_hover_text("Open interactive version in browser").clicked() {
                            let encoded_path = rel_path.replace(' ', "%20");
                            let url = format!("http://localhost:3001/api/files/preview?path={}", encoded_path);
                            let _ = open::that(&url);
                        }

                        let expand_label = if is_expanded { "\u{25B2}" } else { "\u{25BC}" };
                        let expand_btn = egui::Button::new(
                            egui::RichText::new(expand_label)
                                .size(11.0)
                                .color(egui::Color32::from_rgb(200, 210, 220)),
                        ).fill(egui::Color32::TRANSPARENT);
                        if ui.add(expand_btn).clicked() {
                            if is_expanded {
                                self.expanded_react = None;
                            } else {
                                self.expanded_react = Some(rel_path.to_string());
                            }
                        }
                    });
                });

                ui.add_space(8.0);

                // Render charts
                if let Some(cache) = self.chart_cache.get(rel_path) {
                    let chart_height = if is_expanded { 280.0 } else { 180.0 };
                    for (ci, chart) in cache.charts.iter().enumerate() {
                        if ci > 0 {
                            ui.add_space(10.0);
                        }
                        // Chart sub-title
                        if cache.charts.len() > 1 {
                            ui.label(
                                egui::RichText::new(&chart.title)
                                    .size(11.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(180, 190, 200)),
                            );
                            ui.add_space(4.0);
                        }

                        match chart.chart_type {
                            ChartType::Pie => Self::render_pie_chart(ui, chart, chart_height, rel_path, ci),
                            _ => Self::render_plot_chart(ui, chart, chart_height, rel_path, ci),
                        }
                    }
                }

                if chart_count == 0 {
                    ui.label(
                        egui::RichText::new("No chart data found in file")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(150, 160, 170)),
                    );
                }
            });
    }

    // -----------------------------------------------------------------------
    // Parse React/Recharts JSX to extract chart data
    // -----------------------------------------------------------------------

    fn parse_react_charts(content: &str) -> Vec<ParsedChart> {
        let mut charts = Vec::new();

        // 1) Find all const data arrays: const varName = [ { ... }, ... ];
        let data_arrays = Self::extract_js_arrays(content);

        // 2) Find chart components and their data sources
        //    Pattern: <LineChart data={varName}> or <BarChart data={growthData}>
        let chart_refs = Self::extract_chart_refs(content);

        // 3) Match chart types to data
        for (chart_type, data_var, data_keys) in &chart_refs {
            if let Some(arr) = data_arrays.get(data_var.as_str()) {
                let mut datasets = Vec::new();
                for (ki, key) in data_keys.iter().enumerate() {
                    let points: Vec<(String, f64)> = arr
                        .iter()
                        .filter_map(|obj| {
                            let label = Self::get_label_from_obj(obj);
                            let val = obj.get(key.as_str())
                                .and_then(|v| v.as_f64())?;
                            Some((label, val))
                        })
                        .collect();
                    if !points.is_empty() {
                        datasets.push(ChartDataset {
                            label: key.clone(),
                            points,
                            color: chart_color(ki),
                        });
                    }
                }

                if !datasets.is_empty() {
                    let title = Self::humanize_var_name(data_var);
                    charts.push(ParsedChart {
                        title,
                        datasets,
                        chart_type: chart_type.clone(),
                    });
                }
            }
        }

        // If no chart refs found but we have data arrays, auto-detect
        if charts.is_empty() && !data_arrays.is_empty() {
            for (var_name, arr) in &data_arrays {
                if arr.is_empty() { continue; }
                let first = &arr[0];
                let numeric_keys: Vec<String> = first.as_object()
                    .map(|obj| {
                        obj.iter()
                            .filter(|(_, v)| v.is_f64() || v.is_i64() || v.is_u64())
                            .map(|(k, _)| k.clone())
                            .collect()
                    })
                    .unwrap_or_default();

                if numeric_keys.is_empty() { continue; }

                let mut datasets = Vec::new();
                for (ki, key) in numeric_keys.iter().enumerate() {
                    let points: Vec<(String, f64)> = arr
                        .iter()
                        .filter_map(|obj| {
                            let label = Self::get_label_from_obj(obj);
                            let val = obj.get(key.as_str())
                                .and_then(|v| {
                                    if v.is_f64() { v.as_f64() }
                                    else if v.is_i64() { v.as_i64().map(|i| i as f64) }
                                    else { v.as_u64().map(|u| u as f64) }
                                })?;
                            Some((label, val))
                        })
                        .collect();
                    if !points.is_empty() {
                        datasets.push(ChartDataset {
                            label: key.clone(),
                            points,
                            color: chart_color(ki),
                        });
                    }
                }

                if !datasets.is_empty() {
                    // Try to guess chart type based on content
                    let chart_type = if content.contains("PieChart") && arr.len() <= 10 {
                        ChartType::Pie
                    } else if content.contains("BarChart") {
                        ChartType::Bar
                    } else if content.contains("AreaChart") {
                        ChartType::Area
                    } else {
                        ChartType::Line
                    };
                    charts.push(ParsedChart {
                        title: Self::humanize_var_name(var_name),
                        datasets,
                        chart_type,
                    });
                }
            }
        }

        charts
    }

    /// Extract JS array constants: `const name = [ {..}, {..} ];`
    fn extract_js_arrays(content: &str) -> HashMap<String, Vec<serde_json::Value>> {
        let mut result = HashMap::new();
        let re_const = regex::Regex::new(r"(?m)^(?:const|let|var)\s+(\w+)\s*=\s*\[").unwrap();
        for cap in re_const.captures_iter(content) {
            let var_name = cap[1].to_string();
            let start_pos = cap.get(0).unwrap().end() - 1; // position of '['
            if let Some(arr_str) = Self::extract_balanced(content, start_pos, '[', ']') {
                // Fix JS syntax to valid JSON: remove trailing commas, handle single quotes
                let json_str = Self::js_array_to_json(&arr_str);
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&json_str) {
                    if !arr.is_empty() && arr[0].is_object() {
                        result.insert(var_name, arr);
                    }
                }
            }
        }
        result
    }

    /// Extract chart component references: <LineChart data={varName}>
    fn extract_chart_refs(content: &str) -> Vec<(ChartType, String, Vec<String>)> {
        let mut refs = Vec::new();
        let chart_types = [
            ("LineChart", ChartType::Line),
            ("BarChart", ChartType::Bar),
            ("AreaChart", ChartType::Area),
            ("PieChart", ChartType::Pie),
        ];

        for (tag, ct) in &chart_types {
            // Find <LineChart data={varName}> or <BarChart data={varName}>
            let pattern = format!(r"<{}\s[^>]*data=\{{(\w+)\}}", tag);
            let re = regex::Regex::new(&pattern).unwrap();
            for cap in re.captures_iter(content) {
                let data_var = cap[1].to_string();

                // Find data keys: <Line dataKey="price" /> or <Bar dataKey="revenue" />
                let data_keys = Self::extract_data_keys(content, tag);

                if !data_keys.is_empty() {
                    refs.push((ct.clone(), data_var, data_keys));
                }
            }
        }

        // Handle Pie separately — it uses data prop on <Pie> not <PieChart>
        let pie_re = regex::Regex::new(r#"<Pie\s[^>]*data=\{(\w+)\}[^>]*dataKey=["'](\w+)["']"#).unwrap();
        for cap in pie_re.captures_iter(content) {
            let data_var = cap[1].to_string();
            let data_key = cap[2].to_string();
            // Check if already added
            if !refs.iter().any(|(_, v, _)| v == &data_var) {
                refs.push((ChartType::Pie, data_var, vec![data_key]));
            }
        }

        refs
    }

    /// Extract dataKey attributes from Line/Bar/Area child components
    fn extract_data_keys(content: &str, chart_tag: &str) -> Vec<String> {
        let child_tag = match chart_tag {
            "LineChart" => "Line",
            "BarChart" => "Bar",
            "AreaChart" => "Area",
            _ => return vec![],
        };
        let pattern = format!(r#"<{}\s[^>]*dataKey=["'](\w+)["']"#, child_tag);
        let re = regex::Regex::new(&pattern).unwrap();
        re.captures_iter(content)
            .map(|cap| cap[1].to_string())
            .collect()
    }

    /// Get the label (first string field) from a JSON object
    fn get_label_from_obj(obj: &serde_json::Value) -> String {
        if let Some(map) = obj.as_object() {
            // Try common label field names first
            for key in &["name", "label", "month", "date", "year", "segment", "category", "x"] {
                if let Some(v) = map.get(*key) {
                    if let Some(s) = v.as_str() {
                        return s.to_string();
                    }
                    if let Some(n) = v.as_f64() {
                        return format!("{}", n);
                    }
                }
            }
            // Fall back to first string field
            for (_, v) in map {
                if let Some(s) = v.as_str() {
                    return s.to_string();
                }
            }
        }
        String::new()
    }

    /// Extract balanced brackets from content
    fn extract_balanced(content: &str, start: usize, open: char, close: char) -> Option<String> {
        let bytes = content.as_bytes();
        if start >= bytes.len() || bytes[start] as char != open {
            return None;
        }
        let mut depth = 0;
        let mut in_string = false;
        let mut escape = false;
        let mut string_char = '"';
        for (i, ch) in content[start..].char_indices() {
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' && in_string {
                escape = true;
                continue;
            }
            if !in_string && (ch == '"' || ch == '\'') {
                in_string = true;
                string_char = ch;
                continue;
            }
            if in_string && ch == string_char {
                in_string = false;
                continue;
            }
            if !in_string {
                if ch == open { depth += 1; }
                if ch == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(content[start..start + i + 1].to_string());
                    }
                }
            }
        }
        None
    }

    /// Convert JS object/array literal to valid JSON using regex transforms
    fn js_array_to_json(js: &str) -> String {
        let mut s = js.to_string();
        // Remove single-line comments
        let re_comment = regex::Regex::new(r"//[^\n]*").unwrap();
        s = re_comment.replace_all(&s, "").to_string();
        // Quote unquoted property names: { name: -> { "name":
        let re_key = regex::Regex::new(r"(?m)(\{|,)\s*(\w+)\s*:").unwrap();
        // Need multiple passes since replacements may overlap
        for _ in 0..5 {
            let new = re_key.replace_all(&s, r#"$1 "$2":"#).to_string();
            if new == s { break; }
            s = new;
        }
        // Replace single quotes with double quotes (outside existing double-quoted strings)
        s = Self::replace_single_quotes(&s);
        // Remove trailing commas before } or ]
        let re_trail = regex::Regex::new(r",\s*([}\]])").unwrap();
        s = re_trail.replace_all(&s, "$1").to_string();
        s
    }

    /// Replace single-quoted strings with double-quoted strings
    fn replace_single_quotes(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '\'' {
                // Start of single-quoted string — collect until closing '
                result.push('"');
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '"' {
                        result.push('\\');
                        result.push('"');
                    } else if chars[i] == '\\' && i + 1 < chars.len() {
                        result.push(chars[i]);
                        i += 1;
                        result.push(chars[i]);
                    } else {
                        result.push(chars[i]);
                    }
                    i += 1;
                }
                result.push('"');
                if i < chars.len() { i += 1; } // skip closing '
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    /// Convert camelCase/snake_case var name to readable title
    fn humanize_var_name(name: &str) -> String {
        let mut result = String::new();
        for (i, ch) in name.chars().enumerate() {
            if ch == '_' {
                result.push(' ');
            } else if ch.is_uppercase() && i > 0 {
                result.push(' ');
                result.push(ch);
            } else if i == 0 {
                result.extend(ch.to_uppercase());
            } else {
                result.push(ch);
            }
        }
        result
    }

    // -----------------------------------------------------------------------
    // Render Bar/Line/Area chart using painter API (beautiful custom rendering)
    // -----------------------------------------------------------------------

    fn render_plot_chart(ui: &mut egui::Ui, chart: &ParsedChart, height: f32, _id_base: &str, _idx: usize) {
        let is_bar = chart.chart_type == ChartType::Bar;
        let bg = egui::Color32::from_rgb(15, 23, 42);     // slate-900
        let grid_color = egui::Color32::from_rgb(51, 65, 85); // slate-700
        let label_color = egui::Color32::from_rgb(148, 163, 184); // slate-400

        egui::Frame::new()
            .fill(bg)
            .corner_radius(12.0)
            .inner_margin(egui::Margin::same(0))
            .show(ui, |ui| {
                let avail_w = ui.available_width();
                let total_h = height + 30.0; // extra for x-axis labels
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(avail_w, total_h),
                    egui::Sense::hover(),
                );
                let painter = ui.painter_at(rect);

                // Chart area with margins
                let margin_l = 50.0_f32;
                let margin_r = 20.0_f32;
                let margin_t = 12.0_f32;
                let margin_b = 36.0_f32;
                let chart_rect = egui::Rect::from_min_max(
                    rect.min + egui::vec2(margin_l, margin_t),
                    rect.max - egui::vec2(margin_r, margin_b),
                );
                let chart_w = chart_rect.width();
                let chart_h = chart_rect.height();

                // Find data range
                let all_values: Vec<f64> = chart.datasets.iter()
                    .flat_map(|ds| ds.points.iter().map(|(_, v)| *v))
                    .collect();
                let y_min = 0.0_f64;
                let y_max = all_values.iter().cloned().fold(0.0_f64, f64::max) * 1.15;
                if y_max <= 0.0 { return; }
                let n_points = chart.datasets.first().map(|ds| ds.points.len()).unwrap_or(0);
                if n_points == 0 { return; }

                // Draw grid lines (4 horizontal lines)
                for i in 0..=4 {
                    let frac = i as f32 / 4.0;
                    let y = chart_rect.bottom() - frac * chart_h;
                    painter.line_segment(
                        [egui::pos2(chart_rect.left(), y), egui::pos2(chart_rect.right(), y)],
                        egui::Stroke::new(0.5, grid_color),
                    );
                    let val = y_min + (y_max - y_min) * frac as f64;
                    let label = if val >= 1000.0 { format!("{:.0}k", val / 1000.0) }
                                else if val >= 1.0 { format!("{:.0}", val) }
                                else { format!("{:.1}", val) };
                    painter.text(
                        egui::pos2(chart_rect.left() - 6.0, y),
                        egui::Align2::RIGHT_CENTER,
                        label,
                        egui::FontId::proportional(9.0),
                        label_color,
                    );
                }

                let num_datasets = chart.datasets.len();

                if is_bar {
                    // Bar chart — each data point gets one or more colored bars
                    let group_w = chart_w / n_points as f32;
                    let bar_gap = 3.0_f32;
                    let bar_w = ((group_w - bar_gap * 2.0) / num_datasets as f32).min(40.0).max(6.0);

                    for (di, ds) in chart.datasets.iter().enumerate() {
                        for (pi, (label, value)) in ds.points.iter().enumerate() {
                            let bar_h = (*value / y_max) as f32 * chart_h;
                            let x_center = chart_rect.left() + group_w * (pi as f32 + 0.5);
                            let x_offset = if num_datasets > 1 {
                                (di as f32 - (num_datasets - 1) as f32 / 2.0) * (bar_w + 2.0)
                            } else { 0.0 };
                            let bar_x = x_center + x_offset - bar_w / 2.0;
                            let bar_rect = egui::Rect::from_min_max(
                                egui::pos2(bar_x, chart_rect.bottom() - bar_h),
                                egui::pos2(bar_x + bar_w, chart_rect.bottom()),
                            );

                            // Use per-item color if only one dataset, else per-dataset color
                            let color = if num_datasets == 1 { chart_color(pi) } else { ds.color };

                            // Rounded top corners
                            painter.rect_filled(bar_rect, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, color);

                            // Value on top of bar
                            if bar_h > 20.0 {
                                let val_text = if *value >= 1000.0 { format!("{:.0}k", value / 1000.0) }
                                              else if *value == (*value as i64) as f64 { format!("{:.0}", value) }
                                              else { format!("{:.1}", value) };
                                painter.text(
                                    egui::pos2(x_center + x_offset, bar_rect.top() - 4.0),
                                    egui::Align2::CENTER_BOTTOM,
                                    val_text,
                                    egui::FontId::proportional(8.5),
                                    egui::Color32::from_rgb(220, 230, 240),
                                );
                            }

                            // X-axis label (only for first dataset)
                            if di == 0 {
                                painter.text(
                                    egui::pos2(x_center, chart_rect.bottom() + 14.0),
                                    egui::Align2::CENTER_CENTER,
                                    Self::truncate_label(label, 12),
                                    egui::FontId::proportional(9.0),
                                    label_color,
                                );
                            }
                        }
                    }
                } else {
                    // Line / Area chart
                    for (_di, ds) in chart.datasets.iter().enumerate() {
                        let screen_points: Vec<egui::Pos2> = ds.points.iter().enumerate()
                            .map(|(i, (_, v))| {
                                let x = chart_rect.left() + chart_w * (i as f32 / (n_points - 1).max(1) as f32);
                                let y = chart_rect.bottom() - (*v / y_max) as f32 * chart_h;
                                egui::pos2(x, y)
                            })
                            .collect();

                        if chart.chart_type == ChartType::Area && screen_points.len() >= 2 {
                            // Fill area under the line
                            let mut fill_pts = screen_points.clone();
                            fill_pts.push(egui::pos2(chart_rect.right(), chart_rect.bottom()));
                            fill_pts.push(egui::pos2(chart_rect.left(), chart_rect.bottom()));
                            let fill_color = egui::Color32::from_rgba_premultiplied(
                                ds.color.r(), ds.color.g(), ds.color.b(), 40,
                            );
                            painter.add(egui::Shape::convex_polygon(fill_pts, fill_color, egui::Stroke::NONE));
                        }

                        // Draw line segments
                        for pair in screen_points.windows(2) {
                            painter.line_segment(
                                [pair[0], pair[1]],
                                egui::Stroke::new(2.5, ds.color),
                            );
                        }

                        // Draw data points
                        for pt in &screen_points {
                            painter.circle_filled(*pt, 4.0, ds.color);
                            painter.circle_stroke(*pt, 4.0, egui::Stroke::new(1.5, bg));
                        }
                    }

                    // X-axis labels for line/area
                    if let Some(ds) = chart.datasets.first() {
                        let max_labels = 10;
                        let step = if n_points > max_labels { n_points / max_labels } else { 1 };
                        for (i, (label, _)) in ds.points.iter().enumerate() {
                            if i % step == 0 || i == n_points - 1 {
                                let x = chart_rect.left() + chart_w * (i as f32 / (n_points - 1).max(1) as f32);
                                painter.text(
                                    egui::pos2(x, chart_rect.bottom() + 14.0),
                                    egui::Align2::CENTER_CENTER,
                                    Self::truncate_label(label, 10),
                                    egui::FontId::proportional(9.0),
                                    label_color,
                                );
                            }
                        }
                    }
                }

                // Legend (when multiple datasets)
                if num_datasets > 1 {
                    let mut lx = chart_rect.right() - 10.0;
                    for ds in chart.datasets.iter().rev() {
                        let text_w = ds.label.len() as f32 * 6.0 + 16.0;
                        lx -= text_w;
                        let ly = rect.min.y + 6.0;
                        painter.rect_filled(
                            egui::Rect::from_min_size(egui::pos2(lx, ly), egui::vec2(8.0, 8.0)),
                            2.0, ds.color,
                        );
                        painter.text(
                            egui::pos2(lx + 12.0, ly + 4.0),
                            egui::Align2::LEFT_CENTER,
                            &ds.label,
                            egui::FontId::proportional(9.0),
                            label_color,
                        );
                    }
                }
            });
    }

    fn truncate_label(s: &str, max: usize) -> String {
        if s.len() <= max { s.to_string() }
        else {
            let target = max.saturating_sub(2);
            let mut end = target;
            while end > 0 && !s.is_char_boundary(end) { end -= 1; }
            format!("{}..", &s[..end])
        }
    }

    // -----------------------------------------------------------------------
    // Render Pie / Donut chart using painter
    // -----------------------------------------------------------------------

    fn render_pie_chart(ui: &mut egui::Ui, chart: &ParsedChart, height: f32, _id_base: &str, _idx: usize) {
        if chart.datasets.is_empty() { return; }
        let ds = &chart.datasets[0];
        let total: f64 = ds.points.iter().map(|(_, v)| *v).sum();
        if total <= 0.0 { return; }

        let bg = egui::Color32::from_rgb(15, 23, 42);
        let label_color = egui::Color32::from_rgb(203, 213, 225);

        egui::Frame::new()
            .fill(bg)
            .corner_radius(12.0)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                let avail_w = ui.available_width();
                let outer_r = (height / 2.0 - 16.0).min(avail_w / 4.0);
                let inner_r = outer_r * 0.55; // donut hole
                let center_x = avail_w * 0.35;
                let center_y = height / 2.0;

                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(avail_w, height),
                    egui::Sense::hover(),
                );
                let painter = ui.painter_at(rect);
                let center = rect.min + egui::vec2(center_x, center_y);

                let mut angle = -std::f32::consts::FRAC_PI_2;

                for (i, (_label, value)) in ds.points.iter().enumerate() {
                    let frac = (*value / total) as f32;
                    let sweep = frac * std::f32::consts::TAU;
                    let color = chart_color(i);

                    // Draw donut slice as polygon (outer arc + inner arc reversed)
                    let segments = (sweep / 0.04).max(4.0) as usize;
                    let mut points = Vec::with_capacity(segments * 2 + 2);
                    // Outer arc
                    for s in 0..=segments {
                        let a = angle + sweep * (s as f32 / segments as f32);
                        points.push(center + egui::vec2(a.cos() * outer_r, a.sin() * outer_r));
                    }
                    // Inner arc (reversed)
                    for s in (0..=segments).rev() {
                        let a = angle + sweep * (s as f32 / segments as f32);
                        points.push(center + egui::vec2(a.cos() * inner_r, a.sin() * inner_r));
                    }

                    painter.add(egui::Shape::convex_polygon(
                        points,
                        color,
                        egui::Stroke::new(2.0, bg),
                    ));

                    angle += sweep;
                }

                // Center text — total
                painter.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    format!("{:.0}", total),
                    egui::FontId::proportional(18.0),
                    egui::Color32::WHITE,
                );
                painter.text(
                    center + egui::vec2(0.0, 16.0),
                    egui::Align2::CENTER_CENTER,
                    "total",
                    egui::FontId::proportional(9.0),
                    egui::Color32::from_rgb(148, 163, 184),
                );

                // Legend on the right
                let legend_x = center_x + outer_r + 24.0;
                let legend_start_y = (height - ds.points.len() as f32 * 22.0) / 2.0;
                for (i, (label, value)) in ds.points.iter().enumerate() {
                    let frac = (*value / total) * 100.0;
                    let color = chart_color(i);
                    let y = legend_start_y + i as f32 * 22.0;

                    let pos = rect.min + egui::vec2(legend_x, y);
                    painter.circle_filled(pos + egui::vec2(5.0, 5.0), 5.0, color);
                    painter.text(
                        pos + egui::vec2(14.0, 5.0),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(10.0),
                        label_color,
                    );
                    painter.text(
                        pos + egui::vec2(14.0, 17.0),
                        egui::Align2::LEFT_CENTER,
                        format!("{:.0} ({:.0}%)", value, frac),
                        egui::FontId::proportional(9.0),
                        egui::Color32::from_rgb(100, 116, 139),
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

    fn render_markdown_lines(ui: &mut egui::Ui, content: &str) {
        let mut in_code_block = false;
        for line in content.lines() {
            if line.trim_start().starts_with("```") {
                in_code_block = !in_code_block;
                if in_code_block { ui.add_space(2.0); }
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
                let display = line.replace("**", "").replace("__", "").replace('`', "");
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
    // PDF card — visual page preview with navigation
    // -----------------------------------------------------------------------

    /// Cache directory for rendered PDF page images
    fn pdf_cache_dir() -> PathBuf {
        let dir = std::env::temp_dir().join("tigrimos_pdf_pages");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Render a single PDF page to PNG using macOS Quartz (via Python).
    /// Returns the output PNG path.
    fn render_pdf_page_path(pdf_path: &std::path::Path, page: usize) -> PathBuf {
        let hash = format!("{:x}", {
            let s = pdf_path.display().to_string();
            let mut h: u64 = 5381;
            for b in s.bytes() { h = h.wrapping_mul(33).wrapping_add(b as u64); }
            h
        });
        Self::pdf_cache_dir().join(format!("{}_{}.png", hash, page))
    }

    /// Spawn background render of a PDF page using macOS Quartz
    fn spawn_pdf_page_render(
        ctx: egui::Context,
        pdf_path: PathBuf,
        page: usize,
        _rel_path: String,
    ) {
        std::thread::spawn(move || {
            let out_png = Self::render_pdf_page_path(&pdf_path, page);
            if out_png.exists() {
                ctx.request_repaint();
                return;
            }
            // Pass paths via sys.argv to avoid escaping issues with spaces/special chars
            let script = r#"
import sys
pdf_path = sys.argv[1]
page_num = int(sys.argv[2])
out_path = sys.argv[3]

try:
    import Quartz, CoreFoundation
except ImportError:
    # Fallback: try to install pyobjc-framework-Quartz
    import subprocess
    subprocess.check_call([sys.executable, "-m", "pip", "install", "-q", "pyobjc-framework-Quartz"])
    import Quartz, CoreFoundation

pdf_bytes = pdf_path.encode("utf-8")
url = CoreFoundation.CFURLCreateFromFileSystemRepresentation(None, pdf_bytes, len(pdf_bytes), False)
doc = Quartz.CGPDFDocumentCreateWithURL(url)
if doc:
    n = Quartz.CGPDFDocumentGetNumberOfPages(doc)
    pg = page_num + 1
    if pg <= n:
        page_ref = Quartz.CGPDFDocumentGetPage(doc, pg)
        rect = Quartz.CGPDFPageGetBoxRect(page_ref, Quartz.kCGPDFMediaBox)
        w, h = int(rect.size.width), int(rect.size.height)
        scale = min(1600.0 / w, 1600.0 / h, 2.0)
        sw, sh = int(w * scale), int(h * scale)
        cs = Quartz.CGColorSpaceCreateDeviceRGB()
        ctx = Quartz.CGBitmapContextCreate(None, sw, sh, 8, sw * 4, cs, Quartz.kCGImageAlphaPremultipliedLast)
        Quartz.CGContextSetRGBFillColor(ctx, 1, 1, 1, 1)
        Quartz.CGContextFillRect(ctx, Quartz.CGRectMake(0, 0, sw, sh))
        Quartz.CGContextScaleCTM(ctx, scale, scale)
        Quartz.CGContextDrawPDFPage(ctx, page_ref)
        img = Quartz.CGBitmapContextCreateImage(ctx)
        out_bytes = out_path.encode("utf-8")
        out_url = CoreFoundation.CFURLCreateFromFileSystemRepresentation(None, out_bytes, len(out_bytes), False)
        dest = Quartz.CGImageDestinationCreateWithURL(out_url, "public.png", 1, None)
        Quartz.CGImageDestinationAddImage(dest, img, None)
        Quartz.CGImageDestinationFinalize(dest)
    with open(out_path + ".count", "w") as f:
        f.write(str(n))
"#;
            let result = std::process::Command::new("python3")
                .arg("-c")
                .arg(script)
                .arg(pdf_path.to_str().unwrap_or(""))
                .arg(page.to_string())
                .arg(out_png.to_str().unwrap_or(""))
                .output();
            match result {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        eprintln!("[PDF render] python3 failed: {}", stderr);
                        // Write error marker so we don't retry forever
                        let err_path = format!("{}.error", out_png.display());
                        let _ = std::fs::write(&err_path, stderr.as_bytes());
                    }
                }
                Err(e) => {
                    eprintln!("[PDF render] failed to spawn python3: {}", e);
                    let err_path = format!("{}.error", out_png.display());
                    let _ = std::fs::write(&err_path, format!("{}", e));
                }
            }
            ctx.request_repaint();
        });
    }

    fn render_pdf_card(&mut self, ui: &mut egui::Ui, rel_path: &str, border: egui::Color32) {
        let full = Self::full_path(rel_path);
        let filename = Self::filename(rel_path);
        let is_expanded = self.expanded_doc.as_deref() == Some(rel_path);
        let current_page = *self.pdf_current_page.get(rel_path).unwrap_or(&0);

        // Kick off first page render if not started
        if !self.pdf_pages.contains_key(rel_path) && !self.pdf_rendering.get(rel_path).copied().unwrap_or(false) && full.exists() {
            self.pdf_rendering.insert(rel_path.to_string(), true);
            Self::spawn_pdf_page_render(ui.ctx().clone(), full.clone(), 0, rel_path.to_string());
        }

        // Try to load rendered page image into texture cache
        let page_png = Self::render_pdf_page_path(&full, current_page);
        if page_png.exists() {
            let entry = self.pdf_pages.entry(rel_path.to_string()).or_insert_with(|| {
                // Read total page count from marker file
                let count_file = format!("{}.count", page_png.display());
                let total = std::fs::read_to_string(&count_file)
                    .ok()
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(1);
                (total, HashMap::new())
            });
            if !entry.1.contains_key(&current_page) {
                if let Ok(data) = std::fs::read(&page_png) {
                    if let Ok(img) = image::load_from_memory(&data) {
                        let rgba = img.to_rgba8();
                        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                        let color_image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                        let texture = ui.ctx().load_texture(
                            format!("pdf_{}_{}", rel_path, current_page),
                            color_image,
                            egui::TextureOptions::LINEAR,
                        );
                        entry.1.insert(current_page, texture);
                    }
                }
            }
        }

        egui::Frame::new()
            .fill(egui::Color32::WHITE)
            .corner_radius(8.0)
            .stroke(egui::Stroke::new(1.0, border))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                // Header row
                ui.horizontal(|ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_premultiplied(220, 50, 50, 25))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("\u{1F4C4} PDF")
                                    .size(11.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(200, 50, 50)),
                            );
                        });
                    ui.label(
                        egui::RichText::new(filename)
                            .size(13.0)
                            .strong()
                            .color(egui::Color32::from_rgb(31, 35, 40)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Open").clicked() {
                            let _ = open::that(&full);
                        }
                        let expand_label = if is_expanded { "\u{25B2}" } else { "\u{25BC}" };
                        if ui.small_button(expand_label).clicked() {
                            if is_expanded {
                                self.expanded_doc = None;
                            } else {
                                self.expanded_doc = Some(rel_path.to_string());
                            }
                        }
                    });
                });

                // File size + page info
                ui.horizontal(|ui| {
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
                    if let Some((total, _)) = self.pdf_pages.get(rel_path) {
                        ui.label(
                            egui::RichText::new(format!("Page {} / {}", current_page + 1, total))
                                .size(10.0)
                                .color(egui::Color32::from_rgb(100, 110, 120)),
                        );
                    }
                });

                ui.add_space(4.0);

                // Page image preview
                let has_texture = self.pdf_pages.get(rel_path)
                    .and_then(|(_, pages)| pages.get(&current_page))
                    .is_some();

                if has_texture {
                    let (total, pages) = self.pdf_pages.get(rel_path).unwrap();
                    let total = *total;
                    let texture = pages.get(&current_page).unwrap();
                    let tex_size = texture.size_vec2();
                    let max_h = if is_expanded { 800.0 } else { 400.0 };
                    let max_w = ui.available_width() - 4.0;
                    let scale = (max_w / tex_size.x).min(max_h / tex_size.y).min(1.0);
                    let display_size = egui::vec2(tex_size.x * scale, tex_size.y * scale);

                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(245, 245, 248))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(max_h)
                                .id_salt(format!("pdf_page_{}", rel_path))
                                .show(ui, |ui| {
                                    ui.add(egui::Image::new(texture).fit_to_exact_size(display_size));
                                });
                        });

                    // Page navigation
                    if total > 1 {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let prev_enabled = current_page > 0;
                            if ui.add_enabled(prev_enabled, egui::Button::new("\u{25C0} Prev").small()).clicked() {
                                let new_page = current_page.saturating_sub(1);
                                self.pdf_current_page.insert(rel_path.to_string(), new_page);
                                // Trigger render of new page if not cached
                                if !self.pdf_pages.get(rel_path).map(|(_, p)| p.contains_key(&new_page)).unwrap_or(false) {
                                    Self::spawn_pdf_page_render(ui.ctx().clone(), full.clone(), new_page, rel_path.to_string());
                                }
                            }
                            let next_enabled = current_page + 1 < total;
                            if ui.add_enabled(next_enabled, egui::Button::new("Next \u{25B6}").small()).clicked() {
                                let new_page = current_page + 1;
                                self.pdf_current_page.insert(rel_path.to_string(), new_page);
                                if !self.pdf_pages.get(rel_path).map(|(_, p)| p.contains_key(&new_page)).unwrap_or(false) {
                                    Self::spawn_pdf_page_render(ui.ctx().clone(), full.clone(), new_page, rel_path.to_string());
                                }
                            }
                        });
                    }
                } else {
                    // Check if render failed
                    let err_path = format!("{}.error", Self::render_pdf_page_path(&full, current_page).display());
                    if std::path::Path::new(&err_path).exists() {
                        let err_msg = std::fs::read_to_string(&err_path).unwrap_or_default();
                        // Fall back to text preview
                        if !self.pdf_cache.contains_key(rel_path) && full.exists() {
                            let text = match std::panic::catch_unwind(|| pdf_extract::extract_text(&full)) {
                                Ok(Ok(t)) => t,
                                Ok(Err(e)) => format!("[Could not extract PDF text: {}]", e),
                                Err(_) => "[PDF extraction crashed]".to_string(),
                            };
                            self.pdf_cache.insert(rel_path.to_string(), text);
                        }
                        if let Some(text) = self.pdf_cache.get(rel_path) {
                            ui.add_space(4.0);
                            let preview = if is_expanded { text.clone() } else {
                                let limit = text.char_indices().nth(800).map(|(i, _)| i).unwrap_or(text.len());
                                let mut p = text[..limit].to_string();
                                if limit < text.len() { p.push_str("\n\n... (click \u{25BC} to expand)"); }
                                p
                            };
                            egui::Frame::new()
                                .fill(egui::Color32::from_rgb(250, 250, 252))
                                .corner_radius(4.0)
                                .inner_margin(egui::Margin::same(8))
                                .show(ui, |ui| {
                                    let max_h = if is_expanded { 600.0 } else { 200.0 };
                                    egui::ScrollArea::vertical()
                                        .max_height(max_h)
                                        .id_salt(format!("pdf_text_{}", rel_path))
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(&preview)
                                                    .size(11.5)
                                                    .color(egui::Color32::from_rgb(40, 44, 52))
                                            );
                                        });
                                });
                        } else {
                            ui.label(
                                egui::RichText::new(format!("PDF render failed: {}", err_msg.lines().next().unwrap_or("unknown")))
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(200, 60, 60)),
                            );
                        }
                    } else {
                        // Still loading
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                egui::RichText::new("Rendering PDF page...")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(120, 130, 140)),
                            );
                        });
                        ui.add_space(20.0);
                    }
                }
            });
    }

    // -----------------------------------------------------------------------
    // Excel card — inline table preview
    // -----------------------------------------------------------------------

    fn render_excel_card(&mut self, ui: &mut egui::Ui, rel_path: &str, border: egui::Color32) {
        use calamine::{Reader, open_workbook_auto};

        let full = Self::full_path(rel_path);
        let filename = Self::filename(rel_path);
        let is_expanded = self.expanded_doc.as_deref() == Some(rel_path);

        // Parse and cache Excel data
        if !self.excel_cache.contains_key(rel_path) && full.exists() {
            let mut sheets_data: Vec<(String, Vec<Vec<String>>)> = Vec::new();
            if let Ok(mut workbook) = open_workbook_auto(&full) {
                let sheet_names: Vec<String> = workbook.sheet_names().to_vec();
                for name in &sheet_names {
                    if let Ok(range) = workbook.worksheet_range(name) {
                        let mut rows: Vec<Vec<String>> = Vec::new();
                        for row in range.rows() {
                            let cells: Vec<String> = row.iter().map(|c| {
                                match c {
                                    calamine::Data::Empty => String::new(),
                                    calamine::Data::String(s) => s.clone(),
                                    calamine::Data::Float(f) => {
                                        if *f == (*f as i64) as f64 {
                                            format!("{}", *f as i64)
                                        } else {
                                            format!("{:.4}", f).trim_end_matches('0').trim_end_matches('.').to_string()
                                        }
                                    }
                                    calamine::Data::Int(i) => i.to_string(),
                                    calamine::Data::Bool(b) => b.to_string(),
                                    calamine::Data::Error(e) => format!("{:?}", e),
                                    calamine::Data::DateTime(dt) => format!("{}", dt),
                                    calamine::Data::DateTimeIso(s) => s.clone(),
                                    calamine::Data::DurationIso(s) => s.clone(),
                                }
                            }).collect();
                            rows.push(cells);
                            // Limit rows to prevent huge memory use
                            if rows.len() >= 500 { break; }
                        }
                        sheets_data.push((name.clone(), rows));
                    }
                }
            } else {
                sheets_data.push(("Error".to_string(), vec![vec!["Could not open file".to_string()]]));
            }
            self.excel_cache.insert(rel_path.to_string(), sheets_data);
        }

        egui::Frame::new()
            .fill(egui::Color32::WHITE)
            .corner_radius(8.0)
            .stroke(egui::Stroke::new(1.0, border))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_premultiplied(34, 139, 34, 25))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("\u{1F4CA} XLSX")
                                    .size(11.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(34, 139, 34)),
                            );
                        });
                    ui.label(
                        egui::RichText::new(filename)
                            .size(13.0)
                            .strong()
                            .color(egui::Color32::from_rgb(31, 35, 40)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Open").clicked() {
                            let _ = open::that(&full);
                        }
                        let expand_label = if is_expanded { "\u{25B2}" } else { "\u{25BC}" };
                        if ui.small_button(expand_label).clicked() {
                            if is_expanded {
                                self.expanded_doc = None;
                            } else {
                                self.expanded_doc = Some(rel_path.to_string());
                            }
                        }
                    });
                });

                // Show file size and sheet count
                if let Some(sheets) = self.excel_cache.get(rel_path) {
                    let sheet_count = sheets.len();
                    let total_rows: usize = sheets.iter().map(|(_, rows)| rows.len()).sum();
                    let mut info = format!("{} sheet{}", sheet_count, if sheet_count != 1 { "s" } else { "" });
                    if total_rows > 0 {
                        info.push_str(&format!(", {} rows", total_rows));
                    }
                    ui.label(
                        egui::RichText::new(info)
                            .size(10.0)
                            .color(egui::Color32::from_rgb(150, 160, 170)),
                    );
                }

                // Render table preview
                if let Some(sheets) = self.excel_cache.get(rel_path).cloned() {
                    ui.add_space(6.0);
                    let max_h = if is_expanded { 600.0 } else { 220.0 };
                    let max_rows = if is_expanded { 500 } else { 20 };

                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(250, 250, 252))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| {
                            egui::ScrollArea::both()
                                .max_height(max_h)
                                .id_salt(format!("excel_scroll_{}", rel_path))
                                .show(ui, |ui| {
                                    for (sheet_name, rows) in &sheets {
                                        if sheets.len() > 1 {
                                            ui.label(
                                                egui::RichText::new(format!("\u{1F4CB} {}", sheet_name))
                                                    .size(11.0)
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(34, 139, 34)),
                                            );
                                            ui.add_space(4.0);
                                        }

                                        if rows.is_empty() {
                                            ui.label(
                                                egui::RichText::new("(empty sheet)")
                                                    .size(11.0)
                                                    .color(egui::Color32::from_rgb(150, 160, 170)),
                                            );
                                        } else {
                                            let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                                            egui_extras::TableBuilder::new(ui)
                                                .striped(true)
                                                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                                .columns(egui_extras::Column::auto().at_least(40.0).clip(true), col_count)
                                                .header(18.0, |mut header| {
                                                    if let Some(first_row) = rows.first() {
                                                        for cell in first_row {
                                                            header.col(|ui| {
                                                                ui.label(
                                                                    egui::RichText::new(cell)
                                                                        .size(11.0)
                                                                        .strong()
                                                                        .color(egui::Color32::from_rgb(31, 35, 40)),
                                                                );
                                                            });
                                                        }
                                                    }
                                                })
                                                .body(|body| {
                                                    let data_rows: Vec<&Vec<String>> = rows.iter().skip(1).take(max_rows).collect();
                                                    body.rows(16.0, data_rows.len(), |mut row| {
                                                        let idx = row.index();
                                                        if let Some(cells) = data_rows.get(idx) {
                                                            for c in 0..col_count {
                                                                row.col(|ui| {
                                                                    let val = cells.get(c).map(|s| s.as_str()).unwrap_or("");
                                                                    ui.label(
                                                                        egui::RichText::new(val)
                                                                            .size(10.5)
                                                                            .color(egui::Color32::from_rgb(50, 60, 70)),
                                                                    );
                                                                });
                                                            }
                                                        }
                                                    });
                                                });

                                            if rows.len() > max_rows + 1 {
                                                ui.add_space(4.0);
                                                ui.label(
                                                    egui::RichText::new(format!("... {} more rows (click \u{25BC} to expand)", rows.len() - max_rows - 1))
                                                        .size(10.0)
                                                        .color(egui::Color32::from_rgb(150, 160, 170)),
                                                );
                                            }
                                        }
                                        ui.add_space(8.0);
                                    }
                                });
                        });
                }
            });
    }

    // -----------------------------------------------------------------------
    // Document card (Word)
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
                    ui.label(egui::RichText::new(icon).size(18.0));
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
