//! User-customizable UI theme (colors + font sizes), persisted as YAML.
//!
//! The theme is stored in `data/theme.yaml` (see [`Theme::path`]). It is loaded
//! once at startup in [`crate::ui::app::TigrimOSApp::new`] and can be edited live
//! from Settings → Theme, which re-applies it to the running egui context and
//! writes it back to disk.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};

/// Chat message base font size, published globally so the free-function
/// markdown renderers in `chat.rs` can read it without threading it through
/// every call. 0 bits means "unset" → fall back to the default.
static CHAT_FONT_SIZE: AtomicU32 = AtomicU32::new(0);

// Matches the web/mobile remote chat bubble text (`font-size: 14.5px`).
const DEFAULT_CHAT_FONT_SIZE: f32 = 14.5;

/// Current chat message base font size (in points). Read by the chat renderer.
pub fn chat_font_size() -> f32 {
    let bits = CHAT_FONT_SIZE.load(Ordering::Relaxed);
    if bits == 0 {
        DEFAULT_CHAT_FONT_SIZE
    } else {
        f32::from_bits(bits)
    }
}

// Chat bubble colors, published globally so the chat renderers (free functions
// / `&self` methods without theme access) can read the active palette.
// A stored value of 0 means "unset" → use the default; real opaque colors are
// never 0 because their alpha byte is non-zero.
static CHAT_USER_BUBBLE: AtomicU32 = AtomicU32::new(0);
static CHAT_AI_BUBBLE: AtomicU32 = AtomicU32::new(0);
static CHAT_AI_TEXT: AtomicU32 = AtomicU32::new(0);
static CHAT_USER_TEXT: AtomicU32 = AtomicU32::new(0);

fn pack_color(c: Color32) -> u32 {
    u32::from_be_bytes(c.to_array())
}

fn load_color(slot: &AtomicU32, fallback: Color32) -> Color32 {
    let v = slot.load(Ordering::Relaxed);
    if v == 0 {
        fallback
    } else {
        let [r, g, b, a] = v.to_be_bytes();
        Color32::from_rgba_premultiplied(r, g, b, a)
    }
}

/// Fill color for the user's chat bubble (theme accent).
pub fn chat_user_bubble() -> Color32 {
    load_color(&CHAT_USER_BUBBLE, Color32::from_rgb(18, 154, 145))
}

/// Fill color for the AI's chat bubble (theme card color).
pub fn chat_ai_bubble() -> Color32 {
    load_color(&CHAT_AI_BUBBLE, Color32::WHITE)
}

/// Text color for the AI's chat bubble (theme primary text).
pub fn chat_ai_text() -> Color32 {
    load_color(&CHAT_AI_TEXT, Color32::from_rgb(52, 48, 42))
}

/// Readable text color for the user's chat bubble (auto-contrasted to the
/// bubble fill, so it works on light or dark accents).
pub fn chat_user_text() -> Color32 {
    load_color(&CHAT_USER_TEXT, Color32::WHITE)
}

// Core palette, published globally so the main window chrome (sidebar, central
// panel, headers) can color itself from the active theme without a Theme handle.
static T_SURFACE: AtomicU32 = AtomicU32::new(0);
static T_CANVAS: AtomicU32 = AtomicU32::new(0);
static T_CARD: AtomicU32 = AtomicU32::new(0);
static T_HOVER: AtomicU32 = AtomicU32::new(0);
static T_BORDER: AtomicU32 = AtomicU32::new(0);
static T_TEXT_PRIMARY: AtomicU32 = AtomicU32::new(0);
static T_TEXT_SECONDARY: AtomicU32 = AtomicU32::new(0);
static T_ACCENT: AtomicU32 = AtomicU32::new(0);

/// Surface color (main window / panel background).
pub fn surface_color() -> Color32 { load_color(&T_SURFACE, Color32::from_rgb(251, 247, 241)) }
/// Canvas color (inputs / recessed areas).
pub fn canvas_color() -> Color32 { load_color(&T_CANVAS, Color32::from_rgb(244, 238, 229)) }
/// Card color (cards / input bar).
pub fn card_color() -> Color32 { load_color(&T_CARD, Color32::WHITE) }
/// Hover / rail color (sidebar background).
pub fn hover_color() -> Color32 { load_color(&T_HOVER, Color32::from_rgb(239, 231, 218)) }
/// Border / hairline color.
pub fn border_color() -> Color32 { load_color(&T_BORDER, Color32::from_rgb(230, 220, 204)) }
/// Primary text color.
pub fn text_primary_color() -> Color32 { load_color(&T_TEXT_PRIMARY, Color32::from_rgb(52, 48, 42)) }
/// Secondary / muted text color.
pub fn text_secondary_color() -> Color32 { load_color(&T_TEXT_SECONDARY, Color32::from_rgb(124, 115, 104)) }
/// Accent color.
pub fn accent_color() -> Color32 { load_color(&T_ACCENT, Color32::from_rgb(18, 154, 145)) }

