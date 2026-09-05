use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct Disk {
    pub name: String,
    pub model: String,
    pub serial: String,
    pub transport: String,
    pub size: u64,
    pub rotational: bool,
    pub removable: bool,
    pub partitions: Vec<Partition>,
    pub unallocated: u64,
}

#[derive(Clone, Debug)]
pub struct Partition {
    pub name: String,
    pub mapped_name: Option<String>,
    pub label: Option<String>,
    pub fstype: String,
    pub size: u64,
    /// Byte offset from the start of the disk (Linux 512-byte sectors).
    pub start: u64,
    pub locked: bool,
    pub mounts: Vec<Mount>,
}

#[derive(Clone, Debug)]
pub struct Mount {
    pub path: PathBuf,
    pub total: u64,
    pub used: u64,
    pub available: u64,
}

#[derive(Deserialize)]
struct Lsblk {
    blockdevices: Vec<LsblkDev>,
}

#[derive(Deserialize)]
struct LsblkDev {
    name: String,
    kname: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "crate::discover::de_u64")]
    size: u64,
    #[serde(default, deserialize_with = "crate::discover::de_u64")]
    start: u64,
    model: Option<String>,
    serial: Option<String>,
    fstype: Option<String>,
    label: Option<String>,
    rota: Option<bool>,
    tran: Option<String>,
    rm: Option<bool>,
    hotplug: Option<bool>,
    children: Option<Vec<LsblkDev>>,
}

#[derive(Deserialize)]
struct Findmnt {
    filesystems: Vec<FindmntFs>,
}

#[derive(Deserialize)]
struct FindmntFs {
    source: Option<String>,
    target: Option<String>,
    fstype: Option<String>,
}

fn de_u64<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("u64 or numeric string")
        }
        fn visit_u64<E>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }
        fn visit_i64<E>(self, v: i64) -> Result<u64, E>
        where
            E: serde::de::Error,
        {
            Ok(v.max(0) as u64)
        }
        fn visit_str<E>(self, v: &str) -> Result<u64, E>
        where
            E: serde::de::Error,
        {
            v.parse().map_err(E::custom)
        }
        fn visit_none<E>(self) -> Result<u64, E> {
            Ok(0)
        }
        fn visit_unit<E>(self) -> Result<u64, E> {
            Ok(0)
        }
    }
    d.deserialize_any(V)
}

const SKIP_DISK_PREFIXES: &[&str] = &["loop", "ram", "zram", "sr", "fd", "nbd", "md"];

fn skip_disk(name: &str, size: u64) -> bool {
    if size == 0 {
        return true;
    }
    SKIP_DISK_PREFIXES.iter().any(|p| name.starts_with(p))
}

pub(crate) fn skip_fstype(ft: &str) -> bool {
    matches!(
        ft,
        "proc"
            | "sysfs"
            | "cgroup"
            | "cgroup2"
            | "tmpfs"
            | "devtmpfs"
            | "devpts"
            | "securityfs"
            | "pstore"
            | "bpf"
            | "tracefs"
            | "debugfs"
            | "fusectl"
            | "configfs"
            | "mqueue"
            | "hugetlbfs"
            | "overlay"
            | "nsfs"
            | "autofs"
            | "binfmt_misc"
            | "efivarfs"
            | "rpc_pipefs"
            | "nfs"
            | "nfs4"
            | "cifs"
            | "smb3"
            | "fuse.gvfsd-fuse"
            | "fuse.portal"
            | "squashfs"
    ) || ft.starts_with("fuse.")
}

pub fn discover_disks() -> Result<Vec<Disk>> {
    let lsblk = Command::new("lsblk")
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,KNAME,TYPE,SIZE,START,MODEL,SERIAL,FSTYPE,LABEL,ROTA,TRAN,RM,HOTPLUG",
        ])
        .output()
        .context("lsblk")?;
    if !lsblk.status.success() {
        anyhow::bail!(
            "lsblk failed: {}",
            String::from_utf8_lossy(&lsblk.stderr)
        );
    }
    let parsed: Lsblk = serde_json::from_slice(&lsblk.stdout).context("parse lsblk json")?;

    let mounts = load_mounts()?;
    let mut disks = Vec::new();

    for dev in parsed.blockdevices {
        if dev.kind != "disk" || skip_disk(&dev.name, dev.size) {
            continue;
        }
        let mut parts = collect_partitions(&dev, &mounts);
        if parts.is_empty() && dev.fstype.as_deref() == Some("swap") {
            continue;
        }
        let used: u64 = parts.iter().map(|p| p.size).sum();
        let unallocated = dev.size.saturating_sub(used);
        disks.push(Disk {
            name: dev.name,
            model: dev.model.unwrap_or_default(),
            serial: dev.serial.unwrap_or_default(),
            transport: dev.tran.unwrap_or_default(),
            size: dev.size,
            rotational: dev.rota.unwrap_or(false),
            removable: dev.rm.unwrap_or(false) || dev.hotplug.unwrap_or(false),
            partitions: {
                parts.sort_by_key(|p| (p.start, p.name.clone()));
                parts
            },
            unallocated,
        });
    }

    disks.sort_by(|a, b| a.removable.cmp(&b.removable).then(b.size.cmp(&a.size)));
    Ok(disks)
}

