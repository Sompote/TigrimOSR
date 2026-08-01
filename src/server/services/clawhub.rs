use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::process::Command;

/// Find the clawhub CLI binary — checks node_modules first, then PATH
async fn find_clawhub_bin() -> Option<String> {
    let candidates = [
        "Tiger_bot/node_modules/.bin/clawhub",
        "clawhub",
    ];
    for bin in &candidates {
        if Command::new(bin)
            .arg("--cli-version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(bin.to_string());
        }
    }
    None
}

fn workdir() -> PathBuf {
    std::path::Path::new("Tiger_bot")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("Tiger_bot"))
}

/// Search the ClawHub/OpenClaw skill marketplace
pub async fn clawhub_search(query: &str, limit: usize) -> Value {
    let bin = match find_clawhub_bin().await {
        Some(b) => b,
        None => return json!({ "ok": false, "error": "clawhub CLI not found" }),
    };
    let limit = limit.clamp(1, 50);
    let wd = workdir();
    let wd_str = wd.display().to_string();

    match Command::new(&bin)
        .args([
            "search", query,
            "--limit", &limit.to_string(),
            "--no-input",
            "--workdir", &wd_str,
            "--dir", "skills",
        ])
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            json!({ "ok": output.status.success(), "output": stdout.trim(), "warning": stderr.trim() })
        }
        Err(e) => json!({ "ok": false, "error": format!("Failed to run clawhub: {e}") }),
    }
}

/// Install a skill from ClawHub by slug
pub async fn clawhub_install(slug: &str, force: bool) -> Value {
    if !regex::Regex::new(r"^[a-z0-9][a-z0-9-]*$").unwrap().is_match(slug) {
        return json!({ "ok": false, "error": "Invalid slug format" });
    }

    let bin = match find_clawhub_bin().await {
        Some(b) => b,
        None => return json!({ "ok": false, "error": "clawhub CLI not found" }),
    };
    let wd = workdir();
    let wd_str = wd.display().to_string();

    let mut argv: Vec<&str> = vec![
        "install", slug, "--no-input",
        "--workdir", &wd_str,
        "--dir", "skills",
    ];
    if force {
        argv.push("--force");
    }

    match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        Command::new(&bin).args(&argv).output(),
    )
    .await
    {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            json!({
                "ok": output.status.success(),
                "slug": slug,
                "output": stdout.trim(),
                "warning": stderr.trim()
            })
        }
        Ok(Err(e)) => json!({ "ok": false, "error": format!("Failed to run clawhub: {e}") }),
        Err(_) => json!({ "ok": false, "error": "clawhub install timed out (120s)" }),
    }
}

/// Get detailed info about a skill by slug
pub async fn clawhub_info(slug: &str) -> Value {
    let bin = match find_clawhub_bin().await {
        Some(b) => b,
        None => return json!({ "ok": false, "error": "clawhub CLI not found" }),
    };
    let wd = workdir();
    let wd_str = wd.display().to_string();

    match Command::new(&bin)
        .args(["info", slug, "--no-input", "--workdir", &wd_str, "--dir", "skills"])
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            json!({ "ok": output.status.success(), "info": stdout.trim() })
        }
        Err(e) => json!({ "ok": false, "error": format!("Failed to run clawhub: {e}") }),
    }
}

/// List installed skills with version info from data/skills.json and skill directories
pub async fn list_installed_skills() -> Value {
    // Read from skills.json (project-local in CLI mode)
    let skills: Vec<Value> = match tokio::fs::read_to_string(crate::server::data::data_file_path("skills.json")).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => vec![],
    };

    // Also scan skills/ directory for SKILL.md files
    let mut skill_dirs = vec![];
    if let Ok(mut entries) = tokio::fs::read_dir("skills").await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                let skill_md = format!("skills/{}/SKILL.md", name);
                let has_skill_md = tokio::fs::metadata(&skill_md).await.is_ok();
                skill_dirs.push(json!({ "name": name, "hasSkillMd": has_skill_md }));
            }
        }
    }

    json!({ "ok": true, "skills": skills, "directories": skill_dirs })
}

/// Read a skill's SKILL.md content
pub async fn read_skill(name: &str) -> Value {
    let path = format!("skills/{}/SKILL.md", name);
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => json!({ "ok": true, "name": name, "content": content }),
        Err(_) => json!({ "ok": false, "error": format!("Skill '{}' not found or SKILL.md missing", name) }),
    }
}
