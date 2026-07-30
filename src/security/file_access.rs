use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Permission
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    ReadOnly,
    ReadWrite,
}

// ---------------------------------------------------------------------------
// AccessEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessEntry {
    pub id: Uuid,
    pub path: String,
    pub permission: Permission,
    pub granted_at: DateTime<Utc>,
    pub revoked: bool,
}

// ---------------------------------------------------------------------------
// AuditLogEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub path: String,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// FileAccessControl
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct FileAccessControl {
    pub access_entries: Vec<AccessEntry>,
    pub audit_log: Vec<AuditLogEntry>,
    pub config_path: PathBuf,
    pub audit_path: PathBuf,
}

/// Maximum number of audit log entries to keep on disk.
const MAX_AUDIT_ENTRIES: usize = 1000;

impl FileAccessControl {
    /// Create a new `FileAccessControl`, loading any previously persisted
    /// state from JSON files inside `app_support_dir`.
    pub fn new(app_support_dir: &Path) -> Self {
        let config_path = app_support_dir.join("file_access_entries.json");
        let audit_path = app_support_dir.join("file_access_audit.json");

        let mut ctrl = Self {
            access_entries: Vec::new(),
            audit_log: Vec::new(),
            config_path,
            audit_path,
        };

        ctrl.load();
        ctrl
    }

    // -- Access management --------------------------------------------------

    /// Grant access to `path` with the given `permission`.  A new
    /// `AccessEntry` is created, persisted, and an audit record is written.
    pub fn grant_access(&mut self, path: &str, permission: Permission) {
        let entry = AccessEntry {
            id: Uuid::new_v4(),
            path: path.to_string(),
            permission,
            granted_at: Utc::now(),
            revoked: false,
        };

        self.log_audit(
            "grant_access",
            path,
            &format!("Granted {:?} access", permission),
        );

        self.access_entries.push(entry);
        self.save();
    }

    /// Revoke a previously granted access entry identified by its `id`.
    pub fn revoke_access(&mut self, id: Uuid) {
        let path = self
            .access_entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.path.clone());
        if let Some(path) = path {
            if let Some(entry) = self.access_entries.iter_mut().find(|e| e.id == id) {
                entry.revoked = true;
            }
            self.log_audit(
                "revoke_access",
                &path,
                &format!("Revoked access for entry {}", id),
            );
            self.save();
        }
    }

    /// Check whether an active (non-revoked) grant exists for `path` and
    /// return its `Permission` level if so.
    pub fn has_access(&self, path: &str) -> Option<Permission> {
        self.access_entries
            .iter()
            .rev()
            .find(|e| !e.revoked && e.path == path)
            .map(|e| e.permission)
    }

    /// Convenience helper: returns `true` when `path` has `ReadWrite` access.
    pub fn can_write(&self, path: &str) -> bool {
        self.has_access(path) == Some(Permission::ReadWrite)
    }

    // -- Persistence --------------------------------------------------------

    /// Save access entries and audit log to their respective JSON files.
    pub fn save(&self) {
        Self::ensure_parent(&self.config_path);
        Self::ensure_parent(&self.audit_path);

        let entries_json = serde_json::to_string_pretty(&self.access_entries)
            .expect("failed to serialize access entries");
        fs::write(&self.config_path, entries_json).expect("failed to write access entries file");

        let audit_json =
            serde_json::to_string_pretty(&self.audit_log).expect("failed to serialize audit log");
        fs::write(&self.audit_path, audit_json).expect("failed to write audit log file");
    }

    /// Load access entries and audit log from their JSON files.  If the files
    /// do not exist or cannot be parsed the collections remain empty.
    pub fn load(&mut self) {
        if let Ok(data) = fs::read_to_string(&self.config_path) {
            if let Ok(entries) = serde_json::from_str::<Vec<AccessEntry>>(&data) {
                self.access_entries = entries;
            }
        }

        if let Ok(data) = fs::read_to_string(&self.audit_path) {
            if let Ok(log) = serde_json::from_str::<Vec<AuditLogEntry>>(&data) {
                self.audit_log = log;
            }
        }
    }

    // -- Audit --------------------------------------------------------------

    /// Append an audit record.  If the log exceeds `MAX_AUDIT_ENTRIES` the
    /// oldest entries are trimmed so that only the most recent 1000 remain.
    pub fn log_audit(&mut self, action: &str, path: &str, detail: &str) {
        let entry = AuditLogEntry {
            timestamp: Utc::now(),
            action: action.to_string(),
            path: path.to_string(),
            detail: detail.to_string(),
        };
        self.audit_log.push(entry);

        // Keep only the last MAX_AUDIT_ENTRIES entries.
        if self.audit_log.len() > MAX_AUDIT_ENTRIES {
            let excess = self.audit_log.len() - MAX_AUDIT_ENTRIES;
            self.audit_log.drain(..excess);
        }
    }

    // -- Helpers ------------------------------------------------------------

    fn ensure_parent(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
    }
}
