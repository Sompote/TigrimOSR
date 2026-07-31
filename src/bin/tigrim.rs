//! `tigrim` — the TigrimOS CLI (Claude Code-style).
//!
//! Copy the binary into any folder and run it there: the folder becomes the
//! agent's workspace, `.tigrimos/` holds project-local agent/loop/graph YAML
//! and per-folder chat history, and anything missing falls back to the global
//! TigrimOS data directory (shared with the desktop app).

use std::io::Write as _;

use tigrimos::cli::project;
use tigrimos::cli::repl;
use tigrimos::cli::run::{run_turn, RunOpts};
use tigrimos::server;
use tigrimos::server::data;
use tigrimos::server::services::messaging::SUB_AGENT_MODES;

const USAGE: &str = "\
tigrim — TigrimOS agent CLI

Usage:
  tigrim                      Interactive REPL in the current folder
  tigrim -p \"prompt\"          One-shot run: final answer on stdout, progress on stderr

Options (one-shot mode):
  --mode <m>        Sub-agent mode: single|auto|manual|fully_auto|router|graph
  --loop <name>     Agent-loop profile (from .tigrimos/agent_loops/ or global)
  --graph <name>    Graph profile (activates graph mode)
  --agent <file>    Agent team config YAML (from .tigrimos/agents/ or global)
  --model <id>      Model id override for this run
  --session <id>    Continue an existing session id
  --new             Start a fresh session (persisted for this folder)
  --yes, -y         Auto-approve all tool executions
  --cwd <dir>       Run as if launched from <dir>
  -h, --help        Show this help
  -V, --version     Show version

Exit codes: 0 success, 1 run failed, 2 usage/config error, 130 interrupted.";

struct CliArgs {
    print_prompt: Option<String>,
    mode: Option<String>,
    loop_profile: Option<String>,
    graph_profile: Option<String>,
    agent_config: Option<String>,
    model: Option<String>,
    session: Option<String>,
    new_session: bool,
    yes: bool,
    cwd: Option<String>,
}

fn usage_error(msg: &str) -> ! {
    eprintln!("tigrim: {}\n\n{}", msg, USAGE);
    std::process::exit(2);
}

fn parse_args() -> CliArgs {
    let mut out = CliArgs {
        print_prompt: None,
        mode: None,
        loop_profile: None,
        graph_profile: None,
        agent_config: None,
        model: None,
        session: None,
        new_session: false,
        yes: false,
        cwd: None,
    };
    let mut args = std::env::args().skip(1);
    let take = |args: &mut dyn Iterator<Item = String>, flag: &str| -> String {
        args.next().unwrap_or_else(|| usage_error(&format!("{} requires a value", flag)))
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "-p" | "--print" => out.print_prompt = Some(take(&mut args, "-p")),
            "--mode" => out.mode = Some(take(&mut args, "--mode")),
            "--loop" => out.loop_profile = Some(take(&mut args, "--loop")),
            "--graph" => out.graph_profile = Some(take(&mut args, "--graph")),
            "--agent" => out.agent_config = Some(take(&mut args, "--agent")),
            "--model" => out.model = Some(take(&mut args, "--model")),
            "--session" => out.session = Some(take(&mut args, "--session")),
            "--new" => out.new_session = true,
            "--yes" | "-y" => out.yes = true,
            "--cwd" => out.cwd = Some(take(&mut args, "--cwd")),
            "-h" | "--help" => {
                println!("{}", USAGE);
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("tigrim {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => usage_error(&format!("unknown argument '{}'", other)),
        }
    }
    out
}

fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

fn prompt_line(label: &str, default: &str) -> String {
    if default.is_empty() {
        print!("{}: ", label);
    } else {
        print!("{} [{}]: ", label, default);
    }
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    let trimmed = input.trim().to_string();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed
    }
}

