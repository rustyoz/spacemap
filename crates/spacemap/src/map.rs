use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use spacemap_core::{
    format_bytes, squarify, Bounds, DiskView, Kind, PartitionView, ViewNode,
};

#[derive(Clone, Debug)]
pub struct Hit {
    pub disk: String,
    pub partition: String,
    pub mount: String,
    pub path: String,
    pub name: String,
    pub size: u64,
    pub kind: Kind,
}

pub struct RowResponse {
    pub hover: Option<Hit>,
    pub clicked: Option<Hit>,
    pub shift_clicked: Option<Hit>,
    pub double_clicked: Option<Hit>,
    pub gutter_clicked: bool,
}

pub fn disk_row(
    ui: &mut egui::Ui,
    disk: &DiskView,
    height: f32,
    zoom: &HashMap<String, String>,
    open: &HashSet<String>,
    selected: Option<&Hit>,
    focused: bool,
    scroll_to: bool,
) -> RowResponse {
    let width = ui.available_width();
    let (id, rect) = ui.allocate_space(Vec2::new(width, height));
    let response = ui.interact(rect, id, Sense::click());
    if focused && scroll_to {
        ui.scroll_to_rect(rect, Some(egui::Align::Center));
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, Color32::from_rgb(22, 24, 30));
    if focused {
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.4_f32, Color32::from_rgb(48, 148, 168)),
            egui::StrokeKind::Inside,
        );
    }

    let gutter = 168.0;
    let gutter_rect = Rect::from_min_max(rect.min, Pos2::new(rect.min.x + gutter, rect.max.y));
    paint_gutter(&painter, gutter_rect, disk);

    let map_rect = Rect::from_min_max(
        Pos2::new(rect.min.x + gutter + 8.0, rect.min.y + 8.0),
        Pos2::new(rect.max.x - 8.0, rect.max.y - 8.0),
    );

    let mut tiles = Vec::new();
    layout_disk(disk, map_rect, zoom, open, &mut tiles);

    let pointer = ui.ctx().pointer_interact_pos();
    let mut hover = None;
    for tile in &tiles {
        let eg = tile.rect;
        let sel = selected.is_some_and(|s| s.path == tile.hit.path);
        paint_tile(&painter, eg, tile, sel);
        if let Some(pos) = pointer {
            if eg.contains(pos) {
                hover = Some(tile.hit.clone());
            }
        }
    }

    // Labels on top so nested tiles don't hide them.
    for tile in &tiles {
        paint_label(&painter, tile.rect, tile);
    }

    let gutter_hit = pointer.is_some_and(|pos| gutter_rect.contains(pos));
    let shift = ui.ctx().input(|i| i.modifiers.shift);
    let mut clicked = None;
    let mut shift_clicked = None;
    let mut double_clicked = None;
    let mut gutter_clicked = false;
    if response.double_clicked() {
        if gutter_hit {
            gutter_clicked = true;
        } else if let Some(h) = hover.clone() {
            double_clicked = Some(h);
        }
    } else if response.clicked() {
        if gutter_hit {
            gutter_clicked = true;
        } else if shift {
            shift_clicked = hover.clone();
        } else {
            clicked = hover.clone();
        }
    }

    RowResponse {
        hover,
        clicked,
        shift_clicked,
        double_clicked,
        gutter_clicked,
    }
}

struct Tile {
    rect: Rect,
    color: Color32,
    hit: Hit,
    leaf: bool,
}

fn paint_gutter(painter: &egui::Painter, rect: Rect, disk: &DiskView) {
    let kind = if disk.removable {
        "USB"
    } else if disk.rotational {
        "HDD"
    } else {
        "SSD"
    };
    let media = if disk.transport.is_empty() {
        kind.to_string()
    } else {
        format!("{} · {}", disk.transport.to_uppercase(), kind)
    };
    let model = if disk.model.trim().is_empty() {
        disk.name.clone()
    } else {
        disk.model.trim().to_string()
    };

    let x = rect.min.x + 14.0;
    let mut y = rect.min.y + 16.0;
    painter.text(
        Pos2::new(x, y),
        egui::Align2::LEFT_TOP,
        &disk.name,
        FontId::proportional(15.0),
        Color32::from_rgb(236, 232, 223),
    );
    y += 22.0;
    painter.text(
        Pos2::new(x, y),
        egui::Align2::LEFT_TOP,
        truncate(&model, 22),
        FontId::proportional(12.0),
        Color32::from_rgb(168, 172, 180),
    );
    y += 18.0;
    painter.text(
        Pos2::new(x, y),
        egui::Align2::LEFT_TOP,
        format_bytes(disk.size),
        FontId::proportional(12.0),
        Color32::from_rgb(120, 168, 196),
    );
    y += 18.0;
    painter.text(
        Pos2::new(x, y),
        egui::Align2::LEFT_TOP,
        media,
        FontId::proportional(11.0),
        Color32::from_rgb(110, 114, 124),
    );
}