// How AI output files (graphs/pictures) are presented: false = side output
// panel (default), true = embedded inline in chat with click-to-zoom.
static EMBED_FILES: AtomicBool = AtomicBool::new(false);

/// True when output files should be embedded inline in chat instead of the panel.
pub fn embed_files_in_chat() -> bool {
    EMBED_FILES.load(Ordering::Relaxed)
}

/// Full theme definition. Serialized to / from `theme.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub colors: ThemeColors,
    pub fonts: ThemeFonts,
    /// Name of the bundled UI font to use (see [`bundled_font_names`]).
    /// Ignored when `custom_font_path` is set.
    #[serde(default = "default_font_family")]
    pub font_family: String,
    /// Absolute path to a custom UI font (`.ttf` / `.otf` / `.ttc`). When set,
    /// it overrides `font_family`. Empty means use `font_family`.
    #[serde(default)]
    pub custom_font_path: String,
    /// How AI output files (graphs/pictures) are shown:
    /// `"panel"` (side output panel, default) or `"chat"` (embedded inline).
    #[serde(default = "default_file_display")]
    pub file_display: String,
}

fn default_font_family() -> String {
    DEFAULT_FONT_FAMILY.to_string()
}

fn default_file_display() -> String {
    "chat".to_string()
}

/// Color palette. Each value is a `#RRGGBB` hex string so the YAML stays
/// human-readable and hand-editable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeColors {
    /// Panel & window background.
    pub surface: String,
    /// Canvas / extreme background (text edit fields, etc.).
    pub canvas: String,
    /// Card / non-interactive widget fill.
    pub card: String,
    /// Hover background.
    pub hover: String,
    /// Hairline borders & separators.
    pub border: String,
    /// Primary text.
    pub text_primary: String,
    /// Secondary / muted text.
    pub text_secondary: String,
    /// Accent (active widgets, selection, links).
    pub accent: String,
    /// Deeper accent used for active/pressed strokes.
    pub accent_hover: String,
    /// User chat bubble fill. Empty = use `accent`. Supports `#RRGGBBAA`.
    #[serde(default)]
    pub user_bubble: String,
    /// AI chat bubble fill. Empty = use `card`. Supports `#RRGGBBAA`.
    #[serde(default)]
    pub ai_bubble: String,
}

/// Font sizes (in points) for each egui text style.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeFonts {
    pub small: f32,
    pub body: f32,
    pub button: f32,
    pub heading: f32,
    pub monospace: f32,
    /// Base font size for chat message text (headings/code scale relative to it).
    pub chat: f32,
}

impl Default for ThemeColors {
    fn default() -> Self {
        // Warm neutral surfaces with a teal accent — matches the original
        // hardcoded palette so existing installs look identical.
        Self {
            surface: "#FBF7F1".to_string(),
            canvas: "#F4EEE5".to_string(),
            card: "#FFFFFF".to_string(),
            hover: "#EFE7DA".to_string(),
            border: "#E6DCCC".to_string(),
            text_primary: "#34302A".to_string(),
            text_secondary: "#7C7368".to_string(),
            accent: "#129A91".to_string(),
            accent_hover: "#0C817A".to_string(),
            user_bubble: String::new(),
            ai_bubble: String::new(),
        }
    }
}

impl Default for ThemeFonts {
    fn default() -> Self {
        Self {
            small: 12.9,
            body: 15.2,
            button: 13.7,
            heading: 21.2,
            monospace: 14.7,
            chat: DEFAULT_CHAT_FONT_SIZE, // 15.0
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            colors: ThemeColors::default(),
            fonts: ThemeFonts::default(),
            font_family: default_font_family(),
            custom_font_path: String::new(),
            file_display: default_file_display(),
        }
    }
}

