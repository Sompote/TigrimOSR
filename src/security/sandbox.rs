use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// PathValidationError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum PathValidationError {
    #[error("resolved path is outside the allowed root directory")]
    OutsideRoot,

    #[error("symlink at path escapes the root directory")]
    SymlinkEscape,
}

// ---------------------------------------------------------------------------
// SandboxManager - persistent bookmark storage (JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Default)]
struct BookmarkStore {
    bookmarks: HashMap<String, PathBuf>,
}

#[derive(Debug)]
pub struct SandboxManager {
    bookmarks_path: PathBuf,
}

impl SandboxManager {
    /// Create a new `SandboxManager` that persists bookmarks inside
    /// `app_support_dir` (e.g. `~/Library/Application Support/AndrewOS`).
    pub fn new(app_support_dir: &Path) -> Self {
        let bookmarks_path = app_support_dir.join("sandbox_bookmarks.json");
        Self { bookmarks_path }
    }

    // -- public API ---------------------------------------------------------

    /// Persist a bookmark for the given path.
    pub fn save_bookmark(&self, path: &str) {
        let mut store = self.load_store();
        let resolved = PathBuf::from(path);
        store.bookmarks.insert(path.to_string(), resolved);
        self.write_store(&store);
    }

    /// Resolve a previously-saved bookmark, returning its `PathBuf` if it
    /// exists on disk and is still bookmarked.
    pub fn resolve_bookmark(&self, path: &str) -> Option<PathBuf> {
        let store = self.load_store();
        store
            .bookmarks
            .get(path)
            .and_then(|p| if p.exists() { Some(p.clone()) } else { None })
    }

    /// Return every bookmarked path (equivalent of macOS
    /// `startAccessingSecurityScopedResource` for all bookmarks).
    pub fn start_accessing_all_resources(&self) -> Vec<PathBuf> {
        let store = self.load_store();
        store
            .bookmarks
            .values()
            .filter(|p| p.exists())
            .cloned()
            .collect()
    }

    /// Remove the bookmark associated with the given path string.
    pub fn remove_bookmark(&self, path: &str) {
        let mut store = self.load_store();
        store.bookmarks.remove(path);
        self.write_store(&store);
    }

    /// Load all bookmarks from the JSON file and return them as a `HashMap`.
    pub fn load_bookmarks(&self) -> HashMap<String, PathBuf> {
        self.load_store().bookmarks
    }

    // -- internal helpers ---------------------------------------------------

    fn load_store(&self) -> BookmarkStore {
        match fs::read_to_string(&self.bookmarks_path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => BookmarkStore::default(),
        }
    }

    fn write_store(&self, store: &BookmarkStore) {
        if let Some(parent) = self.bookmarks_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(store).expect("failed to serialize bookmarks");
        fs::write(&self.bookmarks_path, json).expect("failed to write bookmarks file");
    }
}

// ---------------------------------------------------------------------------
// PathValidator - ensure paths stay within an allowed root
// ---------------------------------------------------------------------------

pub struct PathValidator;

impl PathValidator {
    /// Validate that `path` resolves to a location inside `root`.
    ///
    /// Returns the canonicalized path string on success, or a
    /// `PathValidationError` if the path escapes the root.
    pub fn validate(path: &str, root: &str) -> Result<String, PathValidationError> {
        let root_canonical =
            fs::canonicalize(root).map_err(|_| PathValidationError::OutsideRoot)?;
        let target = Path::new(path);

        // Check every component along the way for symlinks that escape root.
        let mut accumulated = PathBuf::new();
        for component in target.components() {
            accumulated.push(component);
            if accumulated.exists() {
                // If any component is a symlink, resolve it and verify it
                // stays within root.
                if let Ok(link_target) = fs::read_link(&accumulated) {
                    let resolved = if link_target.is_absolute() {
                        link_target
                    } else {
                        accumulated
                            .parent()
                            .unwrap_or_else(|| Path::new("/"))
                            .join(&link_target)
                    };

                    let resolved_canonical = fs::canonicalize(&resolved)
                        .map_err(|_| PathValidationError::SymlinkEscape)?;

                    if !resolved_canonical.starts_with(&root_canonical) {
                        return Err(PathValidationError::SymlinkEscape);
                    }
                }
            }
        }

        // Final canonicalization of the full path.
        let full_canonical =
            fs::canonicalize(path).map_err(|_| PathValidationError::OutsideRoot)?;

        if full_canonical.starts_with(&root_canonical) {
            Ok(full_canonical.to_string_lossy().into_owned())
        } else {
            Err(PathValidationError::OutsideRoot)
        }
    }
}
