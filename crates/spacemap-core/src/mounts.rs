use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::discover::Disk;

/// A mount visible in this process, used to avoid walking other filesystems
/// and to jump to the owning physical disk.
#[derive(Clone, Debug)]
pub struct MountEntry {
    pub target: PathBuf,
    pub source: String,
    pub fstype: String,
    pub disk: Option<String>,
    pub partition: Option<String>,
    pub model: Option<String>,
    pub physical: bool,
    pub used: u64,
}

#[derive(Clone, Debug, Default)]
pub struct MountTable {
    entries: Vec<MountEntry>,
    by_target: HashMap<PathBuf, usize>,
}

impl MountTable {
    pub fn load(disks: &[Disk]) -> Result<Self> {
        let text = std::fs::read_to_string("/proc/self/mountinfo")
            .context("read /proc/self/mountinfo")?;
        Ok(Self::from_mountinfo(&text, disks))
    }

    pub fn from_mountinfo(text: &str, disks: &[Disk]) -> Self {
        let mut entries: Vec<MountEntry> = text.lines().filter_map(parse_mountinfo_line).collect();
        attach_disks(&mut entries, disks);
        let mut by_target = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            by_target.insert(e.target.clone(), i);
        }
        Self { entries, by_target }
    }

    pub fn get(&self, path: &Path) -> Option<&MountEntry> {
        self.by_target.get(path).map(|&i| &self.entries[i])
    }

    pub fn is_mount_point(&self, path: &Path) -> bool {
        self.by_target.contains_key(path)
    }

    pub fn identity(&self, path: &Path) -> Option<&MountEntry> {
        self.get(path).or_else(|| {
            // Find the longest mount prefix (the filesystem that contains path).
            self.entries
                .iter()
                .filter(|e| path.starts_with(&e.target))
                .max_by_key(|e| e.target.as_os_str().len())
        })
    }

    /// Mount points strictly inside `scan_root` (not `scan_root` itself).
    pub fn nested(&self, scan_root: &Path) -> Vec<&MountEntry> {
        self.entries
            .iter()
            .filter(|e| is_nested_target(scan_root, &e.target))
            .collect()
    }

    pub fn is_nested_mount(&self, scan_root: &Path, path: &Path) -> bool {
        if path == scan_root {
            return false;
        }
        self.is_mount_point(path) && path.starts_with(scan_root)
    }

    /// True if `path` lives under a nested mount of `scan_root` (so it belongs
    /// to another filesystem).
    pub fn is_foreign_path(&self, scan_root: &Path, path: &Path) -> bool {
        if !path.starts_with(scan_root) {
            return true;
        }
        self.nested(scan_root).iter().any(|e| {
            path == e.target.as_path() || path.starts_with(&e.target)
        })
    }
}

fn is_nested_target(scan_root: &Path, target: &Path) -> bool {
    target != scan_root && target.starts_with(scan_root)
}

pub fn unescape_mount(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            if let Ok(oct) = std::str::from_utf8(&bytes[i + 1..i + 4]) {
                if let Ok(c) = u8::from_str_radix(oct, 8) {
                    out.push(c as char);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn parse_mountinfo_line(line: &str) -> Option<MountEntry> {
    let (left, right) = line.split_once(" - ")?;
    let mut lf = left.split(' ');
    let _id = lf.next()?;
    let _parent = lf.next()?;
    let _dev = lf.next()?;
    let _fsroot = lf.next()?;
    let target = PathBuf::from(unescape_mount(lf.next()?));
    let mut rf = right.split(' ');
    let fstype = rf.next()?.to_string();
    let source = unescape_mount(rf.next().unwrap_or(""));
    Some(MountEntry {
        target,
        source,
        fstype,
        disk: None,
        partition: None,
        model: None,
        physical: false,
        used: 0,
    })
}

fn attach_disks(entries: &mut [MountEntry], disks: &[Disk]) {
    for e in entries.iter_mut() {
        let src = normalize_source(&e.source);
        for d in disks {
            for p in &d.partitions {
                let matched_path = p.mounts.iter().any(|m| m.path == e.target);
                let matched_dev = device_aliases(p).iter().any(|alias| {
                    src == *alias || src.ends_with(&format!("/{alias}")) || src == format!("/dev/{alias}")
                });
                if matched_path || matched_dev {
                    e.disk = Some(d.name.clone());
                    e.partition = Some(p.name.clone());
                    e.model = Some(d.model.clone());
                    e.physical = true;
                    if let Some(m) = p.mounts.iter().find(|m| m.path == e.target) {
                        e.used = m.used;
                    } else if e.used == 0 {
                        e.used = p.mounts.first().map(|m| m.used).unwrap_or(0);
                    }
                    break;
                }
            }
            if e.physical {
                break;
            }
        }
    }
}

fn device_aliases(p: &crate::discover::Partition) -> Vec<String> {
    let mut v = vec![p.name.clone()];
    if let Some(m) = &p.mapped_name {
        v.push(m.clone());
        v.push(format!("mapper/{m}"));
    }
    v
}

fn normalize_source(source: &str) -> String {
    if let Some(idx) = source.find('[') {
        source[..idx].to_string()
    } else {
        source.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_spaces() {
        assert_eq!(
            unescape_mount(r"/run/media/russell/Western\040Digital\040HDD"),
            "/run/media/russell/Western Digital HDD"
        );
    }

    #[test]
    fn nested_under_root() {
        let table = MountTable::from_mountinfo(
            "32 1 0:29 /@ / rw - btrfs /dev/mapper/omarchy_root rw\n\
             50 32 0:29 /@home /home rw - btrfs /dev/mapper/omarchy_root rw\n\
             51 32 8:85 / /boot rw - vfat /dev/sdf5 rw\n\
             29 32 0:26 / /run rw - tmpfs run rw\n",
            &[],
        );
        let root = Path::new("/");
        let nested: Vec<_> = table
            .nested(root)
            .into_iter()
            .map(|e| e.target.to_string_lossy().into_owned())
            .collect();
        assert!(nested.iter().any(|t| t == "/home"));
        assert!(nested.iter().any(|t| t == "/boot"));
        assert!(nested.iter().any(|t| t == "/run"));
        assert!(!nested.iter().any(|t| t == "/"));
        assert!(table.is_nested_mount(root, Path::new("/home")));
        assert!(!table.is_nested_mount(root, root));
        assert!(table.is_foreign_path(root, Path::new("/home/russell")));
        assert!(!table.is_foreign_path(root, Path::new("/usr/bin")));
    }

    #[test]
    fn scan_of_home_does_not_treat_root_as_nested() {
        let table = MountTable::from_mountinfo(
            "32 1 0:29 /@ / rw - btrfs /dev/mapper/omarchy_root rw\n\
             50 32 0:29 /@home /home rw - btrfs /dev/mapper/omarchy_root rw\n",
            &[],
        );
        assert!(table.nested(Path::new("/home")).is_empty());
    }
}
