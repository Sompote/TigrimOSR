use crate::server::data::{get_tasks, ScheduledTask};

pub async fn init_scheduler() {
    tracing::info!("[Scheduler] Initialized");
    // Load and schedule enabled tasks
    let tasks = get_tasks().await;
    for task in &tasks {
        if task.enabled {
            tracing::info!("[Scheduler] Would schedule task: {} ({})", task.name, task.cron);
        }
    }
}

pub fn schedule_task(_task: &ScheduledTask) {
    // Stub - would use tokio-cron-scheduler
}

pub fn stop_task(_id: &str) {
    // Stub
}
