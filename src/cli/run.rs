//! Streaming agent turn for the CLI.
//!
//! Drives `chat::start_agent_run` in-process and renders `ToolUpdate`s to the
//! terminal: answer text streams as it arrives, tool calls show as dim
//! one-liners, approval requests become y/n prompts, Ctrl-C cancels the run.

use std::io::Write as _;
use std::sync::Arc;

use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, oneshot};

use crate::cli::project::{self, CliState};
use crate::server::routes::chat::{self, AgentRunRequest};
use crate::server::services::toolbox::{self, ToolUpdate};

pub struct RunOpts {
    /// Approve every tool without prompting (`--yes`).
    pub auto_approve: bool,
    /// One-shot `-p` mode: progress → stderr, only the final answer belongs
    /// on stdout (printed by the caller from the returned string).
    pub print_mode: bool,
    /// Transient model id override (`--model`).
    pub model_override: Option<String>,
}

const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

fn progress_line(opts: &RunOpts, line: &str) {
    if opts.print_mode {
        eprintln!("{}", line);
    } else {
        println!("{}", line);
    }
}

fn args_preview(args: &serde_json::Value) -> String {
    let s = serde_json::to_string(args).unwrap_or_default();
    crate::util::truncate_utf8(&s, 120).to_string()
}

async fn read_stdin_line() -> String {
    let mut line = String::new();
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    let _ = reader.read_line(&mut line).await;
    line.trim().to_string()
}

/// Run one agent turn. Returns the final assistant text; Err when the run
/// could not start or ended in an error with no answer.
pub async fn run_turn(state: &mut CliState, message: &str, opts: &RunOpts) -> Result<String, String> {
    let session_id = match &state.session_id {
        Some(s) => s.clone(),
        None => {
            let id = project::new_session_id();
            state.session_id = Some(id.clone());
            project::save_state(state).await;
            id
        }
    };

    let req = AgentRunRequest {
        session_id: session_id.clone(),
        message: message.to_string(),
        session_title: Some(message.chars().take(60).collect()),
        agent_mode: state.mode.clone(),
        agent_loop_profile: state.loop_profile.clone(),
        graph_profile: state.graph_profile.clone(),
        config_file: state.config_file.clone(),
        project_id: None,
        model: opts.model_override.clone(),
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<ToolUpdate>();
    let (done_tx, mut done_rx) = oneshot::channel::<String>();
    let cb: Arc<dyn Fn(ToolUpdate) + Send + Sync> = Arc::new(move |u| {
        let _ = tx.send(u);
    });

    chat::start_agent_run(req, Some(cb), Some(done_tx)).await?;

    let mut streamed = String::new();
    let mut last_error: Option<String> = None;
    let mut rx_open = true;
    let mut cancel_requested = false;

    let final_text = loop {
        tokio::select! {
            update = rx.recv(), if rx_open => match update {
                None => rx_open = false,
                Some(ToolUpdate::TextChunk(chunk)) => {
                    if !opts.print_mode {
                        print!("{}", chunk);
                        let _ = std::io::stdout().flush();
                    }
                    streamed.push_str(&chunk);
                }
                Some(ToolUpdate::ToolCall { name, args }) => {
                    progress_line(opts, &format!("{}⚙ {} {}{}", DIM, name, args_preview(&args), RESET));
                }
                Some(ToolUpdate::ToolResult { name, result }) => {
                    let failure = result
                        .get("error")
                        .and_then(|e| e.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            (result.get("ok").and_then(|o| o.as_bool()) == Some(false))
                                .then(|| "failed".to_string())
                        });
                    match failure {
                        Some(e) => progress_line(
                            opts,
                            &format!("{}✗ {} — {}{}", RED, name, crate::util::truncate_utf8(&e, 160), RESET),
                        ),
                        None => progress_line(opts, &format!("{}✓ {}{}", DIM, name, RESET)),
                    }
                }
                Some(ToolUpdate::Error(e)) => {
                    progress_line(opts, &format!("{}✗ {}{}", RED, e, RESET));
                    last_error = Some(e);
                }
                Some(ToolUpdate::ApprovalRequired { name, args }) => {
                    if opts.auto_approve {
                        progress_line(opts, &format!("{}auto-approved: {}{}", DIM, name, RESET));
                        toolbox::respond_tool_approval(true).await;
                    } else {
                        // Prompt on stderr in print mode so stdout stays clean.
                        let prompt = format!("Allow tool `{}`? {} [y/N]: ", name, args_preview(&args));
                        if opts.print_mode {
                            eprint!("{}", prompt);
                            let _ = std::io::stderr().flush();
                        } else {
                            print!("{}", prompt);
                            let _ = std::io::stdout().flush();
                        }
                        let answer = read_stdin_line().await;
                        let approved = matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes");
                        toolbox::respond_tool_approval(approved).await;
                        if !approved {
                            progress_line(opts, &format!("{}denied: {}{}", DIM, name, RESET));
                        }
                    }
                }
            },
            result = &mut done_rx => {
                break result.unwrap_or_default();
            }
            _ = tokio::signal::ctrl_c() => {
                if cancel_requested {
                    eprintln!("\nforce quit");
                    std::process::exit(130);
                }
                cancel_requested = true;
                eprintln!("\ncancelling… (Ctrl-C again to force quit)");
                chat::kill_session_by_id(&session_id).await;
            }
        }
    };

    if !opts.print_mode {
        if streamed.is_empty() {
            if !final_text.is_empty() {
                println!("{}", final_text);
            }
        } else {
            if !streamed.ends_with('\n') {
                println!();
            }
            // The persisted answer can differ from the streamed text (graph
            // gate rewrites, post-processing). The stream usually contains
            // reasoning + the answer, so only reprint when the answer is NOT
            // already the tail of what streamed.
            if !final_text.is_empty() && !streamed.trim_end().ends_with(final_text.trim()) {
                println!("{}", final_text);
            }
        }
        println!();
    }

    if cancel_requested && final_text.is_empty() {
        return Err("cancelled".to_string());
    }
    if final_text.is_empty() {
        if let Some(e) = last_error {
            return Err(e);
        }
    }
    Ok(final_text)
}