// ---------------------------------------------------------------------------
// Bundled fonts (embedded in the binary so they work from the installed .app)
// ---------------------------------------------------------------------------

macro_rules! asset_font {
    ($p:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $p))
    };
}

// Proportional UI fonts.
// The web/mobile remote UI renders message body text at font-weight 500
// (Medium) — see `.msg .bubble` in static/index.html. Use the Medium weight as
// the primary face so the native UI matches the web's apparent weight.
static FONT_JAKARTA: &[u8] = asset_font!("PlusJakartaSans-Medium.ttf");
static FONT_JAKARTA_BOLD: &[u8] = asset_font!("PlusJakartaSans-SemiBold.ttf");
static FONT_INTER: &[u8] = asset_font!("fonts/Inter.ttf");
static FONT_GEIST: &[u8] = asset_font!("fonts/Geist.ttf");
static FONT_ROBOTO: &[u8] = asset_font!("fonts/Roboto.ttf");
static FONT_IBM_PLEX: &[u8] = asset_font!("fonts/IBMPlexSans.ttf");
// Monospace (code) font + emoji fallback.
static FONT_JETBRAINS_MONO: &[u8] = asset_font!("fonts/JetBrainsMono.ttf");
static FONT_NOTO_EMOJI: &[u8] = asset_font!("NotoEmoji-Regular.ttf");
// Bundled Thai coverage (Noto Sans Thai, OFL). Guarantees Thai glyphs on every
// platform instead of relying on OS-installed fonts (which are absent on most
// Linux/Windows installs, rendering Thai as tofu boxes).
static FONT_NOTO_THAI: &[u8] = asset_font!("fonts/NotoSansThai-Regular.ttf");

/// The default UI font family name. Matches the web/mobile remote UI, which
/// renders with "Plus Jakarta Sans" (see `static/index.html`).
pub const DEFAULT_FONT_FAMILY: &str = "Plus Jakarta Sans";

/// Names of the bundled, selectable proportional UI fonts (modern web fonts).
/// "Inter" is the font used by Vite / VitePress; "Geist" is Vercel's.
pub fn bundled_font_names() -> &'static [&'static str] {
    &[
        "Plus Jakarta Sans",
        "Inter",
        "Geist",
        "Roboto",
        "IBM Plex Sans",
    ]
}

/// Bytes for a named bundled font, or `None` if the name is unknown.
fn bundled_font_bytes(name: &str) -> Option<&'static [u8]> {
    match name {
        "Plus Jakarta Sans" => Some(FONT_JAKARTA),
        "Inter" => Some(FONT_INTER),
        "Geist" => Some(FONT_GEIST),
        "Roboto" => Some(FONT_ROBOTO),
        "IBM Plex Sans" => Some(FONT_IBM_PLEX),
        _ => None,
    }
}

/// Parse a `#RRGGBB` or `#RRGGBBAA` (with or without `#`) hex string into a
/// [`Color32`]. Falls back to `fallback` on any parse error so a typo in YAML
/// never panics.
pub fn hex_to_color(hex: &str, fallback: Color32) -> Color32 {
    let h = hex.trim().trim_start_matches('#');
    let p = |a: usize, b: usize| u8::from_str_radix(&h[a..b], 16);
    if h.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (p(0, 2), p(2, 4), p(4, 6)) {
            return Color32::from_rgb(r, g, b);
        }
    } else if h.len() == 8 {
        if let (Ok(r), Ok(g), Ok(b), Ok(a)) = (p(0, 2), p(2, 4), p(4, 6), p(6, 8)) {
            return Color32::from_rgba_unmultiplied(r, g, b, a);
        }
    }
    fallback
}

/// Format a [`Color32`] as `#RRGGBB`, or `#RRGGBBAA` when it has transparency.
pub fn color_to_hex(c: Color32) -> String {
    if c.a() == 255 {
        format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
    } else {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a)
    }
}

/// Pick a readable foreground (near-black or white) for a given background,
/// based on perceived luminance.
pub fn readable_on(bg: Color32) -> Color32 {
    let l = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if l > 150.0 {
        Color32::from_rgb(32, 33, 35)
    } else {
        Color32::WHITE
    }
}

/// Names of the built-in preset themes the user can pick from.
pub fn preset_names() -> &'static [&'static str] {
    &["Default", "Dark", "Minimal", "Transparent", "Colorful"]
}

