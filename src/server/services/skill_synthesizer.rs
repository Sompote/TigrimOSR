use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::server::data::{
    get_chat_history, get_settings, get_skills, save_settings, save_skills,
    ChatSession, Skill, SkillAutoMeta,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillProposal {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: String, // "create" or "update"
    pub content: String, // full SKILL.md content
    pub rationale: String,
    pub based_on: Vec<String>, // session IDs
    pub generated_at: String,
    pub model: String,
    pub review_status: String, // "pending", "approved", "rejected"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_content: Option<String>, // for diff on updates
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizerStatus {
    pub running: bool,
    pub last_run_at: Option<String>,
    pub last_run_summary: Option<String>,
    pub proposals: Vec<SkillProposal>,
}

impl Default for SynthesizerStatus {
    fn default() -> Self {
        Self {
            running: false,
            last_run_at: None,
            last_run_summary: None,
            proposals: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

fn synth_status() -> &'static Arc<Mutex<SynthesizerStatus>> {
    static INSTANCE: OnceLock<Arc<Mutex<SynthesizerStatus>>> = OnceLock::new();
    INSTANCE.get_or_init(|| Arc::new(Mutex::new(SynthesizerStatus::default())))
}

pub async fn get_synth_status() -> SynthesizerStatus {
    synth_status().lock().await.clone()
}

// ---------------------------------------------------------------------------
// Skill file helpers
// ---------------------------------------------------------------------------

fn skills_dir() -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    base.join("data").join("skills")
}

async fn list_existing_skills() -> Vec<(String, String)> {
    let dir = skills_dir();
    let mut skills = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                let skill_md = entry.path().join("SKILL.md");
                if let Ok(content) = tokio::fs::read_to_string(&skill_md).await {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Truncate for prompt
                    let truncated = if content.len() > 4000 {
                        format!("{}...(truncated)", &content[..4000])
                    } else {
                        content
                    };
                    skills.push((name, truncated));
                }
            }
        }
    }
    skills
}

async fn write_skill(name: &str, content: &str, proposed: bool) -> Result<(), String> {
    let dir = skills_dir().join(name);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create skill dir: {e}"))?;

    let filename = if proposed {
        "SKILL.md.proposed"
    } else {
        "SKILL.md"
    };

    tokio::fs::write(dir.join(filename), content)
        .await
        .map_err(|e| format!("Failed to write {filename}: {e}"))
}

// ---------------------------------------------------------------------------
// Session analysis
// ---------------------------------------------------------------------------

fn is_error_reply(content: &str) -> bool {
    let lower = content.to_lowercase();
    lower.starts_with("error:")
        || lower.starts_with("i'm sorry")
        || lower.contains("i cannot")
        || lower.contains("i can't help")
        || (lower.len() < 50 && lower.contains("error"))
}

fn summarise_session(session: &ChatSession) -> Option<String> {
    if session.messages.len() < 2 {
        return None;
    }

    // Must have at least one user and one assistant message
    let has_user = session.messages.iter().any(|m| m.role == "user");
    let has_assistant = session.messages.iter().any(|m| m.role == "assistant");
    if !has_user || !has_assistant {
        return None;
    }

    // Last assistant reply should not be an error
    if let Some(last_asst) = session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
    {
        if is_error_reply(&last_asst.content) {
            return None;
        }
    }

    // Build a compact summary
    let mut summary = String::new();
    summary.push_str(&format!("Session: {} ({})\n", session.title, session.id));

    // Check for feedback
    let has_thumbs_down = session.messages.iter().any(|m| {
        m.feedback
            .as_ref()
            .and_then(|f| f.rating.as_deref())
            .map(|r| r == "down")
            .unwrap_or(false)
    });
    if has_thumbs_down {
        summary.push_str("[HAS NEGATIVE FEEDBACK]\n");
    }

    for msg in &session.messages {
        let role = &msg.role;
        let content = if msg.content.len() > 500 {
            format!("{}...", &msg.content[..500])
        } else {
            msg.content.clone()
        };
        summary.push_str(&format!("{}: {}\n", role, content));
    }

    Some(summary)
}

// ---------------------------------------------------------------------------
// LLM skill synthesis
// ---------------------------------------------------------------------------

