use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use parking_lot::{Mutex, RwLock};
use tracing::{info, warn};

use crate::discover::{discover_disks, stat_fs, Disk, Partition};
use crate::ipc::data_dir;
use crate::mounts::MountTable;
use crate::scan::{apply_change, graft_expand_layer, scan_into, scan_mount, watch_mounts, ScanProgress};
use crate::snapshot::{
    DiskView, Kind, MountView, PartitionView, ScanStatus, Snapshot, SnapshotSource, ViewNode,
};
use crate::tree::FileTree;

type SharedTree = Arc<RwLock<FileTree>>;

enum ExpandResult {
    Done,
    Retry,
}

pub struct Engine {
    source: SnapshotSource,
    disks: RwLock<Vec<Disk>>,
    mounts: RwLock<MountTable>,
    trees: RwLock<HashMap<PathBuf, SharedTree>>,
    progress: RwLock<HashMap<PathBuf, ScanProgress>>,
    stop: AtomicBool,
    rescan_flag: AtomicBool,
    force_rescan: AtomicBool,
    watch_reload: AtomicBool,
    dirty: Mutex<Vec<PathBuf>>,
    expand_queue: Mutex<Vec<PathBuf>>,
    persist_dir: PathBuf,
    ticks: AtomicU64,
}

impl Engine {
    pub fn new(source: SnapshotSource) -> Result<Arc<Self>> {
        let persist_dir = data_dir();
        std::fs::create_dir_all(&persist_dir)?;
        let engine = Arc::new(Self {
            source,
            disks: RwLock::new(Vec::new()),
            mounts: RwLock::new(MountTable::default()),
            trees: RwLock::new(HashMap::new()),
            progress: RwLock::new(HashMap::new()),
            stop: AtomicBool::new(false),
            rescan_flag: AtomicBool::new(true),
            force_rescan: AtomicBool::new(true),
            watch_reload: AtomicBool::new(false),
            dirty: Mutex::new(Vec::new()),
            expand_queue: Mutex::new(Vec::new()),
            persist_dir,
            ticks: AtomicU64::new(0),
        });
        engine.refresh_topology()?;
        engine.load_all();
        Ok(engine)
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn request_rescan(&self, mount: Option<PathBuf>) {
        if let Some(m) = mount {
            self.progress.write().insert(
                m.clone(),
                ScanProgress {
                    files: 0,
                    bytes: 0,
                    done: false,
                    error: None,
                },
            );
            self.trees.write().remove(&m);
            self.rescan_flag.store(true, Ordering::Relaxed);
        } else {
            self.trees.write().clear();
            self.force_rescan.store(true, Ordering::Relaxed);
            self.rescan_flag.store(true, Ordering::Relaxed);
        }
    }

    pub fn request_expand(&self, path: PathBuf) {
        if path.as_os_str().is_empty() {
            return;
        }
        let s = path.to_string_lossy();
        if s.contains("::") {
            return;
        }
        let mut q = self.expand_queue.lock();
        if !q.iter().any(|p| p == &path) {
            q.push(path);
        }
    }

    pub fn refresh_topology(&self) -> Result<()> {
        let disks = discover_disks()?;
        let table = match MountTable::load(&disks) {
            Ok(t) => t,
            Err(err) => {
                warn!(error = %err, "failed to read mount table");
                MountTable::default()
            }
        };
        let old_mounts: Vec<PathBuf> = self
            .disks
            .read()
            .iter()
            .flat_map(|d| {
                d.partitions
                    .iter()
                    .flat_map(|p| p.mounts.iter().map(|m| m.path.clone()))
            })
            .collect();
        let new_mounts: Vec<PathBuf> = disks
            .iter()
            .flat_map(|d| {
                d.partitions
                    .iter()
                    .flat_map(|p| p.mounts.iter().map(|m| m.path.clone()))
            })
            .collect();
        if old_mounts != new_mounts {
            self.watch_reload.store(true, Ordering::Relaxed);
            self.rescan_flag.store(true, Ordering::Relaxed);
        }
        *self.mounts.write() = table;
        *self.disks.write() = disks;
        self.ticks.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn snapshot(&self, min_fraction: f32, focus: &[String], open: &[String]) -> Snapshot {
        // Clone handles and drop engine locks before walking trees. Holding
        // `progress` while waiting on a tree write-lock deadlocks the scanner,
        // which updates progress from inside that write-lock.
        let disks = self.disks.read().clone();
        let trees = self.trees.read().clone();
        let progress = self.progress.read().clone();
        let min_fraction = min_fraction.clamp(0.0005, 0.05);

        let views = disks
            .iter()
            .map(|d| DiskView {
                name: d.name.clone(),
                model: d.model.clone(),
                serial: d.serial.clone(),
                transport: d.transport.clone(),
                size: d.size,
                rotational: d.rotational,
                removable: d.removable,
                unallocated: d.unallocated,
                partitions: d
                    .partitions
                    .iter()
                    .map(|p| PartitionView {
                        name: p.name.clone(),
                        mapped_name: p.mapped_name.clone(),
                        label: p.label.clone(),
                        fstype: p.fstype.clone(),
                        size: p.size,
                        start: p.start,
                        locked: p.locked,
                        mounts: partition_mounts(
                            p,
                            &trees,
                            &progress,
                            min_fraction,
                            focus,
                            open,
                        ),
                    })
                    .collect(),
            })
            .collect();

        Snapshot {
            generated_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            source: self.source.clone(),
            disks: views,
        }
    }

    pub fn run(self: &Arc<Self>) {
        let engine = Arc::clone(self);
        let stop = &engine.stop;

        let watcher_engine = Arc::clone(&engine);
        std::thread::Builder::new()
            .name("spacemap-watch".into())
            .spawn(move || {
                loop {
                    if watcher_engine.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let mounts: Vec<PathBuf> = watcher_engine
                        .disks
                        .read()
                        .iter()
                        .flat_map(|d| d.partitions.iter().flat_map(|p| p.mounts.iter().map(|m| m.path.clone())))
                        .collect();
                    watcher_engine.watch_reload.store(false, Ordering::Relaxed);
                    let we = Arc::clone(&watcher_engine);
                    let _ = watch_mounts(
                        mounts,
                        |p| we.dirty.lock().push(p),
                        &we.stop,
                        &we.watch_reload,
                    );
                    std::thread::sleep(Duration::from_secs(2));
                }
            })
            .ok();

        let expand_engine = Arc::clone(&engine);
        std::thread::Builder::new()
            .name("spacemap-expand".into())
            .spawn(move || {
                while !expand_engine.stop.load(Ordering::Relaxed) {
                    expand_engine.drain_expands();
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
            .ok();

        let mut last_topo = Instant::now();
        let mut last_full = Instant::now();

        while !stop.load(Ordering::Relaxed) {
            if last_topo.elapsed() > Duration::from_secs(4) {
                if let Err(err) = engine.refresh_topology() {
                    warn!(error = %err, "topology refresh failed");
                }
                last_topo = Instant::now();
            }

            if engine.rescan_flag.swap(false, Ordering::Relaxed)
                || last_full.elapsed() > Duration::from_secs(6 * 3600)
            {
                if last_full.elapsed() > Duration::from_secs(6 * 3600) {
                    engine.force_rescan.store(true, Ordering::Relaxed);
                }
                engine.scan_all();
                last_full = Instant::now();
            }

            let dirty: Vec<PathBuf> = {
                let mut d = engine.dirty.lock();
                let out = d.split_off(0);
                out
            };
            if !dirty.is_empty() {
                engine.apply_dirty(dirty);
            }

            std::thread::sleep(Duration::from_millis(120));
        }

        engine.persist_all();
    }

    fn scan_all(&self) {
        let mut jobs: Vec<(bool, bool, u64, PathBuf)> = Vec::new();
        {
            let disks = self.disks.read();
            for d in disks.iter() {
                for p in &d.partitions {
                    for m in &p.mounts {
                        jobs.push((d.removable, d.rotational, m.total.max(p.size), m.path.clone()));
                    }
                }
            }
        }
        jobs.sort_by_key(|(rem, rota, size, _)| (*rem, *rota, *size));
        let mounts: Vec<PathBuf> = jobs.into_iter().map(|(_, _, _, p)| p).collect();

        let force = self.force_rescan.swap(false, Ordering::Relaxed);
        for mount in mounts {
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            if !force && self.trees.read().contains_key(&mount) {
                continue;
            }
            info!(path = %mount.display(), "scanning");
            let start = Instant::now();
            self.progress.write().insert(
                mount.clone(),
                ScanProgress {
                    files: 0,
                    bytes: 0,
                    done: false,
                    error: None,
                },
            );
            let shared = {
                let mut map = self.trees.write();
                map.entry(mount.clone())
                    .or_insert_with(|| {
                        Arc::new(RwLock::new(FileTree::new(mount.clone())))
                    })
                    .clone()
            };
            let table = self.mounts.read().clone();
            let progress_map = &self.progress;
            let mount_for_cb = mount.clone();
            match scan_into(&shared, &mount, &table, &self.stop, crate::tree::STORE_DEPTH, |files, bytes| {
                progress_map.write().insert(
                    mount_for_cb.clone(),
                    ScanProgress {
                        files,
                        bytes,
                        done: false,
                        error: None,
                    },
                );
            }) {
                Ok(()) => {
                    let (files, bytes) = {
                        let tree = shared.read();
                        (tree.files, tree.bytes)
                    };
                    info!(
                        path = %mount.display(),
                        files,
                        bytes,
                        elapsed = ?start.elapsed(),
                        "scan complete"
                    );
                    self.progress.write().insert(
                        mount.clone(),
                        ScanProgress {
                            files,
                            bytes,
                            done: true,
                            error: None,
                        },
                    );
                    self.persist_tree(&mount, &shared.read());
                }
                Err(err) => {
                    warn!(path = %mount.display(), error = %err, "scan failed");
                    self.progress.write().insert(
                        mount.clone(),
                        ScanProgress {
                            files: 0,
                            bytes: 0,
                            done: true,
                            error: Some(err.to_string()),
                        },
                    );
                }
            }
            self.ticks.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn apply_dirty(&self, paths: Vec<PathBuf>) {
        let table = self.mounts.read().clone();
        let mut unique = paths;
        unique.sort();
        unique.dedup();
        let trees = self.trees.write();
        for path in unique.into_iter().take(400) {
            let Some(mount) = trees
                .keys()
                .filter(|m| path.starts_with(m))
                .max_by_key(|m| m.as_os_str().len())
                .cloned()
            else {
                continue;
            };
            if let Some(tree) = trees.get(&mount) {
                apply_change(&mut tree.write(), &path, &table);
            }
        }
        self.ticks.fetch_add(1, Ordering::Relaxed);
    }

    fn drain_expands(&self) {
        let jobs: Vec<PathBuf> = {
            let mut q = self.expand_queue.lock();
            if q.is_empty() {
                return;
            }
            let mut seen = HashSet::new();
            q.drain(..)
                .filter(|p| seen.insert(p.clone()))
                .collect()
        };
        let mut retry = Vec::new();
        for path in jobs {
            if self.stop.load(Ordering::Relaxed) {
                retry.push(path);
                continue;
            }
            match self.expand_dir(&path) {
                ExpandResult::Done => {}
                ExpandResult::Retry => retry.push(path),
            }
        }
        if !retry.is_empty() {
            self.expand_queue.lock().extend(retry);
        }
    }

    fn expand_dir(&self, path: &Path) -> ExpandResult {
        let trees = self.trees.read().clone();
        let Some(mount) = trees
            .keys()
            .filter(|m| path.starts_with(m))
            .max_by_key(|m| m.as_os_str().len())
            .cloned()
        else {
            return ExpandResult::Retry;
        };
        let scanning = self
            .progress
            .read()
            .get(&mount)
            .map(|p| !p.done)
            .unwrap_or(true);
        if scanning {
            return ExpandResult::Retry;
        }
        let Some(shared) = trees.get(&mount).cloned() else {
            return ExpandResult::Retry;
        };
        drop(trees);
        {
            let mut tree = shared.write();
            if tree.dirs.get(path).is_none() {
                tree.ensure_dir_uncapped(path);
            }
        }
        if !path.is_dir() {
            return ExpandResult::Done;
        }
        let table = self.mounts.read().clone();
        info!(path = %path.display(), "expanding");
        let start = Instant::now();
        let sub = match scan_mount(path, &table, &self.stop, 1, |_, _| {}) {
            Ok(t) => t,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "expand failed");
                return ExpandResult::Done;
            }
        };
        let added = {
            let mut tree = shared.write();
            graft_expand_layer(&mut tree, path, &sub)
        };
        if added > 0 {
            info!(
                path = %path.display(),
                added,
                elapsed = ?start.elapsed(),
                "expand complete"
            );
            self.persist_tree(&mount, &shared.read());
            self.ticks.fetch_add(1, Ordering::Relaxed);
        }
        ExpandResult::Done
    }

    fn persist_all(&self) {
        let trees = self.trees.read();
        for (path, tree) in trees.iter() {
            self.persist_tree(path, &tree.read());
        }
    }

    fn persist_tree(&self, mount: &Path, tree: &FileTree) {
        let path = persist_name(&self.persist_dir, mount);
        match std::fs::File::create(&path).and_then(|f| {
            bincode::serialize_into(f, tree).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        }) {
            Ok(()) => {}
            Err(err) => warn!(path = %path.display(), error = %err, "persist failed"),
        }
    }

    fn load_all(&self) {
        let mounts: Vec<PathBuf> = self
            .disks
            .read()
            .iter()
            .flat_map(|d| {
                d.partitions
                    .iter()
                    .flat_map(|p| p.mounts.iter().map(|m| m.path.clone()))
            })
            .collect();
        for mount in mounts {
            let path = persist_name(&self.persist_dir, &mount);
            let Ok(file) = std::fs::File::open(&path) else { continue };
            match bincode::deserialize_from::<_, FileTree>(file) {
                Ok(mut tree) => {
                    if tree.store_depth != crate::tree::STORE_DEPTH {
                        warn!(
                            path = %path.display(),
                            stored = tree.store_depth,
                            "ignoring deep cached tree"
                        );
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    tree.prune_to_store_depth();
                    tree.rebuild_index();
                    info!(path = %mount.display(), files = tree.files, nodes = tree.nodes.len(), "loaded cached tree");
                    self.progress.write().insert(
                        mount.clone(),
                        ScanProgress {
                            files: tree.files,
                            bytes: tree.bytes,
                            done: true,
                            error: None,
                        },
                    );
                    self.trees.write().insert(mount, Arc::new(RwLock::new(tree)));
                }
                Err(err) => {
                    warn!(path = %path.display(), error = %err, "cache load failed");
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

fn tree_snapshot<'a>(
    trees: &'a HashMap<PathBuf, SharedTree>,
    path: &Path,
) -> Option<parking_lot::RwLockReadGuard<'a, FileTree>> {
    trees.get(path)?.try_read_for(Duration::from_millis(400))
}

fn focus_for_mount(mount: &Path, focus: &[String]) -> Option<PathBuf> {
    focus
        .iter()
        .map(Path::new)
        .filter(|p| *p == mount || p.starts_with(mount))
        .max_by_key(|p| p.as_os_str().len())
        .map(|p| p.to_path_buf())
}

fn open_for_mount(mount: &Path, open: &[String]) -> Vec<PathBuf> {
    open.iter()
        .map(Path::new)
        .filter(|p| *p == mount || p.starts_with(mount))
        .map(|p| p.to_path_buf())
        .collect()
}

fn partition_mounts(
    p: &Partition,
    trees: &HashMap<PathBuf, SharedTree>,
    progress: &HashMap<PathBuf, ScanProgress>,
    min_fraction: f32,
    focus: &[String],
    open: &[String],
) -> Vec<MountView> {
    if p.mounts.is_empty() {
        return Vec::new();
    }

    let usage: Vec<crate::discover::Mount> = p
        .mounts
        .iter()
        .map(|m| stat_fs(&m.path).unwrap_or_else(|| m.clone()))
        .collect();

    let share_pool = usage.len() > 1
        && usage
            .iter()
            .all(|m| m.total == usage[0].total && m.available == usage[0].available);

    if !share_pool {
        return usage
            .iter()
            .map(|m| {
                let guard = tree_snapshot(trees, &m.path);
                let tree = guard.as_deref();
                let scan = progress_for(m, tree, progress);
                let at = focus_for_mount(&m.path, focus);
                let opened = open_for_mount(&m.path, open);
                mount_view(
                    m,
                    tree,
                    &scan,
                    min_fraction,
                    true,
                    &p.fstype,
                    at.as_deref(),
                    &opened,
                )
            })
            .collect();
    }

    let min_size = ((usage[0].total as f32) * min_fraction).max(64.0 * 1024.0) as u64;
    let mut children = Vec::new();
    let mut scanned = 0u64;
    let mut files = 0u64;
    let mut scanning = false;
    let mut err = None;

    for m in &usage {
        let guard = tree_snapshot(trees, &m.path);
        let tree = guard.as_deref();
        let scan = progress_for(m, tree, progress);
        if let Some(e) = &scan.error {
            err = Some(e.clone());
        }
        if !scan.done {
            scanning = true;
        }
        files += scan.files;
        let at = focus_for_mount(&m.path, focus);
        let opened = open_for_mount(&m.path, open);
        let inner = mount_view(
            m,
            tree,
            &scan,
            min_fraction,
            false,
            &p.fstype,
            at.as_deref(),
            &opened,
        );
        scanned += tree.map(|t| t.bytes).unwrap_or(scan.bytes);
        children.push(ViewNode {
            path: m.path.to_string_lossy().into_owned(),
            name: m.path.to_string_lossy().into_owned(),
            size: inner
                .tree
                .children
                .iter()
                .map(|c| c.size)
                .sum::<u64>()
                .max(1),
            kind: Kind::Directory,
            children: inner.tree.children,
        });
    }

    let pool = &usage[0];
    push_usage_gap(
        &mut children,
        &format!("{}::gap", p.name),
        pool.used.saturating_sub(scanned),
        min_size,
        !scanning,
        &p.fstype,
    );
    if pool.available > 0 {
        children.push(ViewNode {
            path: format!("{}::free", p.name),
            name: "free".into(),
            size: pool.available,
            kind: Kind::Free,
            children: Vec::new(),
        });
    }
    children.sort_by(|a, b| b.size.cmp(&a.size));

    let status = if let Some(e) = err {
        ScanStatus::Error(e)
    } else if scanning {
        ScanStatus::Scanning {
            files,
            bytes: scanned,
        }
    } else {
        ScanStatus::Live {
            files,
            bytes: scanned,
        }
    };

    let name = p
        .mapped_name
        .clone()
        .or_else(|| p.label.clone())
        .unwrap_or_else(|| p.name.clone());

    vec![MountView {
        path: name.clone(),
        total: pool.total,
        used: pool.used,
        available: pool.available,
        scan: status,
        tree: ViewNode {
            path: name.clone(),
            name,
            size: pool.total.max(1),
            kind: Kind::Directory,
            children,
        },
    }]
}

fn progress_for(
    m: &crate::discover::Mount,
    tree: Option<&FileTree>,
    progress: &HashMap<PathBuf, ScanProgress>,
) -> ScanProgress {
    progress.get(&m.path).cloned().unwrap_or(ScanProgress {
        files: tree.map(|t| t.files).unwrap_or(0),
        bytes: tree.map(|t| t.bytes).unwrap_or(0),
        done: tree.is_some(),
        error: None,
    })
}

fn suppress_df_gap(fstype: &str) -> bool {
    let ft = fstype.to_ascii_lowercase();
    matches!(
        ft.as_str(),
        "ntfs" | "ntfs3" | "fuseblk" | "exfat" | "vfat" | "msdos"
    ) || ft.contains("ntfs")
}

fn push_usage_gap(
    children: &mut Vec<ViewNode>,
    path: &str,
    gap: u64,
    min_size: u64,
    scan_done: bool,
    fstype: &str,
) {
    if gap <= min_size {
        return;
    }
    if !scan_done {
        children.push(ViewNode {
            path: path.to_string(),
            name: "scanning".into(),
            size: gap,
            kind: Kind::Scanning,
            children: Vec::new(),
        });
        return;
    }
    if suppress_df_gap(fstype) {
        return;
    }
    children.push(ViewNode {
        path: path.to_string(),
        name: "overhead".into(),
        size: gap,
        kind: Kind::Overhead,
        children: Vec::new(),
    });
}

fn persist_name(dir: &Path, mount: &Path) -> PathBuf {
    let slug: String = mount
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{slug}.bin"))
}

fn mount_view(
    mount: &crate::discover::Mount,
    tree: Option<&FileTree>,
    scan: &ScanProgress,
    min_fraction: f32,
    include_pool: bool,
    fstype: &str,
    focus: Option<&Path>,
    open: &[PathBuf],
) -> MountView {
    let min_size = ((mount.total as f32) * min_fraction).max(64.0 * 1024.0) as u64;
    let zoomed = focus.map(|p| p != mount.path.as_path()).unwrap_or(false);
    let mut children = Vec::new();
    let scanned = if let Some(t) = tree {
        let view = t.view_layer(focus, open, min_size);
        children = view.children;
        t.bytes.max(scan.bytes)
    } else {
        scan.bytes
    };

    if include_pool && !zoomed {
        push_usage_gap(
            &mut children,
            &format!("{}::gap", mount.path.display()),
            mount.used.saturating_sub(scanned),
            min_size,
            scan.done,
            fstype,
        );
        if mount.available > 0 {
            children.push(ViewNode {
                path: format!("{}::free", mount.path.display()),
                name: "free".into(),
                size: mount.available,
                kind: Kind::Free,
                children: Vec::new(),
            });
        }
    }
    children.sort_by(|a, b| b.size.cmp(&a.size));

    let status = if let Some(err) = &scan.error {
        ScanStatus::Error(err.clone())
    } else if !scan.done {
        ScanStatus::Scanning {
            files: scan.files,
            bytes: if scan.bytes > 0 { scan.bytes } else { scanned },
        }
    } else {
        ScanStatus::Live {
            files: scan.files,
            bytes: scanned,
        }
    };

    let name = mount.path.to_string_lossy().into_owned();
    MountView {
        path: name.clone(),
        total: mount.total,
        used: mount.used,
        available: mount.available,
        scan: status,
        tree: ViewNode {
            path: name.clone(),
            name,
            size: mount.total.max(1),
            kind: Kind::Directory,
            children,
        },
    }
}
