//! VM configuration — paths, constants, and helpers.
//!
//! Rust equivalent of the Swift `VMConfig` struct.  All VM artefacts live under
//! `<data_dir>/AndrewOS/` where `data_dir` is `~/Library/Application Support`
//! on macOS (provided by [`dirs::data_dir`]).  A custom storage path can be
//! persisted in `settings.json` inside that directory.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Settings file (JSON stored at <app_support_dir>/settings.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Settings {
    /// Optional override for the VM storage directory.
    vm_storage_path: Option<String>,
}

// ---------------------------------------------------------------------------
// VmConfig
// ---------------------------------------------------------------------------

/// Central configuration for the AndrewOS virtual machine.
///
/// Every method is an associated function (no `&self`) so callers can use
/// `VmConfig::raw_disk_path()` etc. without instantiation — mirrors the Swift
/// `static` approach.
pub struct VmConfig;

impl VmConfig {
    // -----------------------------------------------------------------------
    // Directories
    // -----------------------------------------------------------------------

    /// Default Application Support directory: `<data_dir>/AndrewOS/`.
    ///
    /// Delegates to [`crate::server::data::app_root_dir`] rather than joining
    /// the product name itself. That function also owns the pre-rebrand
    /// (`TigrimOS`) fallback, so an upgraded install keeps its already-
    /// provisioned disk image, seed and `.provisioned` marker instead of
    /// finding an empty directory and re-downloading ~700MB.
    pub fn default_app_support_dir() -> PathBuf {
        crate::server::data::app_root_dir()
    }

    /// Effective storage root.  Returns the custom path from `settings.json`
    /// when present, otherwise falls back to [`Self::default_app_support_dir`].
    pub fn app_support_dir() -> PathBuf {
        if let Ok(data) = fs::read_to_string(Self::settings_path()) {
            if let Ok(settings) = serde_json::from_str::<Settings>(&data) {
                if let Some(ref custom) = settings.vm_storage_path {
                    if !custom.is_empty() {
                        return PathBuf::from(custom);
                    }
                }
            }
        }
        Self::default_app_support_dir()
    }

    /// Persist (or clear) a custom storage path.
    #[allow(dead_code)]
    pub fn set_storage_path(path: Option<&str>) {
        let settings = Settings {
            vm_storage_path: path.map(|s| s.to_string()),
        };
        if let Ok(json) = serde_json::to_string_pretty(&settings) {
            let _ = fs::create_dir_all(Self::default_app_support_dir());
            let _ = fs::write(Self::settings_path(), json);
        }
    }

    /// Path to the settings file itself.
    fn settings_path() -> PathBuf {
        Self::default_app_support_dir().join("settings.json")
    }

    // -----------------------------------------------------------------------
    // VM file paths (all relative to app_support_dir)
    // -----------------------------------------------------------------------

    /// Raw disk image converted from the Ubuntu QCOW2 cloud image.
    pub fn raw_disk_path() -> PathBuf {
        Self::app_support_dir().join("ubuntu-raw.img")
    }

    /// Cached Ubuntu QCOW2 cloud image (downloaded once).
    pub fn cloud_image_path() -> PathBuf {
        Self::app_support_dir().join("ubuntu-cloud.qcow2")
    }

    /// Linux kernel (vmlinuz) — used on Intel for direct kernel boot.
    pub fn kernel_path() -> PathBuf {
        Self::app_support_dir().join("vmlinuz")
    }

    /// Initial ramdisk — used on Intel for direct kernel boot.
    pub fn initrd_path() -> PathBuf {
        Self::app_support_dir().join("initrd")
    }

    /// Cloud-init seed image (FAT12 with meta-data, user-data, network-config).
    pub fn seed_iso_path() -> PathBuf {
        Self::app_support_dir().join("seed.img")
    }

    /// EFI variable store (kept for compatibility with UEFI boot path).
    pub fn efi_store_path() -> PathBuf {
        Self::app_support_dir().join("efi_vars.fd")
    }