#[allow(clippy::too_many_arguments)]
fn mk_colors(
    surface: &str, canvas: &str, card: &str, hover: &str, border: &str,
    text_primary: &str, text_secondary: &str, accent: &str, accent_hover: &str,
    user_bubble: &str, ai_bubble: &str,
) -> ThemeColors {
    ThemeColors {
        surface: surface.into(), canvas: canvas.into(), card: card.into(),
        hover: hover.into(), border: border.into(),
        text_primary: text_primary.into(), text_secondary: text_secondary.into(),
        accent: accent.into(), accent_hover: accent_hover.into(),
        user_bubble: user_bubble.into(), ai_bubble: ai_bubble.into(),
    }
}

/// Color palette for a named preset, or `None` if the name is unknown.
pub fn preset_colors(name: &str) -> Option<ThemeColors> {
    Some(match name {
        // Warm neutral surfaces with a teal accent (the original look).
        "Default" => mk_colors(
            "#FBF7F1", "#F4EEE5", "#FFFFFF", "#EFE7DA", "#E6DCCC",
            "#34302A", "#7C7368", "#129A91", "#0C817A", "", "",
        ),
        // Dark mode.
        "Dark" => mk_colors(
            "#1B1B1F", "#141417", "#26262C", "#30303A", "#3A3A44",
            "#ECE9E4", "#9A938A", "#4FD1C5", "#38B2AC", "", "#26262C",
        ),
        // Clean, ChatGPT-style light: white surface, soft gray bubbles, green accent.
        "Minimal" => mk_colors(
            "#FFFFFF", "#F7F7F8", "#F7F7F8", "#ECECEC", "#E5E5E5",
            "#202123", "#6E7681", "#10A37F", "#0E8C6D", "#ECECEC", "#F7F7F8",
        ),
        // Translucent surfaces (needs window transparency, enabled in main.rs).
        "Transparent" => mk_colors(
            "#F4EEE5C0", "#EAE3D6B0", "#FFFFFFCC", "#EFE7DAC0", "#E6DCCCB0",
            "#34302A", "#7C7368", "#129A91", "#0C817A", "#129A91E0", "#FFFFFFCC",
        ),
        // Vibrant violet palette.
        "Colorful" => mk_colors(
            "#F5F3FF", "#EDE9FE", "#FFFFFF", "#E0E7FF", "#DDD6FE",
            "#2E1065", "#6D28D9", "#8B5CF6", "#7C3AED", "#8B5CF6", "#FFFFFF",
        ),
        _ => return None,
    })
}

impl Theme {
    /// Replace this theme's colors with a named preset (fonts are kept).
    /// Returns `false` if the preset name is unknown.
    pub fn apply_preset(&mut self, name: &str) -> bool {
        match preset_colors(name) {
            Some(c) => { self.colors = c; true }
            None => false,
        }
    }

    /// Path to the theme YAML file inside the app data directory.
    pub fn path() -> std::path::PathBuf {
        crate::server::data::data_dir().join("theme.yaml")
    }

    /// Load the theme from `theme.yaml`, falling back to defaults if the file
    /// is missing or cannot be parsed.
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(content) => serde_yaml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist the theme to `theme.yaml`. Returns an error string on failure.
    pub fn save(&self) -> Result<(), String> {
        let yaml = serde_yaml::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(Self::path(), yaml).map_err(|e| e.to_string())
    }

    // --- color accessors (hex string -> Color32, with safe fallbacks) ---
    fn surface(&self) -> Color32 { hex_to_color(&self.colors.surface, Color32::from_rgb(251, 247, 241)) }
    fn canvas(&self) -> Color32 { hex_to_color(&self.colors.canvas, Color32::from_rgb(244, 238, 229)) }
    fn card(&self) -> Color32 { hex_to_color(&self.colors.card, Color32::WHITE) }
    fn hover(&self) -> Color32 { hex_to_color(&self.colors.hover, Color32::from_rgb(239, 231, 218)) }
    fn border(&self) -> Color32 { hex_to_color(&self.colors.border, Color32::from_rgb(230, 220, 204)) }
    fn text_primary(&self) -> Color32 { hex_to_color(&self.colors.text_primary, Color32::from_rgb(52, 48, 42)) }
    fn text_secondary(&self) -> Color32 { hex_to_color(&self.colors.text_secondary, Color32::from_rgb(124, 115, 104)) }
    fn accent(&self) -> Color32 { hex_to_color(&self.colors.accent, Color32::from_rgb(18, 154, 145)) }
    fn accent_hover(&self) -> Color32 { hex_to_color(&self.colors.accent_hover, Color32::from_rgb(12, 129, 122)) }
    /// User bubble fill: explicit `user_bubble`, else the accent color.
    fn user_bubble(&self) -> Color32 {
        if self.colors.user_bubble.trim().is_empty() {
            self.accent()
        } else {
            hex_to_color(&self.colors.user_bubble, self.accent())
        }
    }
    /// AI bubble fill: explicit `ai_bubble`, else the card color.
    fn ai_bubble(&self) -> Color32 {
        if self.colors.ai_bubble.trim().is_empty() {
            self.card()
        } else {
            hex_to_color(&self.colors.ai_bubble, self.card())
        }
    }

