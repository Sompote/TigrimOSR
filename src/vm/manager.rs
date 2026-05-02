//! VM lifecycle manager — download, provision, start, stop, health-check.
//!
//! Rust equivalent of the Swift `VMManager` class.  Instead of Apple
//! Virtualization.framework the VM is driven by a QEMU child process
//! (`qemu-system-aarch64` on Apple Silicon, `qemu-system-x86_64` on Intel).

use crate::vm::config::VmConfig;

use chrono::Local;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during VM operations.
#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum TigrimOSError {
    #[error("Download failed: {0}")]
    DownloadFailed(String),

    #[error("Failed to create console pipe")]
    PipeCreationFailed,

    #[error("VM is not running")]
    VmNotRunning,

    #[error("Provisioning failed: {0}")]
    ProvisioningFailed(String),

    #[error("QEMU not found: {0}")]
    QemuNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

// ---------------------------------------------------------------------------
// VmState
// ---------------------------------------------------------------------------

/// Observable VM lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    Stopped,
    Downloading,
    Converting,
    Provisioning,
    Starting,
    Running,
    Stopping,
    Error,
}

impl std::fmt::Display for VmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => write!(f, "Stopped"),
            Self::Downloading => write!(f, "Downloading VM Image..."),
            Self::Converting => write!(f, "Converting disk image..."),
            Self::Provisioning => write!(f, "Setting up Ubuntu..."),
            Self::Starting => write!(f, "Starting VM..."),
            Self::Running => write!(f, "Running"),
            Self::Stopping => write!(f, "Stopping..."),
            Self::Error => write!(f, "Error"),
        }
    }
}

impl VmState {
    /// Short human-readable label (same as Display).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Downloading => "Downloading VM Image...",
            Self::Converting => "Converting disk image...",
            Self::Provisioning => "Setting up Ubuntu...",
            Self::Starting => "Starting VM...",
            Self::Running => "Running",
            Self::Stopping => "Stopping...",
            Self::Error => "Error",
        }
    }
}

// ---------------------------------------------------------------------------
// SharedFolderEntry
// ---------------------------------------------------------------------------

/// A user-configured shared folder exposed to the guest via QEMU 9p.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedFolderEntry {
    pub id: Uuid,
    pub tag: String,
    #[serde(with = "path_serde")]
    pub path: PathBuf,
    pub read_only: bool,
    pub name: String,
}

/// Custom serde helpers for PathBuf (serialize as plain string).
mod path_serde {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use std::path::PathBuf;

    pub fn serialize<S>(path: &PathBuf, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&path.to_string_lossy())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(PathBuf::from(s))
    }
}

// ---------------------------------------------------------------------------
// ProcessResult
// ---------------------------------------------------------------------------

/// Captured output of an external command.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProcessResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

// ---------------------------------------------------------------------------
// VmManager (inner mutable state)
// ---------------------------------------------------------------------------

/// All mutable VM state, protected by `Arc<Mutex<…>>`.
#[allow(dead_code)]
struct VmManagerInner {
    state: VmState,
    error_message: Option<String>,
    progress: f64,
    console_output: String,
    shared_folders: Vec<SharedFolderEntry>,
    service_ready: bool,
    vm_ip_address: Option<String>,
    qemu_child: Option<Child>,
    health_check_running: bool,
}

impl VmManagerInner {
    fn new() -> Self {
        Self {
            state: VmState::Stopped,
            error_message: None,
            progress: 0.0,
            console_output: String::new(),
            shared_folders: Vec::new(),
            service_ready: false,
            vm_ip_address: None,
            qemu_child: None,
            health_check_running: false,
        }
    }

    /// Append a timestamped line and keep the buffer at most 500 lines.
    fn append_console(&mut self, text: &str) {
        let ts = Local::now().format("%H:%M:%S").to_string();
        self.console_output
            .push_str(&format!("[{}] {}\n", ts, text));
        let line_count = self.console_output.lines().count();
        if line_count > 500 {
            let keep: String = self
                .console_output
                .lines()
                .skip(line_count - 500)
                .collect::<Vec<_>>()
                .join("\n");
            self.console_output = keep;
            self.console_output.push('\n');
        }
    }
}

// ---------------------------------------------------------------------------
// VmManager (public API)
// ---------------------------------------------------------------------------

/// Manages the full TigrimOS VM lifecycle through QEMU.
///
/// All mutable state is behind `Arc<Mutex<…>>` so the manager can be shared
/// across async tasks (e.g. a health-check loop running concurrently with a
/// UI event loop).
pub struct VmManager {
    inner: Arc<Mutex<VmManagerInner>>,
}

