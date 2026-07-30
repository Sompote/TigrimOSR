use std::collections::HashSet;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Local};
use tracing::{info, warn};

use crate::server::data::{
    get_remote_backend, get_sandbox_dir_sync, get_tasks, save_tasks, ScheduledTask,
};

const TICK_SECS: u64 = 30;
const JOB_TIMEOUT_SECS: u64 = 300;
const MAX_RESULT_LEN: usize = 2_000;

fn running_jobs() -> &'static Mutex<HashSet<String>> {
    static RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// The `cron` crate requires a seconds field; UI presets and crontab habits
/// use the classic 5-field form (min hour dom mon dow).
fn normalize_cron(expr: &str) -> String {
    let expr = expr.trim();
    if expr.split_whitespace().count() == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    }
}

pub fn validate_cron(expr: &str) -> Result<(), String> {
    cron::Schedule::from_str(&normalize_cron(expr))
        .map(|_| ())
        .map_err(|e| format!("Invalid cron expression '{expr}': {e}"))
}

/// True if the task's schedule fired at some instant in (since, now].
fn due_since(task: &ScheduledTask, since: DateTime<Local>, now: DateTime<Local>) -> bool {
    let Ok(schedule) = cron::Schedule::from_str(&normalize_cron(&task.cron)) else {
        return false;
    };
    schedule
        .after(&since)
        .next()
        .map(|t| t <= now)
        .unwrap_or(false)
}

pub async fn init_scheduler() {
    info!("[Scheduler] Started (tick every {TICK_SECS}s)");
    tokio::spawn(async move {
        let mut last_tick = Local::now();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(TICK_SECS)).await;
            let now = Local::now();
            // In remote mode the headless server owns task execution —
            // running them here too would double-execute every job.
            if get_remote_backend().is_some() {
                last_tick = now;
                continue;
            }
            for task in get_tasks().await {
                if task.enabled && due_since(&task, last_tick, now) {
                    spawn_task_run(task);
                }
            }
            last_tick = now;
        }
    });
}

fn spawn_task_run(task: ScheduledTask) {
    {
        let mut running = running_jobs().lock().unwrap();
        if !running.insert(task.id.clone()) {
            warn!(
                "[Scheduler] Skipping '{}': previous run still in progress",
                task.name
            );
            return;
        }
    }
    tokio::spawn(async move {
        info!("[Scheduler] Running '{}': {}", task.name, task.command);
        let result = run_command(&task.command).await;
        match &result {
            Ok(_) => info!("[Scheduler] '{}' finished: success", task.name),
            Err(e) => warn!("[Scheduler] '{}' failed: {}", task.name, e),
        }
        record_result(&task.id, &result).await;
        running_jobs().lock().unwrap().remove(&task.id);
    });
}

async fn run_command(command: &str) -> Result<String, String> {
    let cwd = get_sandbox_dir_sync();
    // Cross-platform shell: cmd.exe on Windows, sh elsewhere.
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd.exe");
        c.arg("/C").arg(command);
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    let child = cmd
        .current_dir(&cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;

    let timeout = std::time::Duration::from_secs(JOB_TIMEOUT_SECS);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if out.status.success() {
                Ok(crate::util::truncate_utf8_ellipsis(&stdout, MAX_RESULT_LEN))
            } else {
                let code = out.status.code().unwrap_or(-1);
                let detail = if stderr.trim().is_empty() {
                    &stdout
                } else {
                    &stderr
                };
                Err(format!(
                    "exit {code}: {}",
                    crate::util::truncate_utf8_ellipsis(detail.trim(), MAX_RESULT_LEN)
                ))
            }
        }
        Ok(Err(e)) => Err(format!("wait failed: {e}")),
        Err(_) => Err(format!("timed out after {JOB_TIMEOUT_SECS}s")),
    }
}

async fn record_result(task_id: &str, result: &Result<String, String>) {
    let mut tasks = get_tasks().await;
    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
        task.last_run = Some(chrono::Utc::now().to_rfc3339());
        // UI shows green only for the literal string "success"
        task.last_result = Some(match result {
            Ok(_) => "success".to_string(),
            Err(e) => format!("error: {e}"),
        });
        save_tasks(&tasks).await;
    }
}

/// Execute a task immediately (manual trigger from UI/API/chat).
/// Returns the captured stdout on success.
pub async fn run_task_now(task_id: &str) -> Result<String, String> {
    let tasks = get_tasks().await;
    let task = tasks
        .into_iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| "Task not found".to_string())?;
    let result = run_command(&task.command).await;
    record_result(&task.id, &result).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_field_cron_is_accepted() {
        assert!(validate_cron("0 9 * * *").is_ok());
        assert!(validate_cron("*/15 * * * *").is_ok());
        assert!(validate_cron("30 8 * * 1-5").is_ok());
    }

    #[test]
    fn six_field_cron_is_accepted() {
        assert!(validate_cron("0 0 9 * * *").is_ok());
    }

    #[test]
    fn invalid_cron_is_rejected() {
        assert!(validate_cron("not a cron").is_err());
        assert!(validate_cron("99 * * * *").is_err());
        assert!(validate_cron("").is_err());
    }

    #[test]
    fn due_detection_fires_within_window() {
        use chrono::TimeZone;
        let task = ScheduledTask {
            id: "t1".into(),
            name: "test".into(),
            cron: "*/5 * * * *".into(),
            command: "true".into(),
            enabled: true,
            last_run: None,
            last_result: None,
            created_at: String::new(),
            source: None,
        };
        let since = Local.with_ymd_and_hms(2026, 6, 13, 10, 4, 30).unwrap();
        let now = Local.with_ymd_and_hms(2026, 6, 13, 10, 5, 10).unwrap();
        assert!(due_since(&task, since, now)); // 10:05:00 falls in window
        let now_early = Local.with_ymd_and_hms(2026, 6, 13, 10, 4, 50).unwrap();
        assert!(!due_since(&task, since, now_early)); // window ends before 10:05
    }
}