async fn call_llm_for_skills(
    api_key: &str,
    api_url: &str,
    model: &str,
    session_summaries: &[String],
    existing_skills: &[(String, String)],
) -> Result<Vec<Value>, String> {
    let skills_list = existing_skills
        .iter()
        .map(|(name, content)| format!("### {}\n{}", name, content))
        .collect::<Vec<_>>()
        .join("\n\n");

    let sessions_text = session_summaries.join("\n---\n");

    let system_prompt = r#"You are the SkillSynthesiser for TigrimOS. Your job is to analyze chat sessions and propose reusable SKILL.md files.

Rules:
1. Output ONLY valid JSON array. No markdown fences, no explanation.
2. Each element: {"kind":"create"|"update", "name":"kebab-case", "description":"max 200 chars", "content":"full SKILL.md with YAML frontmatter", "rationale":"why this skill"}
3. name: [a-zA-Z0-9_-]+, max 64 chars
4. content must include YAML frontmatter with name and description
5. Only propose skills for patterns that appear useful and reusable
6. For "update", name must match an existing skill
7. If no useful skills can be extracted, return []
8. Maximum 5 proposals per run"#;

    let user_prompt = format!(
        "## Existing Skills\n{}\n\n## Recent Chat Sessions\n{}\n\nAnalyze these sessions and propose new or updated skills. Return JSON array only.",
        if skills_list.is_empty() {
            "(none)".to_string()
        } else {
            skills_list
        },
        sessions_text
    );

    let client = Client::new();
    let body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt}
        ],
        "temperature": 0.3,
        "max_tokens": 4096,
    });

    let resp = client
        .post(api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("API request failed: {e}"))?;

    let resp_json: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {e}"))?;

    if let Some(err) = resp_json.get("error") {
        return Err(format!("API error: {}", err));
    }

    let raw_content = resp_json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("[]");

    info!("[SkillSynth] LLM raw response length: {} chars", raw_content.len());

    // Strip <think>...</think> tags (models like DeepSeek, MiniMax output these)
    let content = {
        let mut s = raw_content.to_string();
        while let Some(start) = s.find("<think>") {
            if let Some(end) = s.find("</think>") {
                s = format!("{}{}", &s[..start], &s[end + 8..]);
            } else {
                // Unclosed think tag — remove everything from <think> onwards
                s = s[..start].to_string();
                break;
            }
        }
        s
    };

    info!("[SkillSynth] After stripping think tags: {} chars", content.len());
    info!("[SkillSynth] Content preview: {}", &content[..content.len().min(500)]);

    // Extract JSON from potential markdown fences
    let json_str = if let Some(start) = content.find('[') {
        if let Some(end) = content.rfind(']') {
            &content[start..=end]
        } else {
            warn!("[SkillSynth] Found '[' but no matching ']' in LLM response");
            "[]"
        }
    } else {
        warn!("[SkillSynth] No JSON array found in LLM response");
        "[]"
    };

    let proposals: Vec<Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            error!("[SkillSynth] Failed to parse JSON: {e}");
            error!("[SkillSynth] JSON string was: {}", &json_str[..json_str.len().min(500)]);
            return Err(format!("Invalid JSON from LLM: {e}"));
        }
    };

    info!("[SkillSynth] Parsed {} proposals from LLM", proposals.len());
    Ok(proposals)
}

// ---------------------------------------------------------------------------
// Core synthesis run
// ---------------------------------------------------------------------------

pub async fn run_synthesis_forced() -> Result<String, String> {
    run_synthesis_inner(true).await
}

pub async fn run_synthesis() -> Result<String, String> {
    run_synthesis_inner(false).await
}