/// Interactive setup when this FOLDER has no API key. The CLI is folder-local
/// by design — it never reads the desktop app's global settings — so the
/// wizard saves credentials to `.tigrimos/.env` (gitignored) in this folder.
async fn first_run_wizard(proj: &std::path::Path) -> bool {
    let settings = data::get_settings().await;
    if !settings.tiger_bot_api_key.is_empty() {
        return true; // configured via .env or .tigrimos/settings.json
    }
    let env_path = proj.join(".env");
    if !is_tty() {
        eprintln!(
            "tigrim: no API key configured for this folder. Create {} with:\n  TIGRIMOS_API_KEY=sk-...\n  TIGRIMOS_API_URL=https://api.deepseek.com/v1\n  TIGRIMOS_MODEL=deepseek-chat",
            env_path.display()
        );
        return false;
    }
    println!("Welcome to TigrimOS CLI — setup for THIS folder (saved to .tigrimos/.env, gitignored).");
    let api_url = prompt_line("API base URL", "https://api.deepseek.com/v1");
    let api_key = prompt_line("API key", "");
    if api_key.is_empty() {
        eprintln!("tigrim: an API key is required.");
        return false;
    }
    let model = prompt_line("Model id", "deepseek-chat");
    let env_content = format!(
        "# TigrimOS CLI setup for this folder — gitignored, never commit keys.\nTIGRIMOS_API_KEY={}\nTIGRIMOS_API_URL={}\nTIGRIMOS_MODEL={}\n",
        api_key, api_url, model
    );
    if let Err(e) = std::fs::write(&env_path, env_content) {
        eprintln!("tigrim: could not write {}: {}", env_path.display(), e);
        return false;
    }
    // Make the values live for this session too.
    std::env::set_var("TIGRIMOS_API_KEY", &api_key);
    std::env::set_var("TIGRIMOS_API_URL", &api_url);
    std::env::set_var("TIGRIMOS_MODEL", &model);
    println!("Saved to {} — edit it anytime.\n", env_path.display());
    true
}