enum DiskSpan<'a> {
    Partition(&'a PartitionView),
    Unallocated { size: u64 },
}

impl DiskSpan<'_> {
    fn size(&self) -> u64 {
        match self {
            DiskSpan::Partition(p) => p.size.max(1),
            DiskSpan::Unallocated { size } => (*size).max(1),
        }
    }
}

fn disk_spans(disk: &DiskView) -> Vec<DiskSpan<'_>> {
    if disk.partitions.is_empty() {
        return vec![DiskSpan::Unallocated { size: disk.size.max(1) }];
    }
    let physical = disk.partitions.iter().any(|p| p.start > 0);
    if !physical {
        let mut spans: Vec<DiskSpan<'_>> = disk
            .partitions
            .iter()
            .map(DiskSpan::Partition)
            .collect();
        if disk.unallocated > disk.size / 200 && disk.unallocated > 8 * 1024 * 1024 {
            spans.push(DiskSpan::Unallocated {
                size: disk.unallocated,
            });
        }
        return spans;
    }

    let mut parts: Vec<&PartitionView> = disk.partitions.iter().collect();
    parts.sort_by_key(|p| (p.start, p.name.as_str()));
    let mut spans = Vec::new();
    let mut cursor = 0u64;
    for p in parts {
        if p.start > cursor {
            let gap = p.start - cursor;
            if gap > 512 * 1024 {
                spans.push(DiskSpan::Unallocated { size: gap });
            }
        }
        spans.push(DiskSpan::Partition(p));
        cursor = cursor.max(p.start.saturating_add(p.size));
    }
    if cursor < disk.size {
        let gap = disk.size - cursor;
        if gap > 512 * 1024 {
            spans.push(DiskSpan::Unallocated { size: gap });
        }
    }
    if spans.is_empty() {
        spans.push(DiskSpan::Unallocated {
            size: disk.size.max(1),
        });
    }
    spans
}

fn split_row(rect: Rect, weights: &[u64], gap: f32) -> Vec<Rect> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let total: u64 = weights.iter().sum::<u64>().max(1);
    let usable = (rect.width() - gap * n.saturating_sub(1) as f32).max(1.0);
    let min_px = 4.0;
    let mut widths: Vec<f32> = weights
        .iter()
        .map(|w| (*w as f32 / total as f32) * usable)
        .collect();
    let mut need = 0.0_f32;
    for w in &mut widths {
        if *w > 0.0 && *w < min_px {
            need += min_px - *w;
            *w = min_px;
        }
    }
    if need > 0.0 {
        let mut stealable: Vec<usize> = (0..n)
            .filter(|&i| widths[i] > min_px + 1.0)
            .collect();
        stealable.sort_by(|&a, &b| widths[b].partial_cmp(&widths[a]).unwrap_or(std::cmp::Ordering::Equal));
        for i in stealable {
            if need <= 0.0 {
                break;
            }
            let give = (widths[i] - min_px).min(need);
            widths[i] -= give;
            need -= give;
        }
    }
    let mut x = rect.min.x;
    widths
        .into_iter()
        .map(|w| {
            let r = Rect::from_min_size(Pos2::new(x, rect.min.y), Vec2::new(w.max(1.0), rect.height()));
            x += w + gap;
            r
        })
        .collect()
}

fn zoomed_mount<'a>(
    disk: &'a DiskView,
    zoom: &'a HashMap<String, String>,
) -> Option<(&'a PartitionView, &'a spacemap_core::MountView, &'a str)> {
    for part in &disk.partitions {
        for mount in &part.mounts {
            if let Some(path) = zoom.get(&mount.path) {
                if path != &mount.path {
                    return Some((part, mount, path.as_str()));
                }
            }
        }
    }
    None
}