async fn run_synthesis_inner(force: bool) -> Result<String, String> {
    info!("[SkillSynth] Starting synthesis run (force={})", force);

    {
        let mut status = synth_status().lock().await;
        status.running = true;
    }

    let settings = get_settings().await;
    let api_key = settings.tiger_bot_api_key.clone();
    let api_url_raw = settings.tiger_bot_api_url.clone().unwrap_or_default();
    let model = settings.tiger_bot_model.clone();

    if api_key.is_empty() {
        let mut status = synth_status().lock().await;
        status.running = false;
        return Err("No API key configured".to_string());
    }

    let api_url = if api_url_raw.ends_with("/chat/completions") {
        api_url_raw
    } else {
        format!("{}/chat/completions", api_url_raw.trim_end_matches('/'))
    };

    // Get cursor (last processed timestamp) — ignored when force=true
    let cursor = if force {
        String::new()
    } else {
        settings
            .extra
            .get("skillAutoUpdateCursor")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    let require_approval = settings
        .skill_auto_update_require_approval
        .unwrap_or(true);

    // Load chat sessions
    let sessions = get_chat_history().await;
    info!("[SkillSynth] Total sessions in history: {}", sessions.len());

    // Filter sessions newer than cursor
    let recent_sessions: Vec<&ChatSession> = sessions
        .iter()
        .filter(|s| {
            if cursor.is_empty() {
                true
            } else {
                s.updated_at.as_str() > cursor.as_str()
            }
        })
        .take(30)
        .collect();

    info!("[SkillSynth] Sessions after cursor filter: {}", recent_sessions.len());

    if recent_sessions.is_empty() {
        let mut status = synth_status().lock().await;
        status.running = false;
        status.last_run_at = Some(chrono::Utc::now().to_rfc3339());
        status.last_run_summary = Some("No new sessions to analyze".to_string());
        return Ok("No new sessions to analyze".to_string());
    }

    // Summarize sessions
    let summaries: Vec<String> = recent_sessions
        .iter()
        .filter_map(|s| summarise_session(s))
        .collect();

    if summaries.is_empty() {
        let mut status = synth_status().lock().await;
        status.running = false;
        status.last_run_at = Some(chrono::Utc::now().to_rfc3339());
        status.last_run_summary =
            Some("No eligible sessions (all too short or errors)".to_string());
        return Ok("No eligible sessions".to_string());
    }

    // Load existing skills
    let existing_skills = list_existing_skills().await;

    info!("[SkillSynth] {} eligible sessions, {} existing skills. Calling LLM (model={})...", summaries.len(), existing_skills.len(), model);

    // Call LLM
    let proposals = match call_llm_for_skills(&api_key, &api_url, &model, &summaries, &existing_skills)
        .await {
            Ok(p) => p,
            Err(e) => {
                error!("[SkillSynth] LLM call failed: {}", e);
                let mut status = synth_status().lock().await;
                status.running = false;
                status.last_run_at = Some(chrono::Utc::now().to_rfc3339());
                status.last_run_summary = Some(format!("Error: {}", e));
                return Err(e);
            }
        };

    let mut created = 0;
    let mut updated = 0;

    for proposal in &proposals {
        let kind = proposal["kind"].as_str().unwrap_or("create");
        let name = proposal["name"].as_str().unwrap_or("").to_string();
        let description = proposal["description"].as_str().unwrap_or("").to_string();
        let content = proposal["content"].as_str().unwrap_or("").to_string();
        let rationale = proposal["rationale"].as_str().unwrap_or("").to_string();

        if name.is_empty() || content.is_empty() {
            warn!("[SkillSynth] Skipping proposal with empty name or content. name='{}', content_len={}", name, content.len());
            continue;
        }

        // Validate name
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            warn!("[SkillSynth] Invalid name: {}", name);
            continue;
        }

        let is_update = kind == "update";

        // Get existing content for diff
        let existing_content = if is_update {
            let skill_path = skills_dir().join(&name).join("SKILL.md");
            tokio::fs::read_to_string(&skill_path).await.ok()
        } else {
            None
        };

        // Write the skill file
        if require_approval {
            if let Err(e) = write_skill(&name, &content, true).await {
                error!("[SkillSynth] Failed to write proposed skill: {e}");
                continue;
            }
        } else {
            if let Err(e) = write_skill(&name, &content, false).await {
                error!("[SkillSynth] Failed to write skill: {e}");
                continue;
            }
        }

        let session_ids: Vec<String> = recent_sessions
            .iter()
            .take(5)
            .map(|s| s.id.clone())
            .collect();

        let skill_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let review_status = if require_approval {
            "pending".to_string()
        } else {
            "approved".to_string()
        };

        // Register in skills.json
        let mut skills = get_skills().await;
        // Remove any existing entry with same name (for updates)
        skills.retain(|s| s.name != name);
        skills.push(Skill {
            id: skill_id.clone(),
            name: name.clone(),
            description: description.clone(),
            source: "auto".to_string(),
            script: name.clone(), // folder name
            enabled: !require_approval, // enabled immediately if no approval needed
            installed_at: now.clone(),
            review_status: Some(review_status.clone()),
            auto_meta: Some(SkillAutoMeta {
                kind: kind.to_string(),
                based_on: session_ids.clone(),
                generated_at: now.clone(),
                model: model.clone(),
                proposed_path: if require_approval {
                    Some("SKILL.md.proposed".to_string())
                } else {
                    None
                },
                rationale: Some(rationale.clone()),
            }),
        });
        save_skills(&skills).await;
        info!("[SkillSynth] Registered skill '{}' in skills.json (status={})", name, review_status);

        let skill_proposal = SkillProposal {
            id: skill_id,
            name: name.clone(),
            description,
            kind: kind.to_string(),
            content,
            rationale,
            based_on: session_ids,
            generated_at: now,
            model: model.clone(),
            review_status,
            existing_content,
        };

        {
            let mut status = synth_status().lock().await;
            status.proposals.push(skill_proposal);
        }

        if is_update {
            updated += 1;
        } else {
            created += 1;
        }
    }

    // Update cursor to latest session timestamp
    if let Some(latest) = recent_sessions
        .iter()
        .map(|s| s.updated_at.as_str())
        .max()
    {
        let mut settings = get_settings().await;
        settings
            .extra
            .insert("skillAutoUpdateCursor".to_string(), json!(latest));
        settings.extra.insert(
            "skillAutoUpdateLastRunAt".to_string(),
            json!(chrono::Utc::now().to_rfc3339()),
        );
        let summary = format!(
            "Analyzed {} sessions, created {} skills, updated {}",
            summaries.len(),
            created,
            updated
        );
        settings.extra.insert(
            "skillAutoUpdateLastRunSummary".to_string(),
            json!(&summary),
        );
        save_settings(&settings).await;
    }

    let summary = format!(
        "Created {} skill(s), updated {} skill(s) from {} session(s)",
        created,
        updated,
        summaries.len()
    );

    {
        let mut status = synth_status().lock().await;
        status.running = false;
        status.last_run_at = Some(chrono::Utc::now().to_rfc3339());
        status.last_run_summary = Some(summary.clone());
    }

    info!("[SkillSynth] {}", summary);
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Approve / Reject
// ---------------------------------------------------------------------------

pub async fn approve_proposal(proposal_id: &str) -> Result<(), String> {
    let proposal_name;
    let proposal_content;

    {
        let mut status = synth_status().lock().await;
        let proposal = status
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| "Proposal not found".to_string())?;

        proposal.review_status = "approved".to_string();
        proposal_name = proposal.name.clone();
        proposal_content = proposal.content.clone();
    }

    // Move SKILL.md.proposed -> SKILL.md
    let proposed_path = skills_dir().join(&proposal_name).join("SKILL.md.proposed");
    let final_path = skills_dir().join(&proposal_name).join("SKILL.md");

    if proposed_path.exists() {
        tokio::fs::rename(&proposed_path, &final_path)
            .await
            .map_err(|e| format!("Failed to approve skill: {e}"))?;
    } else {
        write_skill(&proposal_name, &proposal_content, false).await?;
    }

    // Enable the skill in skills.json
    let mut skills = get_skills().await;
    if let Some(skill) = skills.iter_mut().find(|s| s.name == proposal_name) {
        skill.enabled = true;
        skill.review_status = Some("approved".to_string());
        if let Some(ref mut meta) = skill.auto_meta {
            meta.proposed_path = None;
        }
    }
    save_skills(&skills).await;

    info!("[SkillSynth] Approved skill: {}", proposal_name);
    Ok(())
}

