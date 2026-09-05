use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use spacemap_core::{
    format_bytes, Client, Engine, Kind, Snapshot, SnapshotSource,
};

use crate::map::{self, Hit};

struct FeedFocus {
    zoom: Vec<String>,
    open: Vec<String>,
}

pub struct SpacemapApp {
    snapshot: Arc<Mutex<Arc<Snapshot>>>,
    visible: Arc<Snapshot>,
    focus: Arc<Mutex<FeedFocus>>,
    commands: mpsc::Sender<Command>,
    hover: Option<Hit>,
    selected: Option<Hit>,
    expanded_disk: Option<String>,
    zoom: HashMap<String, String>,
    open: HashSet<String>,
    scroll_focus: bool,
    status: String,
}

enum Command {
    Rescan(Option<String>),
    Expand(String),
}

impl SpacemapApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let snapshot = Arc::new(Mutex::new(Arc::new(Snapshot::empty(SnapshotSource::Local))));
        let focus = Arc::new(Mutex::new(FeedFocus {
            zoom: Vec::new(),
            open: Vec::new(),
        }));
        let (tx, rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let snap = Arc::clone(&snapshot);
        let focus_feed = Arc::clone(&focus);
        std::thread::Builder::new()
            .name("spacemap-feed".into())
            .spawn(move || feed_loop(snap, focus_feed, rx, ctx))
            .ok();

        Self {
            visible: Arc::new(Snapshot::empty(SnapshotSource::Local)),
            snapshot,
            focus,
            commands: tx,
            hover: None,
            selected: None,
            expanded_disk: None,
            zoom: HashMap::new(),
            open: HashSet::new(),
            scroll_focus: false,
            status: "starting…".into(),
        }
    }
}

