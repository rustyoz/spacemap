use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::color::rgb_for_kind;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub generated_ms: u128,
    pub source: SnapshotSource,
    pub disks: Vec<DiskView>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SnapshotSource {
    Daemon,
    Local,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskView {
    pub name: String,
    pub model: String,
    pub serial: String,
    pub transport: String,
    pub size: u64,
    pub rotational: bool,
    pub removable: bool,
    pub unallocated: u64,
    pub partitions: Vec<PartitionView>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PartitionView {
    pub name: String,
    pub mapped_name: Option<String>,
    pub label: Option<String>,
    pub fstype: String,
    pub size: u64,
    #[serde(default)]
    pub start: u64,
    pub locked: bool,
    pub mounts: Vec<MountView>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MountView {
    pub path: String,
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub scan: ScanStatus,
    pub tree: ViewNode,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewNode {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub kind: Kind,
    pub children: Vec<ViewNode>,
}

impl ViewNode {
    pub fn color_rgb(&self) -> (u8, u8, u8) {
        rgb_for_kind(&self.kind, &self.name)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    Directory,
    File { ext: String },
    Free,
    Overhead,
    Scanning,
    Other,
    Unmounted,
    Locked,
    Swap,
    /// A nested mount of another volume. Size is display-only; bytes live on `disk`.
    MountPoint {
        disk: String,
        partition: String,
        mount: String,
        model: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ScanStatus {
    Pending,
    Scanning { files: u64, bytes: u64 },
    Live { files: u64, bytes: u64 },
    Error(String),
}

impl ScanStatus {
    pub fn label(&self) -> String {
        match self {
            ScanStatus::Pending => "waiting".into(),
            ScanStatus::Scanning { files, bytes } => {
                format!("scanning {} files · {}", files, crate::bytes::format_bytes(*bytes))
            }
            ScanStatus::Live { files, bytes } => {
                format!("{} files · {}", files, crate::bytes::format_bytes(*bytes))
            }
            ScanStatus::Error(e) => format!("error: {e}"),
        }
    }
}

impl Snapshot {
    pub fn empty(source: SnapshotSource) -> Self {
        Self {
            generated_ms: 0,
            source,
            disks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub disk: String,
    pub partition: String,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub kind: Kind,
}