fn collect_partitions(disk: &LsblkDev, mounts: &MountIndex) -> Vec<Partition> {
    let mut out = Vec::new();
    for child in disk.children.iter().flatten() {
        if child.kind == "part" || child.kind == "crypt" || child.kind == "lvm" {
            if child.kind == "part" {
                out.push(partition_from(child, mounts));
            }
        }
    }
    out
}

fn partition_from(part: &LsblkDev, mounts: &MountIndex) -> Partition {
    let mut mapped_name = None;
    let mut fstype = part.fstype.clone().unwrap_or_default();
    let mut label = part.label.clone();
    let mut locked = fstype == "crypto_LUKS";
    let mut names = vec![part.name.clone()];
    if let Some(k) = &part.kname {
        names.push(k.clone());
    }

    fn walk_nested(
        node: &LsblkDev,
        mapped_name: &mut Option<String>,
        fstype: &mut String,
        label: &mut Option<String>,
        locked: &mut bool,
        names: &mut Vec<String>,
    ) {
        names.push(node.name.clone());
        if let Some(k) = &node.kname {
            names.push(k.clone());
        }
        if node.kind == "crypt" || node.kind == "lvm" || node.kind == "raid1" || node.kind == "raid0"
        {
            *mapped_name = Some(node.name.clone());
        }
        if let Some(ft) = &node.fstype {
            if ft != "crypto_LUKS" {
                *fstype = ft.clone();
                *locked = false;
            }
        }
        if node.label.is_some() {
            *label = node.label.clone();
        }
        for c in node.children.iter().flatten() {
            walk_nested(c, mapped_name, fstype, label, locked, names);
        }
    }

    for c in part.children.iter().flatten() {
        walk_nested(
            c,
            &mut mapped_name,
            &mut fstype,
            &mut label,
            &mut locked,
            &mut names,
        );
    }

    let mut found = Vec::new();
    for n in &names {
        found.extend(mounts.for_device(n));
    }
    found.sort_by_key(|m| m.path.clone());
    found.dedup_by(|a, b| a.path == b.path);

    if !found.is_empty() {
        locked = false;
        if fstype.is_empty() {
            fstype = "mounted".into();
        }
    }

    Partition {
        name: part.name.clone(),
        mapped_name,
        label,
        fstype,
        size: part.size,
        start: part.start.saturating_mul(512),
        locked,
        mounts: found,
    }
}

struct MountIndex {
    by_dev: HashMap<String, Vec<Mount>>,
}

impl MountIndex {
    fn for_device(&self, name: &str) -> Vec<Mount> {
        let mut keys = vec![name.to_string()];
        keys.push(format!("/dev/{name}"));
        keys.push(format!("/dev/mapper/{name}"));
        keys.push(format!("/dev/dm-{name}"));
        let mut out = Vec::new();
        for k in keys {
            if let Some(v) = self.by_dev.get(&k) {
                out.extend(v.iter().cloned());
            }
        }
        out
    }
}

fn load_mounts() -> Result<MountIndex> {
    let out = Command::new("findmnt")
        .args(["-J", "-l", "-o", "SOURCE,TARGET,FSTYPE"])
        .output()
        .context("findmnt")?;
    if !out.status.success() {
        anyhow::bail!("findmnt failed");
    }
    let parsed: Findmnt = serde_json::from_slice(&out.stdout).context("parse findmnt")?;
    let mut by_dev: HashMap<String, Vec<Mount>> = HashMap::new();

    for fs in parsed.filesystems {
        let Some(target) = fs.target else { continue };
        let Some(source) = fs.source else { continue };
        let ft = fs.fstype.unwrap_or_default();
        if skip_fstype(&ft) {
            continue;
        }
        let path = PathBuf::from(&target);
        if should_skip_target(&path) {
            continue;
        }
        let usage = stat_fs(&path).unwrap_or(Mount {
            path: path.clone(),
            total: 0,
            used: 0,
            available: 0,
        });
        let key = normalize_source(&source);
        by_dev.entry(key.clone()).or_default().push(usage.clone());
        by_dev.entry(source).or_default().push(usage);
    }
    Ok(MountIndex { by_dev })
}

fn should_skip_target(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/proc")
        || s.starts_with("/sys")
        || s.starts_with("/dev")
        || s.starts_with("/run/docker")
        || s.starts_with("/run/user/") && s.contains("/doc")
        || s.starts_with("/tmp/.mount_")
}

fn normalize_source(source: &str) -> String {
    // /dev/mapper/omarchy_root[/@home] -> /dev/mapper/omarchy_root
    if let Some(idx) = source.find('[') {
        source[..idx].to_string()
    } else {
        source.to_string()
    }
}

pub fn stat_fs(path: &Path) -> Option<Mount> {
    let cstr = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let mut vfs = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if libc::statvfs(cstr.as_ptr(), vfs.as_mut_ptr()) != 0 {
            return None;
        }
        let vfs = vfs.assume_init();
        let fr = vfs.f_frsize as u64;
        let total = vfs.f_blocks.saturating_mul(fr);
        let free_all = vfs.f_bfree.saturating_mul(fr);
        let available = vfs.f_bavail.saturating_mul(fr);
        let used = total.saturating_sub(free_all);
        Some(Mount {
            path: path.to_path_buf(),
            total,
            used,
            available,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_physical_disks() {
        let disks = discover_disks().expect("lsblk");
        assert!(
            disks.iter().any(|d| !d.partitions.is_empty()),
            "expected at least one partitioned disk, got {disks:?}"
        );
    }
}
