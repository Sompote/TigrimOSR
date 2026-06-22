//! Per-session OS-process tracking.
//!
//! Killing a chat task used to only flip an in-process `cancel_flag` that the
//! tool loop checked at round boundaries. Any child process the session had
//! already spawned (a `python3` script, a `sh -c` pipeline, a CLI agent, …)
//! kept running — and kept whatever filesystem/network access it had — because
//! nothing ever signalled the OS process. `kill_on_drop(true)` only fires when
//! the spawning future is dropped (it never was, since the task wasn't aborted)
//! and even then it SIGKILLs only the *direct* child, leaving grandchildren
//! (e.g. `python` launched under `sh -c`) orphaned and alive.
//!
//! This registry closes that hole. Every guarded spawn puts its child in its
//! OWN process group (`Command::process_group(0)`, so the child's PID == its
//! PGID) and records the PID under the owning session. On a kill we
//! `kill(-pgid, SIGKILL)` each tracked group, reaping the entire subtree —
//! direct child *and* descendants — in one signal.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

static SESSION_PIDS: OnceLock<Mutex<HashMap<String, Vec<u32>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, Vec<u32>>> {
    SESSION_PIDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record a process-group leader PID as belonging to `session_id`.
pub fn register(session_id: &str, pid: u32) {
    registry()
        .lock()
        .unwrap()
        .entry(session_id.to_string())
        .or_default()
        .push(pid);
}

/// Drop a PID once its process has exited normally (so a later kill doesn't
/// signal a reused PID).
pub fn unregister(session_id: &str, pid: u32) {
    let mut reg = registry().lock().unwrap();
    if let Some(pids) = reg.get_mut(session_id) {
        pids.retain(|&p| p != pid);
        if pids.is_empty() {
            reg.remove(session_id);
        }
    }
}

/// SIGKILL every process group still tracked for `session_id`. Returns how many
/// groups were signalled. Called when a chat task is killed so nothing the
/// session started outlives it.
pub fn kill_session(session_id: &str) -> usize {
    let pids = registry()
        .lock()
        .unwrap()
        .remove(session_id)
        .unwrap_or_default();
    let mut killed = 0;
    for pid in pids {
        if kill_group(pid) {
            killed += 1;
        }
    }
    if killed > 0 {
        tracing::info!("[proc] killed {} process group(s) for session {}", killed, session_id);
    }
    killed
}

/// SIGKILL the entire process group led by `pid`.
#[cfg(unix)]
pub fn kill_group(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // A negative PID signals the whole process group whose id is `pid`.
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) == 0 }
}

/// Best-effort tree kill on non-unix targets (no process groups).
#[cfg(not(unix))]
pub fn kill_group(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