impl eframe::App for SpacemapApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(400));
        if let Ok(mut paths) = self.focus.try_lock() {
            paths.zoom = self.zoom.values().cloned().collect();
            paths.open = self.open.iter().cloned().collect();
        }
        if let Ok(g) = self.snapshot.try_lock() {
            self.visible = Arc::clone(&g);
        }
        let snap = Arc::clone(&self.visible);

        self.status = match snap.source {
            SnapshotSource::Daemon => "daemon".into(),
            SnapshotSource::Local => "local scan".into(),
        };

        egui::TopBottomPanel::top("top").exact_height(42.0).show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("Spacemap")
                        .size(18.0)
                        .color(egui::Color32::from_rgb(236, 232, 223)),
                );
                ui.add_space(12.0);
                let n = snap.disks.len();
                let cap: u64 = snap.disks.iter().map(|d| d.size).sum();
                ui.label(
                    egui::RichText::new(format!(
                        "{n} disks  ·  {}  ·  {}",
                        format_bytes(cap),
                        self.status
                    ))
                    .size(13.0)
                    .color(egui::Color32::from_rgb(150, 154, 164)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(10.0);
                    if ui
                        .add(egui::Button::new("Rescan all").fill(egui::Color32::from_rgb(42, 52, 68)))
                        .clicked()
                    {
                        let _ = self.commands.send(Command::Rescan(None));
                    }
                    if let Some(sel) = &self.selected {
                        if !sel.path.contains("::")
                            && ui
                                .add(
                                    egui::Button::new("Open")
                                        .fill(egui::Color32::from_rgb(42, 52, 68)),
                                )
                                .clicked()
                        {
                            open_path(&sel.path);
                        }
                    }
                });
            });
        });

        egui::TopBottomPanel::bottom("bottom")
            .exact_height(36.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(10.0);
                    let hit = self.hover.as_ref().or(self.selected.as_ref());
                    if let Some(h) = hit {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}  ·  {}  ·  {}   {}   {}",
                                h.disk,
                                h.partition,
                                h.path,
                                format_bytes(h.size),
                                kind_label(&h.kind)
                            ))
                            .size(13.0)
                            .color(egui::Color32::from_rgb(210, 214, 220)),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(
                                "Click a folder to open it in place · Shift-click fills the window · Esc collapses",
                            )
                            .size(13.0)
                            .color(egui::Color32::from_rgb(120, 124, 134)),
                        );
                    }
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if snap.disks.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("No disks found.");
                });
                return;
            }

            let avail = ui.available_size();
            let n = snap.disks.len() as f32;
            let compact_h = 64.0;
            let gap = 6.0;

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let mut hover = None;
                    let mut clicked = None;
                    let mut shift_clicked = None;
                    let mut dbl = None;
                    let mut gutter_disk = None;
                    for disk in &snap.disks {
                        let row_h = if let Some(exp) = &self.expanded_disk {
                            if *exp == disk.name {
                                (avail.y - compact_h * (n - 1.0) - gap * n).clamp(220.0, avail.y)
                            } else {
                                compact_h
                            }
                        } else {
                            ((avail.y - 8.0) / n).clamp(96.0, 168.0)
                        };
                        let response = map::disk_row(
                            ui,
                            disk,
                            row_h,
                            &self.zoom,
                            &self.open,
                            self.selected.as_ref(),
                            self.expanded_disk.as_deref() == Some(disk.name.as_str()),
                            self.scroll_focus
                                && self.expanded_disk.as_deref() == Some(disk.name.as_str()),
                        );
                        if let Some(h) = response.hover {
                            hover = Some(h);
                        }
                        if let Some(h) = response.clicked {
                            clicked = Some(h);
                        }
                        if let Some(h) = response.shift_clicked {
                            shift_clicked = Some(h);
                        }
                        if let Some(h) = response.double_clicked {
                            dbl = Some(h);
                        }
                        if response.gutter_clicked {
                            gutter_disk = Some(disk);
                        }
                    }
                    self.hover = hover;
                    if let Some(disk) = gutter_disk {
                        self.expand_disk(disk);
                    }
                    if let Some(h) = clicked {
                        self.on_tile_click(h, false);
                    }
                    if let Some(h) = shift_clicked {
                        self.on_tile_click(h, true);
                    }
                    if let Some(h) = dbl {
                        self.on_tile_click(h, false);
                    }
                    self.scroll_focus = false;
                });
        });

        ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                self.on_escape();
            }
            if i.key_pressed(egui::Key::R) && i.modifiers.ctrl {
                let _ = self.commands.send(Command::Rescan(None));
            }
        });
    }
}

impl SpacemapApp {
    fn expand_disk(&mut self, disk: &spacemap_core::DiskView) {
        self.expanded_disk = Some(disk.name.clone());
        self.scroll_focus = true;
        self.selected = Some(Hit {
            disk: disk.name.clone(),
            partition: String::new(),
            mount: String::new(),
            path: format!("{}::disk", disk.name),
            name: if disk.model.trim().is_empty() {
                disk.name.clone()
            } else {
                disk.model.trim().to_string()
            },
            size: disk.size,
            kind: Kind::Other,
        });
    }

    fn on_tile_click(&mut self, h: Hit, fill_window: bool) {
        if self.jump_to_mount(&h) {
            return;
        }
        if matches!(h.kind, Kind::Directory) && !h.path.contains("::") && !h.mount.is_empty() {
            let _ = self.commands.send(Command::Expand(h.path.clone()));
            if fill_window {
                self.zoom.insert(h.mount.clone(), h.path.clone());
                self.expanded_disk = Some(h.disk.clone());
                self.scroll_focus = true;
            } else {
                self.open.insert(h.path.clone());
            }
        } else if fill_window {
            self.expanded_disk = Some(h.disk.clone());
            self.scroll_focus = true;
        }
        self.selected = Some(h);
    }