    /// Build the egui font definitions for this theme and apply them to `ctx`.
    ///
    /// Proportional (UI) family priority:
    ///   1. custom font file (if `custom_font_path` is set & readable)
    ///   2. the selected bundled font (`font_family`, default Plus Jakarta Sans)
    ///   3. Plus Jakarta Sans (Latin coverage) + bold
    ///   4. Thai / emoji / symbol fallbacks
    ///
    /// Monospace family starts with the bundled JetBrains Mono.
    ///
    /// Safe to call at runtime (e.g. when the user picks a new font in Settings).
    pub fn apply_fonts(&self, ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        let prop = egui::FontFamily::Proportional;
        let mono = egui::FontFamily::Monospace;

        // Helper to register a font and place it at a position in a family.
        // `front` = insert at the head (highest priority); else append.
        let add = |fonts: &mut egui::FontDefinitions,
                   key: &str,
                   data: egui::FontData,
                   families: &[(egui::FontFamily, bool)]| {
            fonts.font_data.insert(key.to_owned(), data.into());
            for (fam, front) in families {
                if let Some(v) = fonts.families.get_mut(fam) {
                    if *front {
                        v.insert(0, key.to_owned());
                    } else {
                        v.push(key.to_owned());
                    }
                }
            }
        };

        // 1. Primary UI font: custom file > selected bundled font > Jakarta.
        let custom = self.custom_font_path.trim();
        let mut primary_registered = false;
        if !custom.is_empty() {
            if let Ok(data) = std::fs::read(custom) {
                add(&mut fonts, "PrimaryFont", egui::FontData::from_owned(data),
                    &[(prop.clone(), true)]);
                primary_registered = true;
            }
        }
        if !primary_registered {
            let bytes = bundled_font_bytes(&self.font_family).unwrap_or(FONT_JAKARTA);
            add(&mut fonts, "PrimaryFont", egui::FontData::from_static(bytes),
                &[(prop.clone(), true)]);
        }

        // 2. Plus Jakarta Sans as a Latin fallback + bold coverage (appended).
        add(&mut fonts, "JakartaSans", egui::FontData::from_static(FONT_JAKARTA),
            &[(prop.clone(), false)]);
        add(&mut fonts, "JakartaSansBold", egui::FontData::from_static(FONT_JAKARTA_BOLD),
            &[(prop.clone(), false)]);

        // 3. Monospace primary: bundled JetBrains Mono.
        add(&mut fonts, "JetBrainsMono", egui::FontData::from_static(FONT_JETBRAINS_MONO),
            &[(mono.clone(), true)]);

        // 4a. Thai fallback — appended to both families.
        // Prefer an OS-installed Thai face (nicer native look on macOS), then
        // always append the bundled Noto Sans Thai so Thai renders on every
        // platform even when no system Thai font exists (Linux/Windows).
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
                add(&mut fonts, "ThaiFallback", egui::FontData::from_owned(data),
                    &[(prop.clone(), false), (mono.clone(), false)]);
                break;
            }
        }
        // Bundled Thai coverage (always present, lowest Thai priority).
        add(&mut fonts, "NotoSansThai", egui::FontData::from_static(FONT_NOTO_THAI),
            &[(prop.clone(), false), (mono.clone(), false)]);

        // 4b. Emoji fallback (bundled monochrome Noto Emoji).
        add(&mut fonts, "NotoEmoji", egui::FontData::from_static(FONT_NOTO_EMOJI),
            &[(prop.clone(), false), (mono.clone(), false)]);

        // 4c. Symbol fallback for sidebar icons.
        let symbol_paths = [
            "/System/Library/Fonts/Apple Symbols.ttf",
            "/System/Library/Fonts/Supplemental/Apple Symbols.ttf",
            "/System/Library/Fonts/SFCompact.ttf",
            "/System/Library/Fonts/Menlo.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ];
        for path in &symbol_paths {
            if let Ok(data) = std::fs::read(path) {
                add(&mut fonts, "SymbolFallback", egui::FontData::from_owned(data),
                    &[(prop.clone(), false), (mono.clone(), false)]);
                break;
            }
        }

        ctx.set_fonts(fonts);
    }

    /// Build egui visuals + style from this theme and apply them to `ctx`.
    /// Font *families* are configured separately via [`Theme::apply_fonts`]; this
    /// controls colors, spacing and font *sizes*.
    pub fn apply(&self, ctx: &egui::Context) {
        // Publish the chat base font size for the chat markdown renderers.
        let chat = if self.fonts.chat > 0.0 { self.fonts.chat } else { DEFAULT_CHAT_FONT_SIZE };
        CHAT_FONT_SIZE.store(chat.to_bits(), Ordering::Relaxed);

        // Publish chat bubble colors so the chat renderers track the theme.
        let user_bubble = self.user_bubble();
        CHAT_USER_BUBBLE.store(pack_color(user_bubble), Ordering::Relaxed);
        CHAT_AI_BUBBLE.store(pack_color(self.ai_bubble()), Ordering::Relaxed);
        CHAT_AI_TEXT.store(pack_color(self.text_primary()), Ordering::Relaxed);
        // Auto-contrast the user-bubble text so it reads on any accent.
        CHAT_USER_TEXT.store(pack_color(readable_on(user_bubble)), Ordering::Relaxed);

        // Publish the core palette for the main window chrome.
        T_SURFACE.store(pack_color(self.surface()), Ordering::Relaxed);
        T_CANVAS.store(pack_color(self.canvas()), Ordering::Relaxed);
        T_CARD.store(pack_color(self.card()), Ordering::Relaxed);
        T_HOVER.store(pack_color(self.hover()), Ordering::Relaxed);
        T_BORDER.store(pack_color(self.border()), Ordering::Relaxed);
        T_TEXT_PRIMARY.store(pack_color(self.text_primary()), Ordering::Relaxed);
        T_TEXT_SECONDARY.store(pack_color(self.text_secondary()), Ordering::Relaxed);
        T_ACCENT.store(pack_color(self.accent()), Ordering::Relaxed);
        EMBED_FILES.store(self.file_display.trim() == "chat", Ordering::Relaxed);

        let bg_card = self.card();
        let bg_light = self.canvas();
        let bg_hover = self.hover();
        let border = self.border();
        let text_primary = self.text_primary();
        let text_secondary = self.text_secondary();
        let accent = self.accent();
        let accent_hover = self.accent_hover();
        let bg_surface = self.surface();

        // Use a dark base when the surface is dark, so egui's built-in defaults
        // (scrollbars, code backgrounds, etc.) match the theme.
        let dark = readable_on(bg_surface) == Color32::WHITE;
        let mut visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };

        visuals.panel_fill = bg_surface;
        visuals.window_fill = bg_surface;
        visuals.extreme_bg_color = bg_light;
        visuals.faint_bg_color = bg_light;

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

        visuals.selection.bg_fill =
            egui::Color32::from_rgba_premultiplied(accent.r(), accent.g(), accent.b(), 50);
        visuals.selection.stroke = egui::Stroke::new(1.0, accent);
        visuals.hyperlink_color = accent;

        visuals.window_stroke = egui::Stroke::new(1.0, border);
        visuals.window_shadow = egui::epaint::Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: egui::Color32::from_rgba_premultiplied(0, 0, 0, 20),
        };
        visuals.popup_shadow = visuals.window_shadow;
        visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(0.5, border);

        ctx.set_visuals(visuals);

        // Spacing / global style
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.window_margin = egui::Margin::same(12);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.scroll = egui::style::ScrollStyle {
            bar_width: 6.0,
            ..style.spacing.scroll
        };

        use egui::{FontFamily, FontId, TextStyle};
        style.text_styles = [
            (TextStyle::Small, FontId::new(self.fonts.small, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(self.fonts.body, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(self.fonts.button, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(self.fonts.heading, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(self.fonts.monospace, FontFamily::Monospace)),
        ]
        .into();

        ctx.set_style(style);
    }
}
