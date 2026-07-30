//! Interactive rustyline REPL for the CLI.
//!
//! Readline runs synchronously on the main thread; each dispatch is
//! `block_on`-driven, so streamed output never interleaves with an active
//! prompt. History lives in `.tigrimos/repl_history`.

use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::cli::commands::{self, Outcome};
use crate::cli::project;
use crate::cli::run::{self, RunOpts};
use crate::server::data;

pub fn run_repl(runtime: &tokio::runtime::Runtime) -> i32 {
    let mut rl = match DefaultEditor::new() {
        Ok(rl) => rl,
        Err(e) => {
            eprintln!("Failed to initialize terminal input: {}", e);
            return 1;
        }
    };
    let history_path = data::project_dir().map(|p| p.join("repl_history"));
    if let Some(ref hp) = history_path {
        let _ = rl.load_history(hp);
    }

    let mut state = runtime.block_on(project::load_state());
    let mut interrupted = false;
    loop {
        match rl.readline("tigrim> ") {
            Ok(line) => {
                interrupted = false;
                if line.trim().is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(&line);
                let cmd = commands::parse(&line);
                match runtime.block_on(commands::execute(&mut state, cmd)) {
                    Outcome::Reply(text) => {
                        if !text.is_empty() {
                            println!("{}\n", text);
                        }
                    }
                    Outcome::RunChat(message) => {
                        let opts = RunOpts {
                            auto_approve: false,
                            print_mode: false,
                            model_override: None,
                        };
                        if let Err(e) = runtime.block_on(run::run_turn(&mut state, &message, &opts)) {
                            eprintln!("\x1b[31m{}\x1b[0m\n", e);
                        }
                    }
                    Outcome::ClearScreen => {
                        print!("\x1b[2J\x1b[H");
                        use std::io::Write as _;
                        let _ = std::io::stdout().flush();
                    }
                    Outcome::Exit => break,
                }
            }
            Err(ReadlineError::Interrupted) => {
                if interrupted {
                    break;
                }
                interrupted = true;
                println!("(Ctrl-C — press again to exit, or /exit)");
            }
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("Input error: {}", e);
                break;
            }
        }
    }
    if let Some(ref hp) = history_path {
        let _ = rl.save_history(hp);
    }
    0
}