    /// Machine identifier (binary blob).
    pub fn machine_id_path() -> PathBuf {
        Self::app_support_dir().join("machine_id.bin")
    }

    /// Default shared folder on the host — `~/AndrewOS_Shared/`.
    ///
    /// Keeps using the pre-rebrand `~/TigrimOS_Shared` when that is the folder
    /// the user already has files in. This is a host folder the user put real
    /// work into; silently pointing the VM at a new empty one would look like
    /// data loss.
    pub fn default_shared_dir() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let current = home.join("AndrewOS_Shared");
        if current.exists() {
            return current;
        }
        let legacy = home.join("TigrimOS_Shared");
        if legacy.exists() {
            return legacy;
        }
        current
    }

    /// Marker file created after the first successful provision.
    pub fn provisioned_marker() -> PathBuf {
        Self::app_support_dir().join(".provisioned")
    }

    // -----------------------------------------------------------------------
    // Constants
    // -----------------------------------------------------------------------
    #[allow(dead_code)]

    /// Number of virtual CPUs: min(4, host_logical_cores).
    pub fn cpu_count() -> usize {
        std::cmp::min(4, num_cpus::get())
    }

    /// Guest RAM in GiB.
    pub const MEMORY_GB: u64 = 4;

    /// Guest RAM in bytes.
    #[allow(dead_code)]
    pub const MEMORY_SIZE_BYTES: u64 = Self::MEMORY_GB * 1024 * 1024 * 1024;

    /// Virtual disk size in GiB.
    pub const DISK_SIZE_GB: u64 = 5;

    /// Virtual disk size in bytes.
    #[allow(dead_code)]
    pub const DISK_SIZE_BYTES: u64 = Self::DISK_SIZE_GB * 1024 * 1024 * 1024;

    /// Port the AndrewOS service listens on inside the guest.
    pub const VM_PORT: u16 = 3001;

    /// Port forwarded to the host via QEMU user-mode networking.
    /// Must differ from the app's own server port (3001) to avoid false health checks.
    pub const HOST_FORWARD_PORT: u16 = 3002;

    /// SSH port forwarded from host to guest port 22.
    pub const SSH_HOST_PORT: u16 = 2222;

    // -----------------------------------------------------------------------
    // Utilities
    // -----------------------------------------------------------------------

    /// Returns `true` when the VM has been fully provisioned at least once.
    pub fn is_provisioned() -> bool {
        Self::provisioned_marker().exists()
    }

    /// Human-readable size of the raw disk image, e.g. `"4.2 GB"`.
    pub fn disk_usage() -> String {
        match fs::metadata(Self::raw_disk_path()) {
            Ok(meta) => {
                let gb = meta.len() as f64 / (1024.0 * 1024.0 * 1024.0);
                format!("{:.1} GB", gb)
            }
            Err(_) => "Not created".to_string(),
        }
    }

    /// Create the app-support and default-shared directories if they do not
    /// already exist.
    pub fn ensure_directories() -> std::io::Result<()> {
        fs::create_dir_all(Self::app_support_dir())?;
        fs::create_dir_all(Self::default_shared_dir())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_count_bounded() {
        let cpus = VmConfig::cpu_count();
        assert!(cpus >= 1 && cpus <= 4);
    }

    #[test]
    fn paths_under_app_support() {
        let base = VmConfig::app_support_dir();
        assert!(VmConfig::raw_disk_path().starts_with(&base));
        assert!(VmConfig::cloud_image_path().starts_with(&base));
        assert!(VmConfig::seed_iso_path().starts_with(&base));
        assert!(VmConfig::provisioned_marker().starts_with(&base));
    }

    #[test]
    fn disk_usage_not_created() {
        // Unless someone actually provisioned a VM in the test env, this
        // should return the fallback string.
        let usage = VmConfig::disk_usage();
        assert!(usage == "Not created" || usage.contains("GB"));
    }
}
