//! CLI model discovery.
//!
//! Probes the agent CLIs installed on this machine (Claude Code, Codex,
//! Antigravity/`agy`, OpenCode, Grok, Copilot, Gemini) for the models and
//! reasoning-effort levels they actually support, so the UI can offer real
//! dropdowns instead of a free-text box that goes stale the moment a vendor
//! ships a new model.
//!
//! Honesty rule: every entry carries a `source` saying where it came from —
//! `cli` (the CLI enumerated it), `cache` (read from a cache the CLI itself
//! maintains), or `builtin` (a curated fallback we ship). The UI surfaces that
//! distinction so a guess is never presented as a discovered fact. When a CLI
//! cannot enumerate its models we return an empty list and an `error`, rather
//! than inventing plausible-looking model IDs.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::debug;

/// Per-CLI probe budget. These are local processes; if one hangs past this we
/// report it unavailable rather than blocking the settings screen.
const PROBE_TIMEOUT_SECS: u64 = 12;

/// How long a successful discovery is reused before we re-probe.
const CACHE_TTL: Duration = Duration::from_secs(600);

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One selectable model within a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliModel {
    /// The exact string to pass to the CLI's `--model` flag.
    pub id: String,
    /// Human-facing label. Falls back to `id` when the CLI gives no nicer name.
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Effort this model uses when none is specified.
    #[serde(rename = "defaultEffort", skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    /// Effort levels valid for *this* model. Empty means "fall back to the
    /// provider-level list".
    pub efforts: Vec<String>,
}

impl CliModel {
    fn bare(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            label: id.clone(),
            id,
            description: None,
            default_effort: None,
            efforts: Vec::new(),
        }
    }
}

/// A CLI provider and everything we could learn about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliProvider {
    /// Stable key used by settings payloads (`claude`, `codex`, ...).
    pub id: String,
    /// Display name for the dropdown.
    pub name: String,
    /// Executable we probe. Resolved through PATH.
    pub binary: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `cli` | `cache` | `builtin` | `none` — see module docs.
    pub source: String,
    pub models: Vec<CliModel>,
    /// Provider-wide effort levels, applied when a model declares none.
    pub efforts: Vec<String>,
    /// Why discovery came up short, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CliProvider {
    fn missing(id: &str, name: &str, binary: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            binary: binary.into(),
            available: false,
            version: None,
            source: "none".into(),
            models: Vec::new(),
            efforts: Vec::new(),
            error: Some(format!("`{binary}` was not found on PATH")),
        }
    }
}

// ---------------------------------------------------------------------------
// Subprocess plumbing
// ---------------------------------------------------------------------------

/// Run a short-lived probe and return stdout (plus stderr, which several of
/// these CLIs use for their model listings).
///
/// Deliberately self-contained rather than reusing `toolbox::run_guarded`: that
/// helper ties into per-session cancellation, and a settings probe has no
/// session to be cancelled with.
async fn probe(binary: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(binary);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Without this each probe flashes a console window on the desktop app.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let fut = cmd.output();
    let out = match tokio::time::timeout(Duration::from_secs(PROBE_TIMEOUT_SECS), fut).await {
        Err(_) => return Err(format!("`{binary} {}` timed out", args.join(" "))),
        Ok(Err(e)) => return Err(format!("could not run `{binary}`: {e}")),
        Ok(Ok(o)) => o,
    };

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push('\n');
        text.push_str(&err);
    }
    Ok(text)
}

/// True when the binary can be launched at all.
async fn binary_present(binary: &str) -> bool {
    probe(binary, &["--version"]).await.is_ok()
}

