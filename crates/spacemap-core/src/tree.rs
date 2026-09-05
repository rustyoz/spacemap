use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::snapshot::{Kind, ViewNode};

pub type NodeId = u32;

/// Directory levels kept under each mount root. Deeper files are walked for
/// size but not stored as nodes.
pub const STORE_DEPTH: u32 = 3;

pub fn relative_depth(root: &Path, path: &Path) -> Option<u32> {
    let rel = path.strip_prefix(root).ok()?;
    if rel.as_os_str().is_empty() {
        return Some(0);
    }
    Some(rel.components().count() as u32)
}

pub fn cap_path(root: &Path, path: &Path, max_depth: u32) -> PathBuf {
    let Ok(rel) = path.strip_prefix(root) else {
        return path.to_path_buf();
    };
    let mut out = root.to_path_buf();
    for (i, c) in rel.components().enumerate() {
        if i as u32 >= max_depth {
            break;
        }
        out.push(c);
    }
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub parent: Option<NodeId>,
    pub size: u64,
    pub kind: StoredKind,
    pub children: Vec<NodeId>,
    pub alive: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StoredKind {
    Dir,
    File { ext: String },
    MountPoint {
        disk: String,
        partition: String,
        mount: String,
        model: String,
        volume_used: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileTree {
    pub root_path: PathBuf,
    pub nodes: Vec<Node>,
    pub root: NodeId,
    pub files: u64,
    pub bytes: u64,
    pub store_depth: u32,
    #[serde(skip)]
    pub dirs: HashMap<PathBuf, NodeId>,
}

impl FileTree {
    pub fn new(root_path: PathBuf) -> Self {
        let root_name = root_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| root_path.to_string_lossy().into_owned());
        let root = Node {
            name: root_name,
            parent: None,
            size: 0,
            kind: StoredKind::Dir,
            children: Vec::new(),
            alive: true,
        };
        let mut dirs = HashMap::new();
        dirs.insert(root_path.clone(), 0);
        Self {
            root_path,
            nodes: vec![root],
            root: 0,
            files: 0,
            bytes: 0,
            store_depth: STORE_DEPTH,
            dirs,
        }
    }

    pub fn rebuild_index(&mut self) {
        self.dirs.clear();
        self.walk_index(self.root, self.root_path.clone());
    }

    fn walk_index(&mut self, id: NodeId, path: PathBuf) {
        let (alive, is_dir, names) = {
            let n = &self.nodes[id as usize];
            (
                n.alive,
                matches!(n.kind, StoredKind::Dir | StoredKind::MountPoint { .. }),
                n.children
                    .iter()
                    .map(|&c| (c, self.nodes[c as usize].name.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        if !alive {
            return;
        }
        if is_dir {
            self.dirs.insert(path.clone(), id);
        }
        for (child, name) in names {
            self.walk_index(child, path.join(name));
        }
    }

    pub fn add_child(
        &mut self,
        parent: NodeId,
        name: String,
        size: u64,
        kind: StoredKind,
        path: Option<PathBuf>,
    ) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(Node {
            name,
            parent: Some(parent),
            size,
            kind: kind.clone(),
            children: Vec::new(),
            alive: true,
        });
        self.nodes[parent as usize].children.push(id);
        self.add_size(parent, size);
        match &kind {
            StoredKind::Dir | StoredKind::MountPoint { .. } => {
                if let Some(p) = path {
                    self.dirs.insert(p, id);
                }
            }
            StoredKind::File { .. } => {
                self.files += 1;
            }
        }
        self.bytes = self.nodes[self.root as usize].size;
        id
    }

    pub(crate) fn add_size(&mut self, mut id: NodeId, delta: u64) {
        loop {
            let node = &mut self.nodes[id as usize];
            node.size = node.size.saturating_add(delta);
            match node.parent {
                Some(p) => id = p,
                None => break,
            }
        }
    }

    pub(crate) fn sub_size(&mut self, mut id: NodeId, delta: u64) {
        loop {
            let node = &mut self.nodes[id as usize];
            node.size = node.size.saturating_sub(delta);
            match node.parent {
                Some(p) => id = p,
                None => break,
            }
        }
    }

    pub fn remove_path(&mut self, path: &Path) {
        if let Some(&id) = self.dirs.get(path) {
            self.kill(id);
            return;
        }
        if let Some(parent_path) = path.parent() {
            if let Some(&pid) = self.dirs.get(parent_path) {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let child = self.nodes[pid as usize]
                    .children
                    .iter()
                    .copied()
                    .find(|&c| self.nodes[c as usize].alive && self.nodes[c as usize].name == name);
                if let Some(cid) = child {
                    self.kill(cid);
                }
            }
        }
    }

    fn kill(&mut self, id: NodeId) {
        if id == self.root {
            return;
        }
        let size = self.nodes[id as usize].size;
        let parent = self.nodes[id as usize].parent;
        self.kill_recursive(id);
        if let Some(p) = parent {
            self.nodes[p as usize].children.retain(|&c| c != id);
            self.sub_size(p, size);
        }
        self.bytes = self.nodes[self.root as usize].size;
    }

    fn kill_recursive(&mut self, id: NodeId) {
        let children = self.nodes[id as usize].children.clone();
        for c in children {
            self.kill_recursive(c);
        }
        let path = self.path_of(id);
        self.dirs.remove(&path);
        let node = &mut self.nodes[id as usize];
        if matches!(node.kind, StoredKind::File { .. }) && node.alive {
            self.files = self.files.saturating_sub(1);
        }
        node.alive = false;
        node.children.clear();
        node.size = 0;
    }

    pub fn path_of(&self, id: NodeId) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            let n = &self.nodes[c as usize];
            if n.parent.is_some() {
                parts.push(n.name.clone());
            }
            cur = n.parent;
        }
        parts.reverse();
        let mut p = self.root_path.clone();
        for part in parts {
            p.push(part);
        }
        p
    }

    pub fn upsert_file(&mut self, path: &Path, size: u64, ext: String) {
        let Some(parent_path) = path.parent() else { return };
        let Some(&pid) = self.dirs.get(parent_path) else { return };
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(cid) = self.nodes[pid as usize]
            .children
            .iter()
            .copied()
            .find(|&c| self.nodes[c as usize].alive && self.nodes[c as usize].name == name)
        {
            let old = self.nodes[cid as usize].size;
            if old > size {
                self.sub_size(cid, old - size);
            } else if size > old {
                self.add_size(cid, size - old);
            }
            self.nodes[cid as usize].size = size;
            self.bytes = self.nodes[self.root as usize].size;
            return;
        }
        self.add_child(pid, name, size, StoredKind::File { ext }, None);
    }

    pub fn ensure_dir(&mut self, path: &Path) -> Option<NodeId> {
        let path = cap_path(&self.root_path, path, STORE_DEPTH);
        self.ensure_dir_uncapped(&path)
    }

    pub fn ensure_dir_uncapped(&mut self, path: &Path) -> Option<NodeId> {
        if let Some(&id) = self.dirs.get(path) {
            return Some(id);
        }
        if path == self.root_path {
            return Some(self.root);
        }
        let parent = path.parent()?;
        let pid = self.ensure_dir_uncapped(parent)?;
        let name = path.file_name()?.to_string_lossy().into_owned();
        Some(self.add_child(
            pid,
            name,
            0,
            StoredKind::Dir,
            Some(path.to_path_buf()),
        ))
    }

    /// Drop nodes below [`STORE_DEPTH`], keeping recursive sizes on the leaves.
    pub fn prune_to_store_depth(&mut self) {
        if self.max_depth() <= STORE_DEPTH {
            self.store_depth = STORE_DEPTH;
            return;
        }
        let mut next = FileTree::new(self.root_path.clone());
        next.nodes[next.root as usize].name = self.nodes[self.root as usize].name.clone();
        next.nodes[next.root as usize].size = self.nodes[self.root as usize].size;
        let src_root = self.root;
        let dest_root = next.root;
        copy_capped(self, &mut next, src_root, dest_root, 0);
        next.bytes = next.nodes[next.root as usize].size;
        next.files = next
            .nodes
            .iter()
            .filter(|n| n.alive && matches!(n.kind, StoredKind::File { .. }))
            .count() as u64;
        next.rebuild_index();
        *self = next;
    }

    fn max_depth(&self) -> u32 {
        fn walk(tree: &FileTree, id: NodeId, depth: u32, max: &mut u32) {
            *max = (*max).max(depth);
            for &c in &tree.nodes[id as usize].children {
                if tree.nodes[c as usize].alive {
                    walk(tree, c, depth + 1, max);
                }
            }
        }
        let mut max = 0;
        walk(self, self.root, 0, &mut max);
        max
    }

    pub(crate) fn graft_child(
        &mut self,
        parent: NodeId,
        name: String,
        size: u64,
        kind: StoredKind,
        path: Option<PathBuf>,
    ) -> NodeId {
        graft(self, parent, name, size, kind, path)
    }

    #[allow(dead_code)]
    pub fn view(&self, min_size: u64) -> ViewNode {
        self.view_layer(None, &[], min_size)
    }

    pub fn view_layer(&self, at: Option<&Path>, open: &[PathBuf], min_size: u64) -> ViewNode {
        let id = at
            .and_then(|p| self.dirs.get(p).copied())
            .unwrap_or(self.root);
        self.view_node(id, min_size, 0, 1, open)
    }

    fn portal_display_size(&self, volume_used: u64, min_size: u64) -> u64 {
        let tree = self.bytes.max(1);
        let floor = min_size.max(tree / 40).max(4 * 1024 * 1024);
        let cap = (tree / 8).max(floor);
        volume_used.min(cap).max(floor)
    }

    fn view_node(
        &self,
        id: NodeId,
        min_size: u64,
        depth: u32,
        max_depth: u32,
        open: &[PathBuf],
    ) -> ViewNode {
        let node = &self.nodes[id as usize];
        let path = self.path_of(id);
        match &node.kind {
            StoredKind::MountPoint {
                disk,
                partition,
                mount,
                model,
                volume_used,
            } => {
                let label = if !model.trim().is_empty() {
                    format!("→ {model}")
                } else {
                    format!("→ {disk}")
                };
                return ViewNode {
                    path: path.to_string_lossy().into_owned(),
                    name: label,
                    size: self.portal_display_size(*volume_used, min_size),
                    kind: Kind::MountPoint {
                        disk: disk.clone(),
                        partition: partition.clone(),
                        mount: mount.clone(),
                        model: model.clone(),
                    },
                    children: Vec::new(),
                };
            }
            StoredKind::Dir => {}
            StoredKind::File { ext } => {
                return ViewNode {
                    path: path.to_string_lossy().into_owned(),
                    name: node.name.clone(),
                    size: node.size,
                    kind: Kind::File { ext: ext.clone() },
                    children: Vec::new(),
                };
            }
        }

        let mut kids: Vec<(u64, NodeId, bool, bool)> = node
            .children
            .iter()
            .copied()
            .filter(|&c| self.nodes[c as usize].alive)
            .map(|c| {
                let n = &self.nodes[c as usize];
                let portal = matches!(n.kind, StoredKind::MountPoint { .. });
                let child_path = self.path_of(c);
                let needed = open.iter().any(|p| p == &child_path || p.starts_with(&child_path));
                (n.size, c, portal, needed)
            })
            .collect();
        if kids.is_empty() {
            return ViewNode {
                path: path.to_string_lossy().into_owned(),
                name: node.name.clone(),
                size: node.size,
                kind: Kind::Directory,
                children: Vec::new(),
            };
        }
        kids.sort_by(|a, b| match (a.2 || a.3, b.2 || b.3) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b.0.cmp(&a.0),
        });

        let expand = depth < max_depth || open.iter().any(|p| p == &path || p.starts_with(&path));
        let mut children = Vec::new();
        let mut shown = 0u64;
        let mut layout_sum = 0u64;
        if expand {
            for (sz, cid, portal, needed) in kids.iter().copied() {
                if !portal && !needed && children.len() >= 80 {
                    break;
                }
                if !portal && !needed && sz < min_size && children.len() >= 8 {
                    continue;
                }
                let child_min = (sz / 100).max(min_size / 4).max(16 * 1024);
                let child = self.view_node(cid, child_min, depth + 1, max_depth, open);
                layout_sum = layout_sum.saturating_add(child.size);
                if !portal {
                    shown = shown.saturating_add(sz);
                }
                children.push(child);
            }
            let rest = node.size.saturating_sub(shown);
            if rest > min_size / 2 && rest > 64 * 1024 {
                children.push(ViewNode {
                    path: path.join("other").to_string_lossy().into_owned(),
                    name: "other".into(),
                    size: rest,
                    kind: Kind::Other,
                    children: Vec::new(),
                });
                layout_sum = layout_sum.saturating_add(rest);
            }
        }
        ViewNode {
            path: path.to_string_lossy().into_owned(),
            name: node.name.clone(),
            size: node.size.max(layout_sum),
            kind: Kind::Directory,
            children,
        }
    }
}

fn graft(
    dest: &mut FileTree,
    parent: NodeId,
    name: String,
    size: u64,
    kind: StoredKind,
    path: Option<PathBuf>,
) -> NodeId {
    let id = dest.nodes.len() as NodeId;
    dest.nodes.push(Node {
        name,
        parent: Some(parent),
        size,
        kind: kind.clone(),
        children: Vec::new(),
        alive: true,
    });
    dest.nodes[parent as usize].children.push(id);
    match &kind {
        StoredKind::Dir | StoredKind::MountPoint { .. } => {
            if let Some(p) = path {
                dest.dirs.insert(p, id);
            }
        }
        StoredKind::File { .. } => {
            dest.files += 1;
        }
    }
    id
}

fn copy_capped(
    src: &FileTree,
    dest: &mut FileTree,
    src_id: NodeId,
    dest_id: NodeId,
    dest_depth: u32,
) {
    if dest_depth >= STORE_DEPTH {
        return;
    }
    for &cid in &src.nodes[src_id as usize].children {
        let n = &src.nodes[cid as usize];
        if !n.alive {
            continue;
        }
        let path = src.path_of(cid);
        let new_id = graft(
            dest,
            dest_id,
            n.name.clone(),
            n.size,
            n.kind.clone(),
            matches!(n.kind, StoredKind::Dir | StoredKind::MountPoint { .. })
                .then_some(path),
        );
        if matches!(n.kind, StoredKind::Dir) {
            copy_capped(src, dest, cid, new_id, dest_depth + 1);
        }
    }
}

pub fn ext_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_path_stops_at_store_depth() {
        let root = PathBuf::from("/mnt/disk");
        let deep = root.join("a/b/c/d/e.txt");
        assert_eq!(cap_path(&root, &deep, STORE_DEPTH), root.join("a/b/c"));
        assert_eq!(relative_depth(&root, &deep), Some(5));
        assert_eq!(relative_depth(&root, &root), Some(0));
    }

    #[test]
    fn view_layer_opens_nested_folder_in_place() {
        let root = PathBuf::from("/mnt/disk");
        let mut tree = FileTree::new(root.clone());
        let a = tree.add_child(
            tree.root,
            "a".into(),
            100,
            StoredKind::Dir,
            Some(root.join("a")),
        );
        tree.add_child(
            a,
            "b".into(),
            80,
            StoredKind::Dir,
            Some(root.join("a/b")),
        );
        tree.add_child(
            tree.root,
            "c".into(),
            20,
            StoredKind::Dir,
            Some(root.join("c")),
        );

        let closed = tree.view_layer(None, &[], 1);
        assert_eq!(closed.children.len(), 2);
        assert!(closed.children.iter().all(|c| c.children.is_empty()));

        let opened = tree.view_layer(None, &[root.join("a")], 1);
        let a_view = opened
            .children
            .iter()
            .find(|c| c.name == "a")
            .expect("a");
        assert!(
            a_view.children.iter().any(|c| c.name == "b"),
            "opened folder should show its children: {a_view:?}"
        );
        let c_view = opened.children.iter().find(|c| c.name == "c").expect("c");
        assert!(c_view.children.is_empty(), "unopened sibling stays a leaf");
    }
}

