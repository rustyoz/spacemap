#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Bounds {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w: w.max(0.0), h: h.max(0.0) }
    }

    pub fn area(self) -> f32 {
        self.w * self.h
    }

    pub fn inset(self, pad: f32) -> Self {
        Self::new(
            self.x + pad,
            self.y + pad,
            self.w - pad * 2.0,
            self.h - pad * 2.0,
        )
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// Squarified treemap (Bruls, Huizing, van Wijk).
pub fn squarify(weights: &[u64], bounds: Bounds) -> Vec<Bounds> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let total: f64 = weights.iter().map(|&w| w as f64).sum();
    if total <= 0.0 || bounds.w <= 0.5 || bounds.h <= 0.5 {
        return vec![Bounds::new(bounds.x, bounds.y, 0.0, 0.0); n];
    }

    let mut items: Vec<(usize, f64)> = weights
        .iter()
        .enumerate()
        .map(|(i, &w)| (i, (w as f64 / total) * bounds.area() as f64))
        .collect();
    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = vec![Bounds::new(0.0, 0.0, 0.0, 0.0); n];
    layout(&items, bounds, &mut out);
    out
}

fn layout(items: &[(usize, f64)], mut rect: Bounds, out: &mut [Bounds]) {
    if items.is_empty() {
        return;
    }
    if items.len() == 1 {
        out[items[0].0] = rect;
        return;
    }

    let mut row = 0usize;
    let mut best = f64::INFINITY;
    let length = rect.w.min(rect.h) as f64;

    for i in 1..=items.len() {
        let w = worst(&items[..i], length);
        if w <= best {
            best = w;
            row = i;
        } else {
            break;
        }
    }

    let row_items = &items[..row];
    let rest = &items[row..];
    let row_area: f64 = row_items.iter().map(|(_, a)| *a).sum();
    let vertical = rect.w >= rect.h;

    if vertical {
        let width = (row_area / rect.h as f64) as f32;
        let mut y = rect.y;
        let leftover_h = rect.h;
        let mut used = 0.0f32;
        for (k, (idx, area)) in row_items.iter().enumerate() {
            let h = if k + 1 == row_items.len() {
                leftover_h - used
            } else {
                (*area as f32 / width.max(0.0001)).min(leftover_h - used)
            };
            out[*idx] = Bounds::new(rect.x, y, width, h);
            y += h;
            used += h;
        }
        rect.x += width;
        rect.w = (rect.w - width).max(0.0);
    } else {
        let height = (row_area / rect.w as f64) as f32;
        let mut x = rect.x;
        let leftover_w = rect.w;
        let mut used = 0.0f32;
        for (k, (idx, area)) in row_items.iter().enumerate() {
            let w = if k + 1 == row_items.len() {
                leftover_w - used
            } else {
                (*area as f32 / height.max(0.0001)).min(leftover_w - used)
            };
            out[*idx] = Bounds::new(x, rect.y, w, height);
            x += w;
            used += w;
        }
        rect.y += height;
        rect.h = (rect.h - height).max(0.0);
    }

    layout(rest, rect, out);
}

fn worst(row: &[(usize, f64)], length: f64) -> f64 {
    if row.is_empty() || length <= 0.0 {
        return f64::INFINITY;
    }
    let s: f64 = row.iter().map(|(_, a)| *a).sum();
    let mut min = f64::INFINITY;
    let mut max = 0.0f64;
    for (_, a) in row {
        min = min.min(*a);
        max = max.max(*a);
    }
    if min <= 0.0 || s <= 0.0 {
        return f64::INFINITY;
    }
    (length * length * max / (s * s)).max(s * s / (length * length * min))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_parent() {
        let bounds = Bounds::new(0.0, 0.0, 200.0, 100.0);
        let rects = squarify(&[50, 30, 20], bounds);
        assert_eq!(rects.len(), 3);
        let area: f32 = rects.iter().map(|r| r.area()).sum();
        assert!((area - 20_000.0).abs() < 2.0, "area={area}");
    }

    #[test]
    fn empty_and_zero() {
        assert!(squarify(&[], Bounds::new(0.0, 0.0, 10.0, 10.0)).is_empty());
        let r = squarify(&[0, 0], Bounds::new(0.0, 0.0, 10.0, 10.0));
        assert_eq!(r.len(), 2);
    }
}
