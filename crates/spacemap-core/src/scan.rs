use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use parking_lot::RwLock;
use tracing::{debug, warn};
use walkdir::WalkDir;

use crate::mounts::MountTable;
use crate::tree::{cap_path, ext_of, relative_depth, FileTree, StoredKind, STORE_DEPTH};

#[derive(Clone, Debug, Default)]
pub struct ScanProgress {
    pub files: u64,
    pub bytes: u64,
    pub done: bool,
    pub error: Option<String>,
}

pub fn allocated_size(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let blocks = meta.blocks();
        let alloc = blocks.saturating_mul(512);
        // NTFS often under-reports st_blocks relative to the size Windows/df show.
        return alloc.max(meta.len());
    }
    #[cfg(not(unix))]
    meta.len()
}

pub fn scan_into(
    tree: &Arc<RwLock<FileTree>>,
    root: &Path,
    mounts: &MountTable,
    stop: &AtomicBool,
    max_depth: u32,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<()> {
    {
        let mut t = tree.write();
        *t = FileTree::new(root.to_path_buf());
        if let Ok(meta) = std::fs::metadata(root) {
            let root_id = t.root;
            t.nodes[root_id as usize].size = allocated_size(&meta);
        }
    }

    let mut walker = WalkDir::new(root)
        .follow_links(false)
        .same_file_system(true)
        .into_iter();

    let mut stack: Vec<crate::tree::NodeId> = vec![0];
    let mut files = 0u64;
    let mut last_pub = Instant::now();
    let mut batch_started = Instant::now();
    let mut guard = tree.write();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Some(entry) = walker.next() else { break };
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                debug!(error = %err, "walk error");
                continue;
            }
        };
        let depth = entry.depth();
        if depth == 0 {
            continue;
        }

        if mounts.is_nested_mount(root, entry.path()) {
            // Yielded this mount dir; skip its contents without skipping siblings.
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }

        while stack.len() > depth {
            stack.pop();
        }
        let Some(&parent) = stack.last() else { continue };

        let ft = entry.file_type();
        if ft.is_symlink() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = allocated_size(&meta);

        if depth > max_depth as usize {
            guard.add_size(parent, size);
            if ft.is_file() {
                files += 1;
            }
        } else {
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                let id = guard.add_child(
                    parent,
                    name,
                    size,
                    StoredKind::Dir,
                    Some(entry.path().to_path_buf()),
                );
                stack.push(id);
            } else if ft.is_file() {
                let ext = ext_of(&name);
                guard.add_child(parent, name, size, StoredKind::File { ext }, None);
                files += 1;
            }
        }

        if last_pub.elapsed() >= Duration::from_millis(200)
            || batch_started.elapsed() >= Duration::from_millis(250)
        {
            let bytes = guard.nodes[guard.root as usize].size;
            drop(guard);
            on_progress(files, bytes);
            last_pub = Instant::now();
            guard = tree.write();
            batch_started = Instant::now();
        }
    }

    attach_portals(&mut guard, mounts);
    guard.files = files;
    let bytes = guard.nodes[guard.root as usize].size;
    guard.bytes = bytes;
    drop(guard);
    on_progress(files, bytes);
    Ok(())
}

pub fn scan_mount(
    root: &Path,
    mounts: &MountTable,
    stop: &AtomicBool,
    max_depth: u32,
    on_progress: impl FnMut(u64, u64),
) -> Result<FileTree> {
    let tree = Arc::new(RwLock::new(FileTree::new(root.to_path_buf())));
    scan_into(&tree, root, mounts, stop, max_depth, on_progress)?;
    Ok(match Arc::try_unwrap(tree) {
        Ok(lock) => lock.into_inner(),
        Err(arc) => arc.read().clone(),
    })
}

fn attach_portals(tree: &mut FileTree, mounts: &MountTable) {
    let root = tree.root_path.clone();
    let here = mounts.identity(&root);
    let here_disk = here.and_then(|e| e.disk.as_deref());
    let here_part = here.and_then(|e| e.partition.as_deref());
    let here_source = here.map(|e| e.source.as_str());

    for m in mounts.nested(&root) {
        if !m.physical {
            continue;
        }
        if crate::discover::skip_fstype(&m.fstype) {
            continue;
        }
        let same_volume = match (here_source, here_disk, here_part) {
            (Some(src), _, _) if !src.is_empty() && src == m.source => true,
            (_, Some(d), Some(p)) => {
                m.disk.as_deref() == Some(d) && m.partition.as_deref() == Some(p)
            }
            _ => false,
        };
        if same_volume {
            continue;
        }
        let Some(parent_path) = m.target.parent() else { continue };
        let pid = if parent_path == root {
            tree.root
        } else if let Some(&id) = tree.dirs.get(parent_path) {
            id
        } else {
            continue;
        };
        let name = m
            .target
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| m.target.to_string_lossy().into_owned());
        if tree.nodes[pid as usize]
            .children
            .iter()
            .any(|&c| tree.nodes[c as usize].alive && tree.nodes[c as usize].name == name)
        {
            continue;
        }
        let inode = std::fs::metadata(&m.target)
            .map(|meta| allocated_size(&meta))
            .unwrap_or(4096);
        tree.add_child(
            pid,
            name,
            inode,
            StoredKind::MountPoint {
                disk: m.disk.clone().unwrap_or_default(),
                partition: m.partition.clone().unwrap_or_default(),
                mount: m.target.to_string_lossy().into_owned(),
                model: m.model.clone().unwrap_or_default(),
                volume_used: m.used,
            },
            Some(m.target.clone()),
        );
    }
}