impl VmManager {
    // ===================================================================
    // Construction
    // ===================================================================

    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VmManagerInner::new())),
        }
    }

    // ===================================================================
    // State accessors (clone-friendly snapshots for UI layers)
    // ===================================================================

    pub async fn state(&self) -> VmState {
        self.inner.lock().await.state
    }

    pub async fn error_message(&self) -> Option<String> {
        self.inner.lock().await.error_message.clone()
    }

    pub async fn progress(&self) -> f64 {
        self.inner.lock().await.progress
    }

    pub async fn console_output(&self) -> String {
        self.inner.lock().await.console_output.clone()
    }

    pub async fn shared_folders(&self) -> Vec<SharedFolderEntry> {
        self.inner.lock().await.shared_folders.clone()
    }

    pub async fn service_ready(&self) -> bool {
        self.inner.lock().await.service_ready
    }

    pub async fn vm_ip_address(&self) -> Option<String> {
        self.inner.lock().await.vm_ip_address.clone()
    }

    // ===================================================================
    // Lifecycle: start
    // ===================================================================

    /// Full VM start lifecycle:
    /// 1. Ensure directories
    /// 2. Ensure qemu-img is available
    /// 3. Download & convert Ubuntu cloud image (if needed)
    /// 4. Create cloud-init seed (if needed)
    /// 5. Start QEMU child process
    /// 6. Begin health-check polling
    pub async fn start_vm(&self) -> Result<(), TigrimOSError> {
        // Guard: only start from Stopped or Error.
        {
            let st = self.inner.lock().await.state;
            if st != VmState::Stopped && st != VmState::Error {
                return Ok(());
            }
        }

        // Reset observable state.
        {
            let mut g = self.inner.lock().await;
            g.state = VmState::Starting;
            g.error_message = None;
            g.progress = 0.0;
            g.service_ready = false;
            g.vm_ip_address = None;
        }

        // Wrap the fallible steps so we can transition to Error on failure.
        let result = self.start_vm_inner().await;
        if let Err(ref e) = result {
            let mut g = self.inner.lock().await;
            g.state = VmState::Error;
            g.error_message = Some(e.to_string());
            g.append_console(&format!("[ERROR] {}", e));
        }
        result
    }

    /// The fallible core of `start_vm`.
    async fn start_vm_inner(&self) -> Result<(), TigrimOSError> {
        // Step 1: directories.
        VmConfig::ensure_directories().map_err(TigrimOSError::Io)?;
        self.append_console("[TigrimOS] Directories ready").await;

        // Step 2: qemu-img.
        self.ensure_qemu_img().await?;

        // Step 3: download & convert image.
        if !VmConfig::raw_disk_path().exists() {
            self.inner.lock().await.state = VmState::Downloading;
            self.download_and_prepare_image().await?;
        }

        // Step 4: cloud-init seed.
        if !VmConfig::seed_iso_path().exists() {
            self.append_console("[TigrimOS] Creating cloud-init seed...")
                .await;
            self.create_cloud_init_seed().await?;
        }

        // Step 5: start QEMU.
        self.inner.lock().await.state = VmState::Starting;
        self.append_console("[TigrimOS] Starting QEMU virtual machine...")
            .await;
        self.launch_qemu().await?;

        // Mark running (or provisioning on first boot).
        {
            let mut g = self.inner.lock().await;
            if VmConfig::is_provisioned() {
                g.state = VmState::Running;
                g.append_console("[TigrimOS] VM started successfully");
            } else {
                g.state = VmState::Provisioning;
                g.append_console(
                    "[TigrimOS] First run -- provisioning via cloud-init (this takes several minutes)...",
                );
            }
        }

        // Step 6: start health-check polling.
        self.start_health_check().await;

        Ok(())
    }

    // ===================================================================
    // Lifecycle: stop
    // ===================================================================

    /// Kill the QEMU child process and reset observable state.
    pub async fn stop_vm(&self) {
        {
            let st = self.inner.lock().await.state;
            if st != VmState::Running && st != VmState::Provisioning {
                return;
            }
        }

        {
            let mut g = self.inner.lock().await;
            g.state = VmState::Stopping;
            g.health_check_running = false;
            g.service_ready = false;
        }

        // Kill QEMU.
        {
            let mut g = self.inner.lock().await;
            if let Some(ref mut child) = g.qemu_child {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            g.qemu_child = None;
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        {
            let mut g = self.inner.lock().await;
            g.vm_ip_address = None;
            g.state = VmState::Stopped;
            g.append_console("[TigrimOS] VM stopped");
        }
    }

    // ===================================================================
    // Lifecycle: reset
    // ===================================================================

    /// Stop the VM and delete all artefacts so next start re-provisions.
    pub async fn reset_vm(&self) {
        self.stop_vm().await;

        let paths = [
            VmConfig::provisioned_marker(),
            VmConfig::raw_disk_path(),
            VmConfig::seed_iso_path(),
            VmConfig::efi_store_path(),
            VmConfig::machine_id_path(),
            VmConfig::kernel_path(),
            VmConfig::initrd_path(),
            VmConfig::cloud_image_path(),
        ];
        for p in &paths {
            let _ = std::fs::remove_file(p);
        }

        self.append_console(
            "[TigrimOS] VM reset -- will re-download and provision on next start",
        )
        .await;
    }

    // ===================================================================
    // QEMU binary helpers
    // ===================================================================

    /// Locate `qemu-img` on disk.  Checks Homebrew paths first, then `which`.
    pub fn find_qemu_img() -> Option<String> {
        let candidates = [
            "/usr/local/bin/qemu-img",
            "/opt/homebrew/bin/qemu-img",
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                return Some(path.to_string());
            }
        }
        // Fallback: `which qemu-img`.
        if let Ok(output) = std::process::Command::new("which")
            .arg("qemu-img")
            .output()
        {
            let p = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !p.is_empty() && std::path::Path::new(&p).exists() {
                return Some(p);
            }
        }
        None
    }

    /// Locate the correct `qemu-system-*` binary for the host architecture.
    fn find_qemu_system() -> Option<String> {
        let binary = if cfg!(target_arch = "aarch64") {
            "qemu-system-aarch64"
        } else {
            "qemu-system-x86_64"
        };
        let candidates = [
            format!("/usr/local/bin/{}", binary),
            format!("/opt/homebrew/bin/{}", binary),
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                return Some(path.clone());
            }
        }
        if let Ok(output) = std::process::Command::new("which").arg(binary).output() {
            let p = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !p.is_empty() && std::path::Path::new(&p).exists() {
                return Some(p);
            }
        }
        None
    }

    /// Ensure `qemu-img` is available; install via Homebrew if missing.
    async fn ensure_qemu_img(&self) -> Result<(), TigrimOSError> {
        if let Some(path) = Self::find_qemu_img() {
            self.append_console(&format!("[TigrimOS] Found qemu-img at {}", path))
                .await;
            return Ok(());
        }

        self.append_console("[TigrimOS] qemu-img not found -- installing via Homebrew...")
            .await;

        let result =
            Self::run_process("/bin/bash", &["-l", "-c", "brew install qemu 2>&1"]).await?;

        if result.exit_code != 0 {
            return Err(TigrimOSError::ProvisioningFailed(format!(
                "qemu-img is required to convert Ubuntu cloud images.\n\
                 Install manually in Terminal: brew install qemu\n\
                 Output: {}{}",
                result.stderr, result.stdout
            )));
        }

        if Self::find_qemu_img().is_none() {
            return Err(TigrimOSError::ProvisioningFailed(
                "brew install succeeded but qemu-img not found.\n\
                 Try running in Terminal: brew install qemu"
                    .to_string(),
            ));
        }

        self.append_console("[TigrimOS] qemu installed successfully")
            .await;
        Ok(())
    }

    // ===================================================================
    // Image management
    // ===================================================================

    /// Download Ubuntu QCOW2 cloud image and convert to raw with qemu-img.
    async fn download_and_prepare_image(&self) -> Result<(), TigrimOSError> {
        let (arch_label, cloud_img_url) = if cfg!(target_arch = "aarch64") {
            ("arm64", "https://cloud-images.ubuntu.com/releases/22.04/release/ubuntu-22.04-server-cloudimg-arm64.img")
        } else {
            ("amd64", "https://cloud-images.ubuntu.com/releases/22.04/release/ubuntu-22.04-server-cloudimg-amd64.img")
        };

        let qcow2_path = VmConfig::cloud_image_path();

        // Download if not already cached.
        if !qcow2_path.exists() {
            self.append_console(&format!(
                "[TigrimOS] Downloading Ubuntu Server 22.04 ({})...",
                arch_label
            ))
            .await;
            self.append_console("[TigrimOS] This is ~700MB, please wait...")
                .await;
            self.inner.lock().await.progress = 0.0;

            let response = reqwest::get(cloud_img_url).await?;
            if !response.status().is_success() {
                return Err(TigrimOSError::DownloadFailed(format!(
                    "HTTP {} downloading Ubuntu image",
                    response.status()
                )));
            }
            let bytes = response.bytes().await?;
            tokio::fs::write(&qcow2_path, &bytes).await?;

            self.inner.lock().await.progress = 0.5;
            self.append_console("[TigrimOS] Download complete").await;
        } else {
            self.inner.lock().await.progress = 0.5;
            self.append_console("[TigrimOS] Using cached Ubuntu cloud image")
                .await;
        }

        // Convert QCOW2 -> raw.
        self.inner.lock().await.state = VmState::Converting;
        self.append_console(&format!(
            "[TigrimOS] Converting QCOW2 -> raw format ({}GB)...",
            VmConfig::DISK_SIZE_GB
        ))
        .await;

        let qemu_img = Self::find_qemu_img()
            .ok_or_else(|| TigrimOSError::QemuNotFound("qemu-img not found".to_string()))?;

        let _ = std::fs::remove_file(VmConfig::raw_disk_path());

        // Resize the QCOW2 to target size.
        let resize = Self::run_process(
            &qemu_img,
            &[
                "resize",
                &qcow2_path.to_string_lossy(),
                &format!("{}G", VmConfig::DISK_SIZE_GB),
            ],
        )
        .await?;
        if resize.exit_code != 0 {
            self.append_console(&format!("[WARN] QCOW2 resize: {}", resize.stderr))
                .await;
        }
        self.inner.lock().await.progress = 0.65;

        // Convert to raw.
        let raw_path = VmConfig::raw_disk_path();
        let convert = Self::run_process(
            &qemu_img,
            &[
                "convert",
                "-f",
                "qcow2",
                "-O",
                "raw",
                &qcow2_path.to_string_lossy(),
                &raw_path.to_string_lossy(),
            ],
        )
        .await?;
        if convert.exit_code != 0 {
            return Err(TigrimOSError::ProvisioningFailed(format!(
                "qemu-img convert failed: {}",
                convert.stderr
            )));
        }
        self.inner.lock().await.progress = 0.85;

        // Verify.
        let info = Self::run_process(
            &qemu_img,
            &["info", "-f", "raw", &raw_path.to_string_lossy()],
        )
        .await?;
        let first_lines: String = info.stdout.lines().take(3).collect::<Vec<_>>().join(", ");
        self.append_console(&format!("[TigrimOS] Image info: {}", first_lines))
            .await;

        self.inner.lock().await.progress = 1.0;
        self.append_console(&format!(
            "[TigrimOS] Raw disk image ready ({}GB)",
            VmConfig::DISK_SIZE_GB
        ))
        .await;

        Ok(())
    }

    // ===================================================================
    // Cloud-init seed
    // ===================================================================

    /// Create a FAT12 seed image with cloud-init NoCloud data using `hdiutil`.
    ///
    /// Contains: `meta-data`, `user-data` (full provisioning: Node 20,
    /// Python3, TigrimOS service), and `network-config` (DHCP).
    async fn create_cloud_init_seed(&self) -> Result<(), TigrimOSError> {
        let seed_dir = VmConfig::app_support_dir().join("seed");
        tokio::fs::create_dir_all(&seed_dir).await?;

        // ---- meta-data ----
        let instance_id = format!("tigris-vm-{}", chrono::Utc::now().timestamp());
        let meta_data = format!(
            "instance-id: {}\nlocal-hostname: tigris\n",
            instance_id
        );
        tokio::fs::write(seed_dir.join("meta-data"), &meta_data).await?;

        // ---- user-data (matches Swift original) ----
        tokio::fs::write(seed_dir.join("user-data"), CLOUD_INIT_USER_DATA).await?;

        // ---- network-config ----
        let network_config = "\
version: 2
ethernets:
  id0:
    match:
      driver: virtio_net
    dhcp4: true
    dhcp6: false
  fallback:
    match:
      name: en*
    dhcp4: true
";
        tokio::fs::write(seed_dir.join("network-config"), network_config).await?;

        // ---- Build FAT12 image using dd + hdiutil (macOS) ----
        let seed_iso = VmConfig::seed_iso_path();
        let _ = std::fs::remove_file(&seed_iso);
        let iso_str = seed_iso.to_string_lossy().to_string();

        // Step 1: Create empty 1.44 MB raw file.
        let dd = Self::run_process(
            "/bin/dd",
            &["if=/dev/zero", &format!("of={}", iso_str), "bs=512", "count=2880"],
        )
        .await?;
        if dd.exit_code != 0 {
            return Err(TigrimOSError::ProvisioningFailed(format!(
                "Failed to create seed image: {}",
                dd.stderr
            )));
        }

        // Step 2: Attach as disk device (no mount).
        let attach = Self::run_process(
            "/usr/bin/hdiutil",
            &["attach", "-nomount", &iso_str],
        )
        .await?;
        if attach.exit_code != 0 {
            return Err(TigrimOSError::ProvisioningFailed(format!(
                "Failed to attach seed image: {}",
                attach.stderr
            )));
        }

        let device_path = attach
            .stdout
            .trim()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        self.append_console(&format!("[TigrimOS] Seed device: {}", device_path))
            .await;

        if device_path.is_empty() {
            return Err(TigrimOSError::ProvisioningFailed(
                "Could not find device for seed image".to_string(),
            ));
        }

        // Step 3: Format FAT12, volume label "cidata" (required by cloud-init NoCloud).
        let fmt = Self::run_process(
            "/sbin/newfs_msdos",
            &["-F", "12", "-v", "cidata", &device_path],
        )
        .await?;
        if fmt.exit_code != 0 {
            self.append_console(&format!("[WARN] newfs_msdos: {}", fmt.stderr))
                .await;
        }

        // Step 4: Mount, copy files, unmount.
        let mount_point = VmConfig::app_support_dir().join("seed_mount");
        tokio::fs::create_dir_all(&mount_point).await?;
        let mp_str = mount_point.to_string_lossy().to_string();

        let mnt = Self::run_process(
            "/sbin/mount",
            &["-t", "msdos", &device_path, &mp_str],
        )
        .await?;

        if mnt.exit_code == 0 {
            for name in &["meta-data", "user-data", "network-config"] {
                tokio::fs::copy(seed_dir.join(name), mount_point.join(name)).await?;
            }
            let _ = Self::run_process("/sbin/umount", &[&mp_str]).await;
        } else {
            self.append_console(&format!(
                "[WARN] Mount failed: {} -- seed may need manual setup",
                mnt.stderr
            ))
            .await;
        }

        // Detach.
        let _ = Self::run_process("/usr/bin/hdiutil", &["detach", &device_path]).await;

        // Cleanup.
        let _ = tokio::fs::remove_dir_all(&mount_point).await;

        self.append_console("[TigrimOS] Cloud-init seed created (raw FAT12)")
            .await;
        Ok(())
    }

    // ===================================================================
    // QEMU launch
    // ===================================================================

    /// Spawn the QEMU system emulator as a child process and wire up stdout
    /// capture for console output and IP detection.
    async fn launch_qemu(&self) -> Result<(), TigrimOSError> {
        let qemu_bin = Self::find_qemu_system().ok_or_else(|| {
            TigrimOSError::QemuNotFound(
                "qemu-system binary not found. Install via: brew install qemu".to_string(),
            )
        })?;

        let raw_disk = VmConfig::raw_disk_path();
        let seed_iso = VmConfig::seed_iso_path();
        let cpus = VmConfig::cpu_count().to_string();
        let memory = format!("{}G", VmConfig::MEMORY_GB);

        let mut args: Vec<String> = Vec::new();

        // Machine type + accelerator.
        if cfg!(target_arch = "aarch64") {
            args.extend([
                "-machine".into(),
                "virt,highmem=on".into(),
                "-cpu".into(),
                "host".into(),
                "-accel".into(),
                "hvf".into(),
            ]);
        } else {
            args.extend([
                "-machine".into(),
                "q35".into(),
                "-cpu".into(),
                "host".into(),
                "-accel".into(),
                "hvf".into(),
            ]);
        }

        // CPU, memory, display.
        args.extend(["-smp".into(), cpus, "-m".into(), memory, "-nographic".into()]);

        // EFI firmware (arm64 needs EDK2).
        if cfg!(target_arch = "aarch64") {
            let efi_candidates = [
                "/opt/homebrew/share/qemu/edk2-aarch64-code.fd",
                "/usr/local/share/qemu/edk2-aarch64-code.fd",
            ];
            for efi in &efi_candidates {
                if std::path::Path::new(efi).exists() {
                    args.extend(["-bios".into(), efi.to_string()]);
                    break;
                }
            }

            // Persistent EFI variable store.
            let efi_vars = VmConfig::efi_store_path();
            if efi_vars.exists() {
                args.extend([
                    "-drive".into(),
                    format!("file={},if=pflash,format=raw", efi_vars.to_string_lossy()),
                ]);
            }
        }

        // Drives: main disk + cloud-init seed.
        args.extend([
            "-drive".into(),
            format!(
                "file={},format=raw,if=virtio",
                raw_disk.to_string_lossy()
            ),
        ]);
        if seed_iso.exists() {
            args.extend([
                "-drive".into(),
                format!(
                    "file={},format=raw,if=virtio,readonly=on",
                    seed_iso.to_string_lossy()
                ),
            ]);
        }

        // Networking: user-mode NAT with port forwarding.
        args.extend([
            "-netdev".into(),
            format!(
                "user,id=net0,hostfwd=tcp::{}-:{},hostfwd=tcp::{}-:22",
                VmConfig::HOST_FORWARD_PORT,
                VmConfig::VM_PORT,
                VmConfig::SSH_HOST_PORT
            ),
            "-device".into(),
            "virtio-net-pci,netdev=net0".into(),
        ]);

        // Serial console on stdio (for IP detection via TIGRIMOS_IP marker).
        args.extend(["-serial".into(), "stdio".into()]);

        // Shared folders via 9p (VirtioFS requires virtiofsd on macOS).
        {
            let g = self.inner.lock().await;

            // TigerCowork source (read-only).
            if let Some(src_dir) = find_tiger_cowork_source() {
                let src_str = src_dir.to_string_lossy().to_string();
                args.extend([
                    "-fsdev".into(),
                    format!(
                        "local,id=tiger_cowork,path={},security_model=mapped-xattr,readonly=on",
                        src_str
                    ),
                    "-device".into(),
                    "virtio-9p-pci,fsdev=tiger_cowork,mount_tag=tiger-cowork".into(),
                ]);
            }

            // User-configured shared folders.
            for entry in &g.shared_folders {
                let ro = if entry.read_only { ",readonly=on" } else { "" };
                args.extend([
                    "-fsdev".into(),
                    format!(
                        "local,id={id},path={path},security_model=mapped-xattr{ro}",
                        id = entry.tag,
                        path = entry.path.to_string_lossy(),
                        ro = ro,
                    ),
                    "-device".into(),
                    format!(
                        "virtio-9p-pci,fsdev={id},mount_tag={tag}",
                        id = entry.tag,
                        tag = entry.tag,
                    ),
                ]);
            }
        }

        self.append_console(&format!(
            "[TigrimOS] QEMU config: {} CPUs, {}GB RAM",
            VmConfig::cpu_count(),
            VmConfig::MEMORY_GB
        ))
        .await;

        // Spawn child.
        let mut child = Command::new(&qemu_bin)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                TigrimOSError::ProvisioningFailed(format!("Failed to start QEMU: {}", e))
            })?;

        // Capture stdout in background for console output + IP detection.
        if let Some(stdout) = child.stdout.take() {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move {
                let reader = tokio::io::BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    inner.lock().await.append_console(&line);
                }
            });
        }

        // Capture stderr.
        if let Some(stderr) = child.stderr.take() {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move {
                let reader = tokio::io::BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    inner
                        .lock()
                        .await
                        .append_console(&format!("[stderr] {}", line));
                }
            });
        }

        self.inner.lock().await.qemu_child = Some(child);
        Ok(())
    }

    // ===================================================================
    // Health check
    // ===================================================================

    /// Spawn a background task that polls every 5 s for VM IP detection and
    /// TigrimOS service readiness.
    async fn start_health_check(&self) {
        {
            let mut g = self.inner.lock().await;
            if g.health_check_running {
                return;
            }
            g.health_check_running = true;
        }

        let inner = Arc::clone(&self.inner);

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                // Exit conditions.
                {
                    let mut g = inner.lock().await;
                    if !g.health_check_running
                        || (g.state != VmState::Running && g.state != VmState::Provisioning)
                    {
                        g.health_check_running = false;
                        break;
                    }
                }

                // ---- Detect VM IP from console output ----
                {
                    let mut g = inner.lock().await;
                    if g.vm_ip_address.is_none() {
                        if let Some(ip) = detect_vm_ip(&g.console_output) {
                            g.append_console(&format!("[TigrimOS] Detected VM IP: {}", ip));
                            g.vm_ip_address = Some(ip);
                        }
                    }
                }

                // ---- Fallback: ARP table ----
                {
                    let needs_ip = inner.lock().await.vm_ip_address.is_none();
                    if needs_ip {
                        if let Some(ip) = find_vm_via_arp().await {
                            let mut g = inner.lock().await;
                            g.append_console(&format!(
                                "[TigrimOS] Found VM at {} (ARP table)",
                                ip
                            ));
                            g.vm_ip_address = Some(ip);
                        }
                    }
                }

                // With QEMU user-mode networking the service is forwarded to
                // localhost:HOST_FORWARD_PORT, so we always probe that.
                let check_host = {
                    let g = inner.lock().await;
                    g.vm_ip_address
                        .clone()
                        .unwrap_or_else(|| "127.0.0.1".to_string())
                };
                let check_port = VmConfig::HOST_FORWARD_PORT;

                // TCP probe.
                if !try_tcp_connect(&check_host, check_port).await {
                    inner.lock().await.append_console(&format!(
                        "[TigrimOS] Waiting for TigrimOS (port {} not open on {})...",
                        check_port, check_host
                    ));
                    continue;
                }

                // HTTP probe.
                let endpoints = ["/", "/api/auth/verify"];
                let mut ready = false;
                if let Ok(client) = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(3))
                    .build()
                {
                    for ep in &endpoints {
                        let url = format!("http://{}:{}{}", check_host, check_port, ep);
                        let resp = if *ep == "/api/auth/verify" {
                            client
                                .post(&url)
                                .header("Content-Type", "application/json")
                                .body("{}")
                                .send()
                                .await
                        } else {
                            client.get(&url).send().await
                        };
                        if let Ok(r) = resp {
                            let status = r.status().as_u16();
                            if (200..=499).contains(&status) {
                                ready = true;
                                break;
                            }
                        }
                    }
                }

                if ready {
                    let mut g = inner.lock().await;
                    if !g.service_ready {
                        g.service_ready = true;
                        g.state = VmState::Running;
                        if !VmConfig::is_provisioned() {
                            let _ = std::fs::write(VmConfig::provisioned_marker(), b"");
                        }
                        g.append_console(&format!(
                            "[TigrimOS] TigrimOS is ready at http://{}:{}",
                            check_host, check_port
                        ));
                    }
                } else {
                    inner.lock().await.append_console(&format!(
                        "[TigrimOS] Port {} open on {} but service not responding yet...",
                        check_port, check_host
                    ));
                }
            }
        });
    }

    /// Public accessor: parse `TIGRIMOS_IP=<ip>` from console output.
    #[allow(dead_code)]
    pub async fn detect_vm_ip(&self) -> Option<String> {
        let g = self.inner.lock().await;
        detect_vm_ip(&g.console_output)
    }

    // ===================================================================
    // Shared folders
    // ===================================================================

    /// Add a new shared folder entry.
    pub async fn add_shared_folder(&self, path: PathBuf, read_only: bool) {
        let mut g = self.inner.lock().await;
        let idx = g.shared_folders.len();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("shared-{}", idx));
        let entry = SharedFolderEntry {
            id: Uuid::new_v4(),
            tag: format!("shared-{}", idx),
            path: path.clone(),
            read_only,
            name: name.clone(),
        };
        g.shared_folders.push(entry);
        g.append_console(&format!(
            "[TigrimOS] Added shared folder: {} (read-only: {})",
            name, read_only
        ));
        drop(g);
        self.save_shared_folder_config().await;
    }

    /// Remove a shared folder by UUID.
    pub async fn remove_shared_folder(&self, id: Uuid) {
        {
            let mut g = self.inner.lock().await;
            g.shared_folders.retain(|e| e.id != id);
        }
        self.save_shared_folder_config().await;
    }

    /// Toggle `read_only` on a shared folder.
    pub async fn toggle_read_only(&self, id: Uuid) {
        {
            let mut g = self.inner.lock().await;
            let info = g.shared_folders.iter().find(|e| e.id == id).map(|e| (e.name.clone(), !e.read_only));
            if let Some((name, new_val)) = info {
                if let Some(e) = g.shared_folders.iter_mut().find(|e| e.id == id) {
                    e.read_only = new_val;
                }
                g.append_console(&format!("[TigrimOS] {}: read-only = {}", name, new_val));
            }
        }
        self.save_shared_folder_config().await;
    }

    /// Persist shared folder list to JSON.
    pub async fn save_shared_folder_config(&self) {
        let g = self.inner.lock().await;
        let path = VmConfig::app_support_dir().join("shared_folders.json");
        if let Ok(json) = serde_json::to_string_pretty(&g.shared_folders) {
            let _ = tokio::fs::write(&path, json).await;
        }
    }

    /// Load shared folder list from JSON.
    #[allow(dead_code)]
    pub async fn load_shared_folder_config(&self) {
        let path = VmConfig::app_support_dir().join("shared_folders.json");
        if let Ok(data) = tokio::fs::read_to_string(&path).await {
            if let Ok(entries) = serde_json::from_str::<Vec<SharedFolderEntry>>(&data) {
                self.inner.lock().await.shared_folders = entries;
            }
        }
    }

    // ===================================================================
    // Console helper
    // ===================================================================

    /// Append a timestamped line to console output (thread-safe).
    pub async fn clear_console(&self) {
        self.inner.lock().await.console_output.clear();
    }

    pub async fn append_console(&self, text: &str) {
        self.inner.lock().await.append_console(text);
    }

    // ===================================================================
    // Process runner
    // ===================================================================

    /// Run an external command and capture its stdout / stderr.
    pub async fn run_process(path: &str, args: &[&str]) -> Result<ProcessResult, TigrimOSError> {
        let output = Command::new(path)
            .args(args)
            .output()
            .await
            .map_err(TigrimOSError::Io)?;

        Ok(ProcessResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

// ===========================================================================
// Free-standing helpers (used by the health-check background task which
// holds only an Arc<Mutex<VmManagerInner>>)
// ===========================================================================

/// Parse `TIGRIMOS_IP=<ip>` from raw console text.
fn detect_vm_ip(console: &str) -> Option<String> {
    // Primary: explicit marker from print-ip.sh.
    for line in console.lines().rev() {
        if line.contains("TIGRIMOS_IP=") {
            if let Some(rest) = line.split("TIGRIMOS_IP=").last() {
                let ip = rest.trim().split_whitespace().next().unwrap_or("");
                if is_valid_vm_ip(ip) {
                    return Some(ip.to_string());
                }
            }
        }
    }
    // Fallback: DHCP lease messages.
    for line in console.lines().rev() {
        if line.contains("lease of") || line.contains("acquired") || line.contains("bound to") {
            for word in line.split_whitespace() {
                let clean: String = word
                    .chars()
                    .filter(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if is_valid_vm_ip(&clean) {
                    return Some(clean);
                }
            }
        }
    }
    None
}

fn is_valid_vm_ip(ip: &str) -> bool {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    if !parts.iter().all(|p| p.parse::<u8>().is_ok()) {
        return false;
    }
    ip != "127.0.0.1" && ip != "0.0.0.0"
}

/// Parse `arp -a` to find a VM IP on a bridge / vmnet interface.
async fn find_vm_via_arp() -> Option<String> {
    let output = Command::new("/usr/sbin/arp")
        .arg("-a")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains("bridge") || line.contains("vmnet") {
            if let (Some(start), Some(end)) = (line.find('('), line.find(')')) {
                let ip = &line[start + 1..end];
                if is_valid_vm_ip(ip)
                    && ip.starts_with("192.168.")
                    && !ip.ends_with(".1")
                {
                    return Some(ip.to_string());
                }
            }
        }
    }
    None
}

/// Raw TCP connect test with a 2-second timeout.
pub async fn try_tcp_connect(host: &str, port: u16) -> bool {
    let addr = format!("{}:{}", host, port);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        TcpStream::connect(&addr),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Locate the tiger_cowork source directory on the host.
fn find_tiger_cowork_source() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from("/Users/sompoteyouwai/env/Tigris/tiger_cowork"),
        std::env::current_exe()
            .ok()?
            .parent()?
            .parent()?
            .join("tiger_cowork"),
    ];
    for p in &candidates {
        if p.join("package.json").exists() {
            return Some(p.clone());
        }
    }
    None
}

// ===========================================================================
// Cloud-init user-data (embedded as a constant so it compiles even without
// the external YAML file).
// ===========================================================================

const CLOUD_INIT_USER_DATA: &str = r##"#cloud-config
hostname: tigris
manage_etc_hosts: true

users:
  - name: tigris
    groups: sudo
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: false
    plain_text_passwd: "tigris"

ssh_pwauth: true

bootcmd:
  - echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4

write_files:
  - path: /opt/setup-host-gateway.sh
    permissions: "0755"
    content: |
      #!/bin/bash
      GW=$(ip route | grep default | awk '{print $3}' | head -1)
      if [ -n "$GW" ]; then
        grep -q "host.local" /etc/hosts || echo "$GW host.local host.docker.internal" >> /etc/hosts
        echo "[TigrimOS] Host gateway: $GW (accessible as host.local)"
      fi

  - path: /etc/systemd/system/host-gateway.service
    content: |
      [Unit]
      Description=Set host.local to macOS gateway IP
      After=network-online.target
      Wants=network-online.target

      [Service]
      Type=oneshot
      ExecStart=/bin/bash /opt/setup-host-gateway.sh
      RemainAfterExit=yes

      [Install]
      WantedBy=multi-user.target

  - path: /opt/setup-tigris.sh
    permissions: "0755"
    content: |
      #!/bin/bash
      set -e
      export DEBIAN_FRONTEND=noninteractive

      echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4

      retry() {
        local n=$1; shift; local d=$1; shift; local i=1
        while true; do
          "$@" && break || {
            [ $i -ge $n ] && echo "[WARN] Failed after $n attempts: $*" && return 1
            echo "[RETRY $i/$n] retrying in ${d}s..."; sleep "$d"; ((i++))
          }
        done
      }

      echo "[TigrimOS] Waiting for network..."
      for i in $(seq 1 15); do
        if wget -q --spider --timeout=5 http://ports.ubuntu.com 2>/dev/null || \
           ping -c1 -W3 192.168.64.1 >/dev/null 2>&1; then
          echo "[TigrimOS] Network OK"
          break
        fi
        echo "[TigrimOS] Network not ready, waiting... ($i/15)"
        sleep 3
      done

      echo "[TigrimOS] Enabling universe repository..."
      apt-get install -y software-properties-common 2>/dev/null || true
      add-apt-repository -y universe 2>/dev/null || \
        sed -i 's/^deb \(.*\) jammy main$/deb \1 jammy main universe/' /etc/apt/sources.list

      echo "[TigrimOS] Installing base packages..."
      retry 5 15 apt-get update -qq
      retry 3 10 apt-get install -y curl git build-essential python3 python3-pip python3-venv net-tools

      echo "[TigrimOS] Installing Node.js 20..."
      retry 3 5 bash -c 'curl -4 -fsSL https://deb.nodesource.com/setup_20.x | bash -'
      retry 3 5 apt-get install -y nodejs

      echo "[TigrimOS] Setting up Python venv..."
      python3 -m venv /opt/venv
      retry 3 10 /opt/venv/bin/pip install --no-cache-dir numpy pillow matplotlib pandas scipy seaborn openpyxl python-docx

      echo "[TigrimOS] Installing npm packages..."
      retry 3 10 npm i -g clawhub tsx

      echo "[TigrimOS] Setting up TigrimOS..."
      mkdir -p /app

      if [ -d /mnt/tiger-cowork ] && [ -f /mnt/tiger-cowork/package.json ]; then
        echo "[TigrimOS] Copying from local source..."
        cp -r /mnt/tiger-cowork/* /app/
      else
        echo "[TigrimOS] Local source not found, cloning from GitHub..."
        apt-get install -y -qq git 2>/dev/null || true
        retry 3 5 git clone --depth 1 https://github.com/Sompote/TigrimOS.git /tmp/tigris-src
        cp -r /tmp/tigris-src/tiger_cowork/* /app/
        rm -rf /tmp/tigris-src
      fi

      cd /app
      npm install --ignore-scripts --omit=dev 2>/dev/null || true
      npm install tsx 2>/dev/null || true

      if [ -d client ]; then
        cd client && npm install && npx vite build 2>/dev/null || true
        cd /app
      fi

      mkdir -p /app/data /app/data/agents /app/uploads /app/output_file /app/Tiger_bot/skills
      chown -R tigris:tigris /app

      cat > /etc/systemd/system/tiger-cowork.service << 'SVCEOF'
      [Unit]
      Description=TigrimOS
      After=network.target

      [Service]
      Type=simple
      User=tigris
      WorkingDirectory=/app
      Environment=NODE_ENV=production
      Environment=PORT=3001
      Environment=SANDBOX_DIR=/app
      Environment=PATH=/opt/venv/bin:/usr/local/bin:/usr/bin:/bin
      ExecStart=/usr/bin/npx tsx server/index.ts
      Restart=always
      RestartSec=5

      [Install]
      WantedBy=multi-user.target
      SVCEOF

      systemctl daemon-reload
      systemctl enable tiger-cowork
      systemctl start tiger-cowork

      touch /var/lib/tigris-provisioned
      echo "[TigrimOS] Setup complete!"

  - path: /opt/print-ip.sh
    permissions: "0755"
    content: |
      #!/bin/bash
      for i in $(seq 1 60); do
        IP=$(ip -4 addr show scope global | grep inet | awk '{print $2}' | cut -d/ -f1 | head -1)
        if [ -n "$IP" ] && [ "$IP" != "127.0.0.1" ]; then
          echo "TIGRIMOS_IP=$IP" > /dev/hvc0 2>/dev/null || true
          echo "TIGRIMOS_IP=$IP"
          while true; do
            sleep 10
            echo "TIGRIMOS_IP=$IP" > /dev/hvc0 2>/dev/null || true
          done
        fi
        sleep 2
      done
      echo "TIGRIMOS_IP=FAILED" > /dev/hvc0 2>/dev/null || true

  - path: /etc/systemd/system/print-ip.service
    content: |
      [Unit]
      Description=Print VM IP to serial console for host detection
      After=network-online.target
      Wants=network-online.target

      [Service]
      Type=simple
      ExecStart=/bin/bash /opt/print-ip.sh
      Restart=always
      RestartSec=5

      [Install]
      WantedBy=multi-user.target

  - path: /etc/systemd/system/mount-virtiofs.service
    content: |
      [Unit]
      Description=Mount VirtioFS shared folders
      After=local-fs.target

      [Service]
      Type=oneshot
      ExecStart=/bin/bash -c 'mkdir -p /mnt/tiger-cowork && mount -t virtiofs tiger-cowork /mnt/tiger-cowork 2>/dev/null || mount -t 9p -o trans=virtio tiger-cowork /mnt/tiger-cowork 2>/dev/null || true; for tag in shared-0 shared-1 shared-2 shared-3 shared-4; do mkdir -p /mnt/$tag && mount -t virtiofs $tag /mnt/$tag 2>/dev/null || mount -t 9p -o trans=virtio $tag /mnt/$tag 2>/dev/null || true; done'
      RemainAfterExit=yes

      [Install]
      WantedBy=multi-user.target

runcmd:
  - systemctl enable mount-virtiofs
  - systemctl start mount-virtiofs
  - systemctl enable host-gateway
  - systemctl start host-gateway
  - systemctl enable print-ip
  - systemctl start print-ip
  - /bin/bash /opt/setup-tigris.sh

final_message: "Tigris cloud-init complete"
"##;

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tigrimos_ip_from_console() {
        let console = "[12:00:01] boot\n[12:00:05] TIGRIMOS_IP=192.168.64.3\n";
        assert_eq!(detect_vm_ip(console), Some("192.168.64.3".to_string()));
    }

    #[test]
    fn parse_tigrimos_ip_failed_marker() {
        let console = "TIGRIMOS_IP=FAILED\n";
        assert_eq!(detect_vm_ip(console), None);
    }

    #[test]
    fn valid_ip_check() {
        assert!(is_valid_vm_ip("192.168.64.3"));
        assert!(is_valid_vm_ip("10.0.0.2"));
        assert!(!is_valid_vm_ip("127.0.0.1"));
        assert!(!is_valid_vm_ip("0.0.0.0"));
        assert!(!is_valid_vm_ip("abc"));
        assert!(!is_valid_vm_ip("999.999.999.999"));
    }

    #[tokio::test]
    async fn tcp_connect_fails_on_random_port() {
        // Port 59999 is unlikely to be in use.
        let result = try_tcp_connect("127.0.0.1", 59999).await;
        assert!(!result);
    }

    #[test]
    fn find_qemu_img_does_not_panic() {
        let _ = VmManager::find_qemu_img();
    }

    #[test]
    fn vm_state_display() {
        assert_eq!(VmState::Running.to_string(), "Running");
        assert_eq!(VmState::Downloading.label(), "Downloading VM Image...");
    }
}