fn layout_disk(
    disk: &DiskView,
    rect: Rect,
    zoom: &HashMap<String, String>,
    open: &HashSet<String>,
    out: &mut Vec<Tile>,
) {
    if let Some((part, mount, path)) = zoomed_mount(disk, zoom) {
        let node = find_node(&mount.tree, path).unwrap_or(&mount.tree);
        layout_tree(&disk.name, &part.name, &mount.path, node, rect, true, open, out);
        return;
    }
    let spans = disk_spans(disk);
    let weights: Vec<u64> = spans.iter().map(DiskSpan::size).collect();
    let rects = split_row(rect, &weights, 2.0);
    for (span, r) in spans.iter().zip(rects) {
        match span {
            DiskSpan::Unallocated { size } => {
                out.push(Tile {
                    rect: r,
                    color: Color32::from_rgb(32, 34, 40),
                    hit: Hit {
                        disk: disk.name.clone(),
                        partition: "unallocated".into(),
                        mount: String::new(),
                        path: format!("{}::unallocated", disk.name),
                        name: "unallocated".into(),
                        size: *size,
                        kind: Kind::Unmounted,
                    },
                    leaf: true,
                });
            }
            DiskSpan::Partition(part) => {
                layout_partition(&disk.name, part, r, zoom, open, out);
            }
        }
    }
}

fn layout_partition(
    disk: &str,
    part: &PartitionView,
    rect: Rect,
    zoom: &HashMap<String, String>,
    open: &HashSet<String>,
    out: &mut Vec<Tile>,
) {
    if part.mounts.is_empty() {
        let kind = if part.locked {
            Kind::Locked
        } else if part.fstype.eq_ignore_ascii_case("swap") {
            Kind::Swap
        } else {
            Kind::Unmounted
        };
        let (r, g, b) = spacemap_core::rgb_for_kind(&kind, &part.name);
        let label = partition_title(part);
        out.push(Tile {
            rect,
            color: Color32::from_rgb(r, g, b),
            hit: Hit {
                disk: disk.into(),
                partition: part.name.clone(),
                mount: String::new(),
                path: part.name.clone(),
                name: label,
                size: part.size,
                kind,
            },
            leaf: true,
        });
        return;
    }

    let n = part.mounts.len() as f32;
    let gap = 2.0;
    let usable = (rect.width() - gap * (n - 1.0).max(0.0)).max(1.0);
    let sizes: Vec<u64> = part.mounts.iter().map(|m| m.total.max(1)).collect();
    let total: u64 = sizes.iter().sum::<u64>().max(1);
    let mut x = rect.min.x;
    for (i, mount) in part.mounts.iter().enumerate() {
        let w = (sizes[i] as f32 / total as f32) * usable;
        let r = Rect::from_min_size(Pos2::new(x, rect.min.y), Vec2::new(w, rect.height()));
        let zoomed = zoom
            .get(&mount.path)
            .map(|p| p != &mount.path)
            .unwrap_or(false);
        let node = zoom
            .get(&mount.path)
            .and_then(|p| find_node(&mount.tree, p))
            .unwrap_or(&mount.tree);
        if zoomed || mount.available == 0 {
            layout_tree(disk, &part.name, &mount.path, node, r, true, open, out);
        } else {
            let free = mount.available.min(mount.total);
            let used = mount.total.saturating_sub(free).max(1);
            let split = split_row(r, &[used, free.max(1)], 1.5);
            layout_tree(
                disk,
                &part.name,
                &mount.path,
                node,
                split[0],
                true,
                open,
                out,
            );
            if split.len() > 1 {
                let (cr, cg, cb) = spacemap_core::rgb_for_kind(&Kind::Free, "free");
                out.push(Tile {
                    rect: split[1],
                    color: Color32::from_rgb(cr, cg, cb),
                    hit: Hit {
                        disk: disk.into(),
                        partition: part.name.clone(),
                        mount: mount.path.clone(),
                        path: format!("{}::free", mount.path),
                        name: "free".into(),
                        size: free,
                        kind: Kind::Free,
                    },
                    leaf: true,
                });
            }
        }
        x += w + gap;
    }
}