pub async fn reject_proposal(proposal_id: &str) -> Result<(), String> {
    let proposal_name;

    {
        let mut status = synth_status().lock().await;
        let proposal = status
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| "Proposal not found".to_string())?;

        proposal.review_status = "rejected".to_string();
        proposal_name = proposal.name.clone();
    }

    // Remove SKILL.md.proposed
    let proposed_path = skills_dir().join(&proposal_name).join("SKILL.md.proposed");
    let _ = tokio::fs::remove_file(&proposed_path).await;

    // If it was a new skill with no SKILL.md, remove the whole dir
    let skill_md = skills_dir().join(&proposal_name).join("SKILL.md");
    if !skill_md.exists() {
        let dir = skills_dir().join(&proposal_name);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // Remove from skills.json
    let mut skills = get_skills().await;
    skills.retain(|s| s.name != proposal_name);
    save_skills(&skills).await;

    info!("[SkillSynth] Rejected skill: {}", proposal_name);
    Ok(())
}

// ---------------------------------------------------------------------------
// Background cron
// ---------------------------------------------------------------------------

pub fn start_cron(runtime: tokio::runtime::Handle) {
    runtime.spawn(async move {
        // Wait 30 seconds before first run
        tokio::time::sleep(Duration::from_secs(30)).await;

        loop {
            let settings = get_settings().await;
            let enabled = settings.skill_auto_update_enabled.unwrap_or(true);
            let interval_mins = settings
                .skill_auto_update_interval_minutes
                .unwrap_or(60)
                .max(1);

            if enabled {
                info!("[SkillSynth] Cron triggered");
                match run_synthesis().await {
                    Ok(summary) => info!("[SkillSynth] Cron result: {}", summary),
                    Err(e) => error!("[SkillSynth] Cron error: {}", e),
                }
            }

            // Sleep for the configured interval
            tokio::time::sleep(Duration::from_secs(interval_mins * 60)).await;
        }
    });
}