    fn on_escape(&mut self) {
        if let Some(sel) = self.selected.clone() {
            if self.zoom.contains_key(&sel.mount) && self.zoom_out(&sel.mount, &sel.path) {
                return;
            }
            if self.collapse_open(&sel.mount, &sel.path) {
                return;
            }
        }
        if self.expanded_disk.take().is_some() {
            self.zoom.clear();
            self.selected = None;
            return;
        }
        self.selected = None;
        self.zoom.clear();
        self.open.clear();
    }

    fn collapse_open(&mut self, mount: &str, path: &str) -> bool {
        if mount.is_empty() || path.contains("::") {
            return false;
        }
        let path_p = Path::new(path);
        let mut best: Option<String> = None;
        for p in &self.open {
            let pp = Path::new(p);
            if path_p == pp || path_p.starts_with(pp) {
                if best.as_ref().is_none_or(|b| p.len() >= b.len()) {
                    best = Some(p.clone());
                }
            }
        }
        let Some(target) = best else {
            return false;
        };
        let target_p = Path::new(&target);
        self.open
            .retain(|p| p != &target && !Path::new(p).starts_with(target_p));
        self.selected = Some(Hit {
            disk: self.selected.as_ref().map(|s| s.disk.clone()).unwrap_or_default(),
            partition: self
                .selected
                .as_ref()
                .map(|s| s.partition.clone())
                .unwrap_or_default(),
            mount: mount.into(),
            path: target.clone(),
            name: Path::new(&target)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(target),
            size: self.selected.as_ref().map(|s| s.size).unwrap_or(0),
            kind: Kind::Directory,
        });
        true
    }

    fn zoom_out(&mut self, mount: &str, path: &str) -> bool {
        if mount.is_empty() {
            return false;
        }
        let current = self
            .zoom
            .get(mount)
            .cloned()
            .filter(|z| path == z || path.starts_with(z) || z.starts_with(path))
            .unwrap_or_else(|| path.to_string());
        if current == mount || !current.starts_with(mount) {
            if self.zoom.remove(mount).is_some() {
                self.selected = Some(Hit {
                    disk: self.selected.as_ref().map(|s| s.disk.clone()).unwrap_or_default(),
                    partition: self
                        .selected
                        .as_ref()
                        .map(|s| s.partition.clone())
                        .unwrap_or_default(),
                    mount: mount.into(),
                    path: mount.into(),
                    name: Path::new(mount)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| mount.to_string()),
                    size: self.selected.as_ref().map(|s| s.size).unwrap_or(0),
                    kind: Kind::Directory,
                });
                return true;
            }
            return false;
        }
        let parent = Path::new(&current).parent().map(|p| p.to_string_lossy().into_owned());
        match parent {
            Some(p) if p == mount || p.is_empty() => {
                self.zoom.remove(mount);
                self.selected = Some(Hit {
                    disk: self.selected.as_ref().map(|s| s.disk.clone()).unwrap_or_default(),
                    partition: self
                        .selected
                        .as_ref()
                        .map(|s| s.partition.clone())
                        .unwrap_or_default(),
                    mount: mount.into(),
                    path: current,
                    name: Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string()),
                    size: self.selected.as_ref().map(|s| s.size).unwrap_or(0),
                    kind: Kind::Directory,
                });
                true
            }
            Some(p) => {
                self.zoom.insert(mount.to_string(), p.clone());
                self.selected = Some(Hit {
                    disk: self.selected.as_ref().map(|s| s.disk.clone()).unwrap_or_default(),
                    partition: self
                        .selected
                        .as_ref()
                        .map(|s| s.partition.clone())
                        .unwrap_or_default(),
                    mount: mount.into(),
                    path: p.clone(),
                    name: Path::new(&p)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(p),
                    size: self.selected.as_ref().map(|s| s.size).unwrap_or(0),
                    kind: Kind::Directory,
                });
                true
            }
            None => false,
        }
    }

    fn jump_to_mount(&mut self, h: &Hit) -> bool {
        let Kind::MountPoint {
            disk,
            partition,
            mount,
            model: _,
        } = &h.kind
        else {
            return false;
        };
        self.expanded_disk = Some(disk.clone());
        self.scroll_focus = true;
        self.zoom.remove(mount);
        self.zoom.remove(&h.mount);
        self.selected = Some(Hit {
            disk: disk.clone(),
            partition: partition.clone(),
            mount: mount.clone(),
            path: mount.clone(),
            name: h.name.clone(),
            size: h.size,
            kind: h.kind.clone(),
        });
        true
    }
}