pub fn rescan_dir(tree: &mut FileTree, dir: &Path, mounts: &MountTable) -> Result<()> {
    let Some(&id) = tree.dirs.get(dir) else {
        return Ok(());
    };
    if mounts.is_nested_mount(&tree.root_path, dir) {
        return Ok(());
    }
    let depth = relative_depth(&tree.root_path, dir).unwrap_or(0);
    let remaining = STORE_DEPTH.saturating_sub(depth);
    let children = tree.nodes[id as usize].children.clone();
    for c in children {
        if tree.nodes[c as usize].alive {
            let path = tree.path_of(c);
            tree.remove_path(&path);
        }
    }
    if !dir.exists() {
        tree.remove_path(dir);
        return Ok(());
    }

    match scan_mount(
        dir,
        mounts,
        &AtomicBool::new(false),
        remaining,
        |_, _| {},
    ) {
        Ok(sub) => {
            if remaining == 0 {
                let new = sub.bytes;
                let old = tree.nodes[id as usize].size;
                if new > old {
                    tree.add_size(id, new - old);
                } else if old > new {
                    tree.sub_size(id, old - new);
                }
                tree.bytes = tree.nodes[tree.root as usize].size;
            } else {
                merge_subtree(tree, id, sub);
            }
        }
        Err(err) => warn!(path = %dir.display(), error = %err, "rescan failed"),
    }
    Ok(())
}

fn merge_subtree(tree: &mut FileTree, dest: crate::tree::NodeId, sub: FileTree) {
    fn walk_copy(dest: &mut FileTree, src: &FileTree, id: crate::tree::NodeId) {
        let n = &src.nodes[id as usize];
        if !n.alive {
            return;
        }
        let path = src.path_of(id);
        match &n.kind {
            StoredKind::Dir => {
                dest.ensure_dir(&path);
                for &c in &n.children {
                    walk_copy(dest, src, c);
                }
            }
            StoredKind::File { ext } => {
                dest.upsert_file(&path, n.size, ext.clone());
            }
            StoredKind::MountPoint { .. } => {
                if let Some(parent) = path.parent() {
                    if let Some(pid) = dest.ensure_dir(parent) {
                        dest.add_child(pid, n.name.clone(), n.size, n.kind.clone(), Some(path));
                    }
                }
            }
        }
    }

    for &c in &sub.nodes[sub.root as usize].children {
        walk_copy(tree, &sub, c);
    }
    let _ = dest;
}

