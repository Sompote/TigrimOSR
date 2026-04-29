//! VM module — configuration and lifecycle management for the TigrimOS virtual machine.
//!
//! Uses QEMU as the VM backend (qemu-system-aarch64 / qemu-system-x86_64) instead of
//! Apple Virtualization.framework, making the code portable across macOS and Linux.

pub mod config;
pub mod manager;

// Re-export key types for convenient access from other modules.
pub use config::VmConfig;
pub use manager::{ProcessResult, SharedFolderEntry, TigrimOSError, VmManager, VmState};