fn layout_tree(
    disk: &str,
    part: &str,
    mount: &str,
    node: &ViewNode,
    rect: Rect,
    fill: bool,
    open: &HashSet<String>,
    out: &mut Vec<Tile>,
) {
    let (r, g, b) = node.color_rgb();
    let color = Color32::from_rgb(r, g, b);
    let hit = Hit {
        disk: disk.into(),
        partition: part.into(),
        mount: mount.into(),
        path: node.path.clone(),
        name: node.name.clone(),
        size: node.size,
        kind: node.kind.clone(),
    };

    let kids: Vec<&ViewNode> = node
        .children
        .iter()
        .filter(|c| !matches!(c.kind, Kind::Free))
        .collect();

    let show_kids = (fill || open.contains(&node.path)) && !kids.is_empty();
    if !show_kids {
        out.push(Tile {
            rect,
            color,
            hit,
            leaf: true,
        });
        return;
    }

    out.push(Tile {
        rect,
        color: Color32::from_rgb(18, 20, 24),
        hit: hit.clone(),
        leaf: false,
    });

    let inner = rect.shrink(1.2);
    let weights: Vec<u64> = kids.iter().map(|c| c.size.max(1)).collect();
    let bounds = Bounds::new(inner.min.x, inner.min.y, inner.width(), inner.height());
    let rects = squarify(&weights, bounds);
    for (child, b) in kids.iter().copied().zip(rects) {
        if b.w < 1.0 || b.h < 1.0 {
            continue;
        }
        let cr = Rect::from_min_size(Pos2::new(b.x, b.y), Vec2::new(b.w, b.h)).shrink(0.6);
        layout_tree(disk, part, mount, child, cr, false, open, out);
    }
}

fn paint_tile(painter: &egui::Painter, rect: Rect, tile: &Tile, selected: bool) {
    painter.rect_filled(rect, 2.0, tile.color);
    if matches!(tile.hit.kind, Kind::Free | Kind::Scanning) {
        hatch(painter, rect);
    }
    if matches!(tile.hit.kind, Kind::MountPoint { .. }) {
        hatch(painter, rect);
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.2_f32, Color32::from_rgb(120, 210, 220)),
            egui::StrokeKind::Inside,
        );
    }
    if selected {
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.6_f32, Color32::from_rgb(240, 232, 210)),
            egui::StrokeKind::Inside,
        );
    } else if tile.leaf {
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(0.6_f32, Color32::from_rgba_unmultiplied(0, 0, 0, 70)),
            egui::StrokeKind::Inside,
        );
    }
}

fn hatch(painter: &egui::Painter, rect: Rect) {
    let clip = painter.with_clip_rect(rect);
    let stroke = Stroke::new(1.0_f32, Color32::from_rgb(58, 64, 74));
    let mut x = rect.min.x - rect.height();
    while x < rect.max.x {
        clip.line_segment(
            [
                Pos2::new(x, rect.min.y),
                Pos2::new(x + rect.height(), rect.max.y),
            ],
            stroke,
        );
        x += 8.0;
    }
}

fn paint_label(painter: &egui::Painter, rect: Rect, tile: &Tile) {
    if !tile.leaf || rect.width() < 46.0 || rect.height() < 16.0 {
        return;
    }
    let lum = tile.color.r() as u16 + tile.color.g() as u16 + tile.color.b() as u16;
    let fg = if lum > 340 {
        Color32::from_rgb(22, 22, 24)
    } else {
        Color32::from_rgb(236, 236, 238)
    };
    let name = if tile.hit.name.len() > 28 {
        format!("{}…", &tile.hit.name.chars().take(27).collect::<String>())
    } else {
        tile.hit.name.clone()
    };
    let size = format_bytes(tile.hit.size);
    let text = if rect.height() > 28.0 && rect.width() > 70.0 {
        format!("{name}\n{size}")
    } else {
        name
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::proportional(11.0),
        fg,
    );
}

fn partition_title(part: &PartitionView) -> String {
    let mut bits = vec![part.name.clone()];
    if let Some(l) = &part.label {
        if !l.is_empty() {
            bits.push(l.clone());
        }
    }
    if !part.fstype.is_empty() {
        bits.push(part.fstype.clone());
    }
    if part.locked {
        bits.push("locked".into());
    }
    bits.join(" · ")
}

fn find_node<'a>(node: &'a ViewNode, path: &str) -> Option<&'a ViewNode> {
    if node.path == path {
        return Some(node);
    }
    for c in &node.children {
        if let Some(n) = find_node(c, path) {
            return Some(n);
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}