fn main() {
    let args = parse_args();

    // Pin the global data dir BEFORE anything touches data_dir() — the
    // desktop's `./data`-exists heuristic must not hijack project folders.
    let global_data = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("TigrimOS")
        .join("data");
    data::set_global_data_dir(global_data.clone());

    // .env: global data dir first, then the working directory.
    let _ = dotenvy::from_path(global_data.join(".env"));
    let _ = dotenvy::dotenv();

    // Keep agent subprocess spawning working from any launch context.
    let current_path = std::env::var("PATH").unwrap_or_default();
    if !current_path.contains("/opt/homebrew/bin") || !current_path.contains("/usr/local/bin") {
        std::env::set_var(
            "PATH",
            format!("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}", current_path),
        );
    }

    // Quiet logs on stderr — stdout belongs to answers (-p pipes depend on it).
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .init();

    if let Some(ref dir) = args.cwd {
        if let Err(e) = std::env::set_current_dir(dir) {
            usage_error(&format!("--cwd {}: {}", dir, e));
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let proj = project::init_project(&cwd);
    // Project-local .env is the folder's setup file (API key/url/model as
    // TIGRIMOS_* vars) — loaded after the global/cwd .env so it wins.
    let _ = dotenvy::from_path_override(proj.join(".env"));

    let runtime = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");

    // Seed global data files/profiles/skills and connect MCP servers. The
    // seeded settings use the GLOBAL default sandbox so the desktop app's
    // defaults are unaffected; CLI runs use the cwd via the project override.
    let global_sandbox = global_data
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("sandbox")
        .to_string_lossy()
        .to_string();
    runtime.block_on(server::bootstrap_data(&global_sandbox));
    // Copy editable starter YAMLs (loop/graph/team) into empty project dirs
    // now that the global defaults exist.
    project::seed_examples(&proj);

    if !runtime.block_on(first_run_wizard(&proj)) {
        std::process::exit(2);
    }

    // Validate transient flag overrides early — a bad name is exit 2, not a
    // silently ignored fallback mid-run.
    if let Some(ref m) = args.mode {
        if !SUB_AGENT_MODES.contains(&m.as_str()) {
            usage_error(&format!("unknown mode '{}' (valid: {})", m, SUB_AGENT_MODES.join(", ")));
        }
    }
    if let Some(ref name) = args.loop_profile {
        if tigrimos::server::services::agent_loop::load_profile(name).is_none() {
            usage_error(&format!("agent-loop profile '{}' not found", name));
        }
    }
    if let Some(ref name) = args.graph_profile {
        if tigrimos::server::services::graph::load_profile(name).is_none() {
            usage_error(&format!("graph profile '{}' not found", name));
        }
    }
    if let Some(ref f) = args.agent_config {
        let filename = if f.ends_with(".yaml") || f.ends_with(".yml") {
            f.clone()
        } else {
            format!("{}.yaml", f)
        };
        if tigrimos::server::services::toolbox::load_agent_yaml(&filename).is_none() {
            usage_error(&format!("agent config '{}' not found", filename));
        }
    }

    if let Some(prompt) = args.print_prompt {
        let code = runtime.block_on(async {
            let mut state = project::load_state().await;
            // Session selection persists; other flags are transient overrides.
            if args.new_session {
                state.session_id = Some(project::new_session_id());
                project::save_state(&state).await;
            } else if let Some(ref sid) = args.session {
                state.session_id = Some(sid.clone());
                project::save_state(&state).await;
            }
            if let Some(m) = args.mode.clone() {
                state.mode = if m == "single" { None } else { Some(m) };
            }
            if args.loop_profile.is_some() {
                state.loop_profile = args.loop_profile.clone();
            }
            if let Some(g) = args.graph_profile.clone() {
                state.graph_profile = Some(g);
                state.mode = Some("graph".to_string());
            }
            if let Some(f) = args.agent_config.clone() {
                let filename = if f.ends_with(".yaml") || f.ends_with(".yml") {
                    f
                } else {
                    format!("{}.yaml", f)
                };
                state.config_file = Some(filename);
                if state.mode.as_deref().unwrap_or("single") == "single" {
                    state.mode = Some("auto".to_string());
                }
            }
            let opts = RunOpts {
                auto_approve: args.yes,
                print_mode: true,
                model_override: args.model.clone(),
            };
            match run_turn(&mut state, &prompt, &opts).await {
                Ok(answer) => {
                    println!("{}", answer);
                    0
                }
                Err(e) => {
                    eprintln!("tigrim: {}", e);
                    1
                }
            }
        });
        std::process::exit(code);
    }

    // Interactive REPL.
    let (model, mode, loop_profile) = runtime.block_on(async {
        let s = data::get_settings().await;
        let st = project::load_state().await;
        (
            s.tiger_bot_model,
            st.mode.unwrap_or_else(|| "single".to_string()),
            st.loop_profile
                .or(s.agent_loop_profile.filter(|p| !p.is_empty()))
                .unwrap_or_else(|| "(built-in)".to_string()),
        )
    });
    let model_src = if std::env::var("TIGRIMOS_MODEL").map(|v| !v.trim().is_empty()).unwrap_or(false)
        || data::api_key_from_env()
    {
        ".env"
    } else {
        "folder settings"
    };
    let model_display = if model.is_empty() { "deepseek-chat (default)".to_string() } else { model };
    println!("TigrimOS CLI v{}", env!("CARGO_PKG_VERSION"));
    println!("Model: {} ({})   Mode: {}   Loop: {}", model_display, model_src, mode, loop_profile);
    println!("Workspace: {}", cwd.display());
    println!("Type a message to chat, /help for commands. ESC or Ctrl-C cancels a run.\n");

    let code = repl::run_repl(&runtime);
    std::process::exit(code);
}