pub fn watch_mounts(
    mounts: Vec<std::path::PathBuf>,
    on_change: impl Fn(std::path::PathBuf),
    stop: &AtomicBool,
    reload: &AtomicBool,
) -> Result<()> {
    use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
    for m in &mounts {
        if let Err(err) = watcher.watch(m, RecursiveMode::Recursive) {
            warn!(path = %m.display(), error = %err, "watch failed; live updates limited");
        }
    }

    while !stop.load(Ordering::Relaxed) && !reload.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(Ok(event)) => match event.kind {
                EventKind::Create(_)
                | EventKind::Modify(_)
                | EventKind::Remove(_)
                | EventKind::Any => {
                    for p in event.paths {
                        on_change(p);
                    }
                }
                _ => {}
            },
            Ok(Err(err)) => warn!(error = %err, "watch error"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

pub fn apply_change(tree: &mut FileTree, path: &Path, mounts: &MountTable) {
    if mounts.is_foreign_path(&tree.root_path, path) {
        return;
    }
    let depth = relative_depth(&tree.root_path, path).unwrap_or(0);
    if depth > STORE_DEPTH {
        let leaf = cap_path(&tree.root_path, path, STORE_DEPTH);
        let _ = rescan_dir(tree, &leaf, mounts);
        return;
    }
    if path.is_dir() {
        let _ = tree.ensure_dir(path);
        let _ = rescan_dir(tree, path, mounts);
        return;
    }
    if path.is_file() {
        if let Some(parent) = path.parent() {
            if mounts.is_foreign_path(&tree.root_path, parent) {
                return;
            }
            let _ = tree.ensure_dir(parent);
        }
        if let Ok(meta) = std::fs::metadata(path) {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            tree.upsert_file(path, allocated_size(&meta), ext_of(&name));
        }
        return;
    }
    tree.remove_path(path);
}

/// Store one extra directory layer under `dir` (immediate children only).
/// Deeper files are still walked so child sizes are complete. Existing
/// children with the same name are left in place so earlier expansions
/// are not collapsed.
#[cfg(test)]
pub fn expand_one_layer(
    tree: &mut FileTree,
    dir: &Path,
    mounts: &MountTable,
    stop: &AtomicBool,
) -> Result<u32> {
    if mounts.is_nested_mount(&tree.root_path, dir) || mounts.is_foreign_path(&tree.root_path, dir)
    {
        return Ok(0);
    }
    if !dir.is_dir() {
        return Ok(0);
    }
    if tree.dirs.get(dir).is_none() {
        tree.ensure_dir_uncapped(dir);
    }
    let sub = scan_mount(dir, mounts, stop, 1, |_, _| {})?;
    Ok(graft_expand_layer(tree, dir, &sub))
}

pub fn graft_expand_layer(tree: &mut FileTree, dir: &Path, sub: &FileTree) -> u32 {
    let Some(&dest) = tree.dirs.get(dir) else {
        return 0;
    };
    if !matches!(tree.nodes[dest as usize].kind, StoredKind::Dir) {
        return 0;
    }
    let mut added = 0u32;
    let kids = sub.nodes[sub.root as usize].children.clone();
    for cid in kids {
        let n = &sub.nodes[cid as usize];
        if !n.alive {
            continue;
        }
        let exists = tree.nodes[dest as usize].children.iter().any(|&c| {
            tree.nodes[c as usize].alive && tree.nodes[c as usize].name == n.name
        });
        if exists {
            continue;
        }
        let path = sub.path_of(cid);
        let kind = n.kind.clone();
        let store_path = matches!(kind, StoredKind::Dir | StoredKind::MountPoint { .. })
            .then_some(path);
        tree.graft_child(dest, n.name.clone(), n.size, kind, store_path);
        added += 1;
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn max_node_depth(tree: &FileTree, id: crate::tree::NodeId, depth: u32) -> u32 {
        let mut m = depth;
        for &c in &tree.nodes[id as usize].children {
            if tree.nodes[c as usize].alive {
                m = m.max(max_node_depth(tree, c, depth + 1));
            }
        }
        m
    }

    #[test]
    fn stores_three_layers_and_counts_deeper_bytes() {
        let root = std::env::temp_dir().join(format!("spacemap-depth-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/b/c/d")).unwrap();
        fs::write(root.join("top.bin"), vec![0u8; 1000]).unwrap();
        fs::write(root.join("a/mid.bin"), vec![0u8; 2000]).unwrap();
        fs::write(root.join("a/b/c/d/deep.bin"), vec![0u8; 4000]).unwrap();

        let tree = scan_mount(
            &root,
            &MountTable::default(),
            &AtomicBool::new(false),
            STORE_DEPTH,
            |_, _| {},
        )
        .unwrap();
        let _ = fs::remove_dir_all(&root);

        assert!(
            max_node_depth(&tree, tree.root, 0) <= STORE_DEPTH,
            "stored depth {}",
            max_node_depth(&tree, tree.root, 0)
        );
        assert!(
            tree.bytes >= 7000,
            "deep files should still count, bytes={}",
            tree.bytes
        );
        assert!(
            !tree.dirs.contains_key(&root.join("a/b/c/d")),
            "depth-4 directories must not be stored"
        );
        assert!(tree.dirs.contains_key(&root.join("a/b/c")));
    }

    #[test]
    fn expand_adds_one_layer() {
        let root = std::env::temp_dir().join(format!("spacemap-expand-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/b/c/d")).unwrap();
        fs::write(root.join("a/b/c/d/deep.bin"), vec![0u8; 4000]).unwrap();

        let mut tree = scan_mount(
            &root,
            &MountTable::default(),
            &AtomicBool::new(false),
            STORE_DEPTH,
            |_, _| {},
        )
        .unwrap();
        let leaf = root.join("a/b/c");
        assert!(!tree.dirs.contains_key(&root.join("a/b/c/d")));
        let added = expand_one_layer(
            &mut tree,
            &leaf,
            &MountTable::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(added >= 1);
        assert!(tree.dirs.contains_key(&root.join("a/b/c/d")));
        let added_d = expand_one_layer(
            &mut tree,
            &root.join("a/b/c/d"),
            &MountTable::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let _ = fs::remove_dir_all(&root);
        assert!(added_d >= 1);
        assert!(
            tree.nodes[tree.dirs[&root.join("a/b/c/d")] as usize]
                .children
                .iter()
                .any(|&c| tree.nodes[c as usize].alive
                    && tree.nodes[c as usize].name == "deep.bin"),
            "a second expand should store the next layer"
        );
    }
}