/// First line that looks like a version string.
fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Pull `low, medium, high, xhigh, max` out of a help line such as
/// `--effort <level>   Effort level for the current session (low, medium, high, xhigh, max)`.
///
/// Parsing the CLI's own help keeps the list correct when the vendor adds a
/// tier, instead of freezing whatever was true the day this shipped.
fn efforts_from_help(help: &str, flag: &str) -> Vec<String> {
    let mut lines = help.lines();
    while let Some(line) = lines.next() {
        if !line.contains(flag) {
            continue;
        }
        // The list may wrap onto the following line.
        let window = format!("{line} {}", lines.clone().next().unwrap_or(""));
        let Some(open) = window.find('(') else {
            continue;
        };
        let Some(close) = window[open..].find(')') else {
            continue;
        };
        let inner = &window[open + 1..open + close];
        let parsed: Vec<String> = inner
            .split([',', '|'])
            .map(|s| s.trim().to_lowercase())
            .filter(|s| {
                !s.is_empty()
                    && s.len() < 12
                    && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            .collect();
        if parsed.len() >= 2 {
            return parsed;
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Per-provider probes
// ---------------------------------------------------------------------------

/// Claude Code. Has no `models` subcommand, but its `--help` documents both the
/// effort tiers and the alias scheme, so we read the tiers from help and ship a
/// curated alias list marked `builtin`.
async fn probe_claude() -> CliProvider {
    let binary = "claude";
    if !binary_present(binary).await {
        return CliProvider::missing("claude", "Claude Code (Local)", binary);
    }
    let version = probe(binary, &["--version"])
        .await
        .ok()
        .and_then(|s| first_line(&s));
    let help = probe(binary, &["--help"]).await.unwrap_or_default();

    let mut efforts = efforts_from_help(&help, "--effort");
    if efforts.is_empty() {
        efforts = ["low", "medium", "high", "xhigh", "max"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    }

    // Aliases always resolve to the current model, so they age well.
    let models = vec![
        CliModel {
            id: "fable".into(),
            label: "Fable (alias — latest)".into(),
            description: Some("Alias that always resolves to the newest Fable model".into()),
            default_effort: None,
            efforts: efforts.clone(),
        },
        CliModel {
            id: "opus".into(),
            label: "Opus (alias — latest)".into(),
            description: Some("Alias that always resolves to the newest Opus model".into()),
            default_effort: None,
            efforts: efforts.clone(),
        },
        CliModel {
            id: "sonnet".into(),
            label: "Sonnet (alias — latest)".into(),
            description: Some("Alias that always resolves to the newest Sonnet model".into()),
            default_effort: None,
            efforts: efforts.clone(),
        },
        CliModel {
            id: "haiku".into(),
            label: "Haiku (alias — latest)".into(),
            description: Some("Alias that always resolves to the newest Haiku model".into()),
            default_effort: None,
            efforts: efforts.clone(),
        },
    ];

    CliProvider {
        id: "claude".into(),
        name: "Claude Code (Local)".into(),
        binary: binary.into(),
        available: true,
        version,
        // Effort tiers come from the CLI; the model list is curated.
        source: "builtin".into(),
        models,
        efforts,
        error: None,
    }
}

/// Codex keeps a `models_cache.json` next to its config listing every model it
/// offers together with that model's supported reasoning levels — richer than
/// anything the CLI prints, so we read it directly.
async fn probe_codex() -> CliProvider {
    let binary = "codex";
    if !binary_present(binary).await {
        return CliProvider::missing("codex", "Codex (Local)", binary);
    }
    let version = probe(binary, &["--version"])
        .await
        .ok()
        .and_then(|s| first_line(&s));

    let mut provider = CliProvider {
        id: "codex".into(),
        name: "Codex (Local)".into(),
        binary: binary.into(),
        available: true,
        version,
        source: "builtin".into(),
        models: Vec::new(),
        efforts: vec![
            "low".into(),
            "medium".into(),
            "high".into(),
            "xhigh".into(),
            "max".into(),
        ],
        error: None,
    };

    let Some(home) = dirs::home_dir() else {
        provider.error =
            Some("could not resolve the home directory to find Codex's model cache".into());
        return provider;
    };
    let cache_path = home.join(".codex").join("models_cache.json");
    let raw = match tokio::fs::read_to_string(&cache_path).await {
        Ok(r) => r,
        Err(e) => {
            provider.error = Some(format!(
                "Codex model cache unreadable at {}: {e}",
                cache_path.display()
            ));
            return provider;
        }
    };
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            provider.error = Some(format!("Codex model cache is not valid JSON: {e}"));
            return provider;
        }
    };

    let mut models = Vec::new();
    let mut union: Vec<String> = Vec::new();
    for m in parsed["models"].as_array().unwrap_or(&Vec::new()) {
        // `hide` entries are internal (e.g. auto-review) and must not be offered.
        if m["visibility"].as_str() == Some("hide") {
            continue;
        }
        let Some(id) = m["slug"].as_str() else {
            continue;
        };
        let efforts: Vec<String> = m["supported_reasoning_levels"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e["effort"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for e in &efforts {
            if !union.contains(e) {
                union.push(e.clone());
            }
        }
        models.push(CliModel {
            id: id.to_string(),
            label: m["display_name"].as_str().unwrap_or(id).to_string(),
            description: m["description"].as_str().map(str::to_string),
            default_effort: m["default_reasoning_level"].as_str().map(str::to_string),
            efforts,
        });
    }

    if models.is_empty() {
        provider.error = Some("Codex model cache contained no listable models".into());
        return provider;
    }
    if !union.is_empty() {
        provider.efforts = union;
    }
    provider.models = models;
    provider.source = "cache".into();
    provider
}

/// Antigravity (`agy`) prints one model per line from `agy models`. Effort is
/// both a suffix on the model name and a separate `--effort` flag.
async fn probe_antigravity() -> CliProvider {
    let binary = "agy";
    if !binary_present(binary).await {
        return CliProvider::missing("antigravity", "Antigravity (Local)", binary);
    }
    let version = probe(binary, &["--version"])
        .await
        .ok()
        .and_then(|s| first_line(&s));
    let help = probe(binary, &["--help"]).await.unwrap_or_default();
    let mut efforts = efforts_from_help(&help, "--effort");
    if efforts.is_empty() {
        efforts = vec!["low".into(), "medium".into(), "high".into()];
    }

    let listing = probe(binary, &["models"]).await;
    match listing {
        Ok(text) => {
            let models: Vec<CliModel> = text
                .lines()
                .map(str::trim)
                .filter(|l| {
                    !l.is_empty()
                        && !l.contains(char::is_whitespace)
                        && l.chars().all(|c| {
                            c.is_ascii_alphanumeric()
                                || c == '-'
                                || c == '.'
                                || c == '_'
                                || c == '/'
                        })
                })
                .map(CliModel::bare)
                .collect();
            if models.is_empty() {
                return CliProvider {
                    id: "antigravity".into(),
                    name: "Antigravity (Local)".into(),
                    binary: binary.into(),
                    available: true,
                    version,
                    source: "builtin".into(),
                    models: Vec::new(),
                    efforts,
                    error: Some("`agy models` returned nothing recognisable".into()),
                };
            }
            CliProvider {
                id: "antigravity".into(),
                name: "Antigravity (Local)".into(),
                binary: binary.into(),
                available: true,
                version,
                source: "cli".into(),
                models,
                efforts,
                error: None,
            }
        }
        Err(e) => CliProvider {
            id: "antigravity".into(),
            name: "Antigravity (Local)".into(),
            binary: binary.into(),
            available: true,
            version,
            source: "builtin".into(),
            models: Vec::new(),
            efforts,
            error: Some(e),
        },
    }
}

/// OpenCode prints `provider/model` per line. No reasoning-effort flag is
/// documented, so we report none rather than guessing.
async fn probe_opencode() -> CliProvider {
    let binary = "opencode";
    if !binary_present(binary).await {
        return CliProvider::missing("opencode", "OpenCode (Local)", binary);
    }
    let version = probe(binary, &["--version"])
        .await
        .ok()
        .and_then(|s| first_line(&s));
    match probe(binary, &["models"]).await {
        Ok(text) => {
            let models: Vec<CliModel> = text
                .lines()
                .map(str::trim)
                .filter(|l| l.contains('/') && !l.contains(char::is_whitespace) && !l.is_empty())
                .map(CliModel::bare)
                .collect();
            let empty = models.is_empty();
            CliProvider {
                id: "opencode".into(),
                name: "OpenCode (Local)".into(),
                binary: binary.into(),
                available: true,
                version,
                source: if empty {
                    "builtin".into()
                } else {
                    "cli".into()
                },
                models,
                efforts: Vec::new(),
                error: empty.then(|| "`opencode models` returned nothing recognisable".to_string()),
            }
        }
        Err(e) => CliProvider {
            id: "opencode".into(),
            name: "OpenCode (Local)".into(),
            binary: binary.into(),
            available: true,
            version,
            source: "builtin".into(),
            models: Vec::new(),
            efforts: Vec::new(),
            error: Some(e),
        },
    }
}

/// Grok lists models under an `Available models:` heading, one per line, marked
/// with `*` and sometimes annotated `(default)`.
async fn probe_grok() -> CliProvider {
    let binary = "grok";
    if !binary_present(binary).await {
        return CliProvider::missing("grok", "Grok (Local)", binary);
    }
    let version = probe(binary, &["--version"])
        .await
        .ok()
        .and_then(|s| first_line(&s));
    // `--reasoning-effort` exists but its help documents no fixed enum, so we
    // leave efforts empty instead of fabricating tiers.
    match probe(binary, &["models"]).await {
        Ok(text) => {
            let mut models = Vec::new();
            let mut in_list = false;
            for line in text.lines() {
                let t = line.trim();
                if t.to_lowercase().starts_with("available models") {
                    in_list = true;
                    continue;
                }
                if !in_list {
                    continue;
                }
                if t.is_empty() {
                    if !models.is_empty() {
                        break;
                    }
                    continue;
                }
                let cleaned = t
                    .trim_start_matches(['*', '-', '•'])
                    .trim()
                    .replace("(default)", "")
                    .trim()
                    .to_string();
                if !cleaned.is_empty() && !cleaned.contains(char::is_whitespace) {
                    models.push(CliModel::bare(cleaned));
                }
            }
            let empty = models.is_empty();
            CliProvider {
                id: "grok".into(),
                name: "Grok (Local)".into(),
                binary: binary.into(),
                available: true,
                version,
                source: if empty {
                    "builtin".into()
                } else {
                    "cli".into()
                },
                models,
                efforts: Vec::new(),
                error: empty.then(|| {
                    "`grok models` listed no models — you may need to authenticate".to_string()
                }),
            }
        }
        Err(e) => CliProvider {
            id: "grok".into(),
            name: "Grok (Local)".into(),
            binary: binary.into(),
            available: true,
            version,
            source: "builtin".into(),
            models: Vec::new(),
            efforts: Vec::new(),
            error: Some(e),
        },
    }
}

/// Copilot exposes `--model` but documents no enumeration command we can rely
/// on, so we report it present with an empty list and an explanation.
async fn probe_copilot() -> CliProvider {
    let binary = "copilot";
    if !binary_present(binary).await {
        return CliProvider::missing("copilot", "GitHub Copilot (Local)", binary);
    }
    let version = probe(binary, &["--version"])
        .await
        .ok()
        .and_then(|s| first_line(&s));
    CliProvider {
        id: "copilot".into(),
        name: "GitHub Copilot (Local)".into(),
        binary: binary.into(),
        available: true,
        version,
        source: "builtin".into(),
        models: Vec::new(),
        efforts: Vec::new(),
        error: Some(
            "Copilot does not expose a model listing command — enter a model id manually".into(),
        ),
    }
}

/// Gemini CLI. Ships a `--model` flag; enumeration support varies by version,
/// so we try `models` and fall back to reporting nothing rather than guessing.
async fn probe_gemini() -> CliProvider {
    let binary = "gemini";
    if !binary_present(binary).await {
        return CliProvider::missing("gemini", "Gemini CLI (Local)", binary);
    }
    let version = probe(binary, &["--version"])
        .await
        .ok()
        .and_then(|s| first_line(&s));
    match probe(binary, &["models"]).await {
        Ok(text) => {
            let models: Vec<CliModel> = text
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("gemini-") && !l.contains(char::is_whitespace))
                .map(CliModel::bare)
                .collect();
            let empty = models.is_empty();
            CliProvider {
                id: "gemini".into(),
                name: "Gemini CLI (Local)".into(),
                binary: binary.into(),
                available: true,
                version,
                source: if empty {
                    "builtin".into()
                } else {
                    "cli".into()
                },
                models,
                efforts: Vec::new(),
                error: empty.then(|| "`gemini models` returned nothing recognisable".to_string()),
            }
        }
        Err(e) => CliProvider {
            id: "gemini".into(),
            name: "Gemini CLI (Local)".into(),
            binary: binary.into(),
            available: true,
            version,
            source: "builtin".into(),
            models: Vec::new(),
            efforts: Vec::new(),
            error: Some(e),
        },
    }
}

// ---------------------------------------------------------------------------
// Discovery + cache
// ---------------------------------------------------------------------------

/// The sentinel `api_url` that routes a provider id to its CLI backend.
///
/// Single source of truth for the id↔URL mapping: the settings screen, the chat
/// composer and `toolbox::cli_backend_for` must all agree, and an empty string
/// here would silently send traffic down the HTTP path instead.
pub fn provider_sentinel_url(provider_id: &str) -> &'static str {
    match provider_id {
        "claude" => "claude-code",
        "codex" => "codex-cli",
        "gemini" => "gemini-cli",
        "antigravity" => "agy-cli",
        "opencode" => "opencode-cli",
        "grok" => "grok-cli",
        "copilot" => "copilot-cli",
        _ => "",
    }
}

/// True when `api_url` is a local-CLI sentinel rather than an HTTP endpoint.
///
/// Callers use this to skip anything that only makes sense for a hosted API —
/// requiring an API key, appending `/chat/completions`. Kept beside
/// [`provider_sentinel_url`] so adding a provider updates every check at once;
/// previously each site carried its own copy of the list and only knew about
/// the original three.
pub fn is_local_cli_url(api_url: &str) -> bool {
    let url = api_url.trim();
    !url.is_empty()
        && [
            "claude-code",
            "codex-cli",
            "gemini-cli",
            "agy-cli",
            "antigravity-cli",
            "opencode-cli",
            "grok-cli",
            "copilot-cli",
        ]
        .iter()
        .any(|s| url.starts_with(s))
}

type Cache = RwLock<Option<(Instant, Vec<CliProvider>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(None))
}

/// Probe every known CLI in parallel. Takes as long as the slowest probe.
async fn discover_all() -> Vec<CliProvider> {
    let (claude, codex, agy, opencode, grok, copilot, gemini) = tokio::join!(
        probe_claude(),
        probe_codex(),
        probe_antigravity(),
        probe_opencode(),
        probe_grok(),
        probe_copilot(),
        probe_gemini(),
    );
    vec![claude, codex, agy, opencode, grok, copilot, gemini]
}

/// Discovered providers, served from cache unless `force` or the entry is stale.
pub async fn get_providers(force: bool) -> Vec<CliProvider> {
    if !force {
        if let Some((at, cached)) = cache().read().await.as_ref() {
            if at.elapsed() < CACHE_TTL {
                return cached.clone();
            }
        }
    }
    let fresh = discover_all().await;
    debug!(
        "[CliModels] probed {} providers, {} available",
        fresh.len(),
        fresh.iter().filter(|p| p.available).count()
    );
    *cache().write().await = Some((Instant::now(), fresh.clone()));
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_effort_list_from_help_text() {
        let help = "  --effort <level>   Effort level for the current session (low, medium, high, xhigh, max)";
        assert_eq!(
            efforts_from_help(help, "--effort"),
            vec!["low", "medium", "high", "xhigh", "max"]
        );
    }

    #[test]
    fn parses_effort_list_that_wraps_onto_the_next_line() {
        let help = "  --effort <level>   Effort level for the current session\n                     (low, medium, high)";
        assert_eq!(
            efforts_from_help(help, "--effort"),
            vec!["low", "medium", "high"]
        );
    }

    #[test]
    fn ignores_prose_parentheses_that_are_not_an_effort_list() {
        let help = "  --effort <level>   Effort level (see docs)";
        assert!(efforts_from_help(help, "--effort").is_empty());
    }

    /// Live probe against whatever is installed on this machine. Ignored by
    /// default because it shells out and its result depends on the host; run it
    /// with `cargo test -- --ignored --nocapture live_discovery` to see exactly
    /// what the settings dropdowns will be populated with.
    #[tokio::test]
    #[ignore]
    async fn live_discovery_reports_installed_clis() {
        let providers = get_providers(true).await;
        for p in &providers {
            println!(
                "{:<14} available={:<5} source={:<8} models={:<3} efforts=[{}] {}",
                p.id,
                p.available,
                p.source,
                p.models.len(),
                p.efforts.join(","),
                p.error.as_deref().unwrap_or("")
            );
            for m in &p.models {
                println!("    {:<24} {}", m.id, m.efforts.join(","));
            }
        }
        assert!(
            !providers.is_empty(),
            "discovery must always report the known provider set"
        );
    }

    #[test]
    fn every_sentinel_url_is_recognised_as_a_local_cli() {
        // Each provider we can discover must also be routable, or selecting it
        // sends traffic down the HTTP path and demands an API key it has none of.
        for id in [
            "claude",
            "codex",
            "gemini",
            "antigravity",
            "opencode",
            "grok",
            "copilot",
        ] {
            let url = provider_sentinel_url(id);
            assert!(!url.is_empty(), "{id} has no sentinel url");
            assert!(
                is_local_cli_url(url),
                "{id} sentinel '{url}' not recognised as local"
            );
        }
    }

    #[test]
    fn http_endpoints_are_not_treated_as_local_clis() {
        for url in [
            "https://api.anthropic.com/v1",
            "http://localhost:1234/v1",
            "",
        ] {
            assert!(!is_local_cli_url(url), "{url} should not be local");
        }
    }

    #[test]
    fn missing_provider_is_marked_unavailable_with_a_reason() {
        let p = CliProvider::missing("x", "X", "xbin");
        assert!(!p.available);
        assert_eq!(p.source, "none");
        assert!(p.error.unwrap().contains("xbin"));
    }
}