fn kind_label(kind: &Kind) -> String {
    match kind {
        Kind::Directory => "directory".into(),
        Kind::File { .. } => "file".into(),
        Kind::Free => "free space".into(),
        Kind::Overhead => "filesystem overhead".into(),
        Kind::Scanning => "scanning".into(),
        Kind::Other => "other".into(),
        Kind::Unmounted => "unmounted".into(),
        Kind::Locked => "locked".into(),
        Kind::Swap => "swap".into(),
        Kind::MountPoint {
            disk, partition, ..
        } => format!("mount → {disk} {partition}"),
    }
}

fn open_path(path: &str) {
    let p = path.trim();
    if p.is_empty() || p.contains("::") {
        return;
    }
    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
}

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = egui::Color32::from_rgb(16, 18, 22);
    style.visuals.window_fill = egui::Color32::from_rgb(16, 18, 22);
    style.visuals.override_text_color = Some(egui::Color32::from_rgb(214, 216, 220));
    style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(36, 42, 52);
    style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(52, 62, 78);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    ctx.set_style(style);
}

fn feed_loop(
    snapshot: Arc<Mutex<Arc<Snapshot>>>,
    focus: Arc<Mutex<FeedFocus>>,
    commands: mpsc::Receiver<Command>,
    ctx: egui::Context,
) {
    loop {
        match Client::connect() {
            Ok(mut client) => {
                tracing::info!("connected to spacemapd");
                loop {
                    while let Ok(cmd) = commands.try_recv() {
                        match cmd {
                            Command::Rescan(mount) => {
                                let _ = client.rescan(mount);
                            }
                            Command::Expand(path) => {
                                let _ = client.expand(path);
                            }
                        }
                    }
                    let (focus_paths, open_paths) = focus
                        .lock()
                        .ok()
                        .map(|g| (g.zoom.clone(), g.open.clone()))
                        .unwrap_or_default();
                    match client.snapshot(0.002, &focus_paths, &open_paths) {
                        Ok(s) => {
                            if let Ok(mut g) = snapshot.lock() {
                                *g = Arc::new(s);
                            }
                            ctx.request_repaint();
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "daemon snapshot failed");
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(400));
                }
            }
            Err(_) => {
                tracing::info!("no daemon; scanning in-process");
                let Ok(engine) = Engine::new(SnapshotSource::Local) else {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                };
                let runner = Arc::clone(&engine);
                std::thread::spawn(move || runner.run());
                loop {
                    while let Ok(cmd) = commands.try_recv() {
                        match cmd {
                            Command::Rescan(mount) => {
                                engine.request_rescan(mount.map(std::path::PathBuf::from));
                            }
                            Command::Expand(path) => {
                                engine.request_expand(std::path::PathBuf::from(path));
                            }
                        }
                    }
                    let (focus_paths, open_paths) = focus
                        .lock()
                        .ok()
                        .map(|g| (g.zoom.clone(), g.open.clone()))
                        .unwrap_or_default();
                    if let Ok(mut g) = snapshot.lock() {
                        *g = Arc::new(engine.snapshot(0.002, &focus_paths, &open_paths));
                    }
                    ctx.request_repaint();
                    std::thread::sleep(Duration::from_millis(400));
                    if Client::connect().is_ok() {
                        engine.stop();
                        break;
                    }
                }
            }
        }
    }
}
