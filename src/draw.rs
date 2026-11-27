use std::collections::HashMap;

use anyhow::{Result, bail};
use image::RgbaImage;
use serde::Deserialize;

use crate::color::{CLEAR, Palette, Px, parse_color};

#[derive(Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Set individual pixels.
    Pixels { points: Vec<[i32; 2]>, color: String },
    /// Bresenham line (inclusive), optionally through several points. clean = pixel-perfect
    /// (drop the L-shaped elbow pixels at turns and joints).
    Line { points: Vec<[i32; 2]>, color: String, #[serde(default)] clean: bool },
    /// Smooth curve. kind 'spline' (default) passes through every point (Catmull-Rom);
    /// 'bezier' treats points as control points (3 = quadratic, 4 = cubic, 7/10/.. = chained
    /// cubics). closed + fill paints the inside with `color` (pattern ok) and the edge with
    /// `stroke` (default color; 'none' = no edge line). Edges are pixel-perfect unless clean=false.
    Curve {
        points: Vec<[f32; 2]>,
        color: String,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        closed: bool,
        #[serde(default)]
        fill: bool,
        stroke: Option<String>,
        #[serde(default = "yes")]
        clean: bool,
        #[serde(flatten)]
        pat: Pattern,
    },
    /// Copy one half onto the other, mirrored: axis 'x' (left<->right, default) or 'y'.
    /// at = mirror line in pixel-edge coordinates (default: the middle); from = 'left'/'right'
    /// or 'top'/'bottom' (default left/top).
    Mirror { axis: Option<String>, at: Option<f32>, from: Option<String> },
    /// Cel-shade along palette ramps from a light direction. Pixels within `depth` px of their
    /// region's far side (away from the light) step `steps` shades darker; with highlight > 0,
    /// pixels within that distance of the lit side step lighter. Regions end where the ramp
    /// changes (edge='ramp', default), the color changes (edge='color') or at transparency
    /// (edge='alpha'). Colors without a ramp are left alone.
    Shade {
        light: Option<[f32; 2]>,
        depth: Option<u32>,
        #[serde(default)]
        highlight: u32,
        steps: Option<i32>,
        colors: Option<Vec<String>>,
        rect: Option<[i32; 4]>,
        edge: Option<String>,
    },
    /// Step pixels along their palette ramp (negative = darker): all, those in rect, at points,
    /// and/or of the given colors.
    Ramp { steps: i32, rect: Option<[i32; 4]>, points: Option<Vec<[i32; 2]>>, colors: Option<Vec<String>> },
    /// Remove doubled corners from line work: in every 2x2 block where three line pixels form an
    /// L, the elbow pixel takes the color of the fourth cell. Line pixels are the given colors, or
    /// any opaque pixel darker than `level` (perceived lightness, default 0.3). Good after a
    /// lineart import, where outlines come out 1-2 px thick.
    Unjag { colors: Option<Vec<String>>, level: Option<f32>, passes: Option<u32> },
    /// Inner contour lines: pixels of color `of` that touch a different opaque color (or only
    /// `against`) become `color`, or with selout the darkest shade of of's ramp.
    Contour {
        of: String,
        color: Option<String>,
        against: Option<String>,
        #[serde(default)]
        selout: bool,
        #[serde(default)]
        diagonal: bool,
    },
    Rect { x: i32, y: i32, w: i32, h: i32, color: String, #[serde(default)] fill: bool, #[serde(flatten)] pat: Pattern },
    Ellipse { x: i32, y: i32, w: i32, h: i32, color: String, #[serde(default)] fill: bool, #[serde(flatten)] pat: Pattern },
    /// Filled polygon through pixel coordinates (edges included).
    Polygon { points: Vec<[i32; 2]>, color: String, #[serde(default = "yes")] fill: bool, #[serde(flatten)] pat: Pattern },
    /// Flood fill (4-connected, exact color match).
    Fill { x: i32, y: i32, color: String, #[serde(flatten)] pat: Pattern },
    /// Paint a block of pixels from text rows. Each char: palette index char
    /// (0-9a-zA-Z), a legend char, '.'/' ' = leave unchanged (or erase if
    /// erase_dots), '_' = erase.
    Rows {
        #[serde(default)]
        x: i32,
        #[serde(default)]
        y: i32,
        rows: Vec<String>,
        #[serde(default)]
        legend: HashMap<String, String>,
        #[serde(default)]
        erase_dots: bool,
    },
    Replace { from: String, to: String },
    /// Add an outline around opaque pixels (on transparent neighbors). selout: color each
    /// outline pixel with the darkest shade of the adjacent color's ramp.
    Outline { color: Option<String>, #[serde(default)] diagonal: bool, #[serde(default)] selout: bool },
    Shift { dx: i32, dy: i32, #[serde(default)] wrap: bool },
    Flip { #[serde(default)] h: bool, #[serde(default)] v: bool },
    /// Clear a rect, or everything if rect is omitted.
    Clear { rect: Option<[i32; 4]> },
    /// Copy a region [x,y,w,h] to (x,y) within the same part; transparent source pixels are skipped.
    Copy { from: [i32; 4], to: [i32; 2] },
}

fn yes() -> bool {
    true
}

/// Optional dither: pattern = "checker" | "bayer:N" (N = 0..16 density) | "hlines" | "vlines".
/// Pixels "on" get `color`, the rest get `color2` (or stay untouched).
#[derive(Deserialize, Debug, Default)]
pub struct Pattern {
    pub pattern: Option<String>,
    pub color2: Option<String>,
}

const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

struct Brush {
    c1: Px,
    c2: Option<Px>,
    pat: Option<(String, u8)>,
}

impl Brush {
    fn new(color: &str, pat: &Pattern, pal: &Palette) -> Result<Brush> {
        let c1 = parse_color(color, pal)?;
        let c2 = pat.color2.as_deref().map(|c| parse_color(c, pal)).transpose()?;
        let pat = match pat.pattern.as_deref() {
            None | Some("solid") => None,
            Some(p @ ("checker" | "hlines" | "vlines")) => Some((p.to_string(), 0)),
            Some(p) if p.starts_with("bayer:") => {
                let n: u8 = p[6..].parse().map_err(|_| anyhow::anyhow!("bad pattern '{p}'"))?;
                Some(("bayer".to_string(), n.min(16)))
            }
            Some(p) => bail!("unknown pattern '{p}' (checker, bayer:N, hlines, vlines)"),
        };
        Ok(Brush { c1, c2, pat })
    }
    fn at(&self, x: i32, y: i32) -> Option<Px> {
        let on = match &self.pat {
            None => true,
            Some((p, n)) => match p.as_str() {
                "checker" => (x + y).rem_euclid(2) == 0,
                "hlines" => y.rem_euclid(2) == 0,
                "vlines" => x.rem_euclid(2) == 0,
                _ => BAYER4[y.rem_euclid(4) as usize][x.rem_euclid(4) as usize] < *n,
            },
        };
        if on { Some(self.c1) } else { self.c2 }
    }
    fn paint(&self, img: &mut RgbaImage, x: i32, y: i32) {
        if let Some(c) = self.at(x, y) {
            put(img, x, y, c);
        }
    }
}

fn inb(img: &RgbaImage, x: i32, y: i32) -> bool {
    x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height()
}

pub fn put(img: &mut RgbaImage, x: i32, y: i32, c: Px) {
    if inb(img, x, y) {
        img.put_pixel(x as u32, y as u32, c);
    }
}

fn get(img: &RgbaImage, x: i32, y: i32) -> Option<Px> {
    inb(img, x, y).then(|| *img.get_pixel(x as u32, y as u32))
}

fn line_pts(a: [i32; 2], b: [i32; 2]) -> Vec<(i32, i32)> {
    let mut img_pts = vec![];
    let (mut x0, mut y0) = (a[0], a[1]);
    let (x1, y1) = (b[0], b[1]);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        img_pts.push((x0, y0));
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
    img_pts
}

fn polygon(img: &mut RgbaImage, pts: &[[i32; 2]], b: &Brush, fill: bool) {
    if pts.is_empty() {
        return;
    }
    let n = pts.len();
    if fill && n >= 3 {
        let miny = pts.iter().map(|p| p[1]).min().unwrap();
        let maxy = pts.iter().map(|p| p[1]).max().unwrap();
        for y in span(miny, maxy - miny + 1, img.height()) {
            let yc = y as f32 + 0.5;
            let mut xs = vec![];
            for i in 0..n {
                let (a, c) = (pts[i], pts[(i + 1) % n]);
                let (ay, cy) = (a[1] as f32 + 0.5, c[1] as f32 + 0.5);
                if (ay <= yc && cy > yc) || (cy <= yc && ay > yc) {
                    let t = (yc - ay) / (cy - ay);
                    xs.push(a[0] as f32 + 0.5 + t * (c[0] - a[0]) as f32);
                }
            }
            xs.sort_by(f32::total_cmp);
            for pair in xs.chunks(2) {
                if let [l, r] = pair {
                    let x0 = (l - 0.5).ceil() as i32;
                    let x1 = (r - 0.5).floor() as i32;
                    for x in span(x0, x1 - x0 + 1, img.width()) {
                        b.paint(img, x, y);
                    }
                }
            }
        }
    }
    let edges = if fill || n > 2 { n } else { n - 1 };
    for i in 0..edges.max(1) {
        for (x, y) in line_pts(pts[i], pts[(i + 1) % n]) {
            b.paint(img, x, y);
        }
    }
}

fn ellipse(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, b: &Brush, fill: bool) {
    if w <= 0 || h <= 0 {
        return;
    }
    let (rx, ry) = (w as f32 / 2.0, h as f32 / 2.0);
    let (cx, cy) = (x as f32 + rx, y as f32 + ry);
    let inside = |px: i32, py: i32| {
        let nx = (px as f32 + 0.5 - cx) / rx;
        let ny = (py as f32 + 0.5 - cy) / ry;
        nx * nx + ny * ny <= 1.0
    };
    for py in span(y, h, img.height()) {
        for px in span(x, w, img.width()) {
            if !inside(px, py) {
                continue;
            }
            let edge = !inside(px - 1, py) || !inside(px + 1, py) || !inside(px, py - 1) || !inside(px, py + 1);
            if fill || edge {
                b.paint(img, px, py);
            }
        }
    }
}

fn flood(img: &mut RgbaImage, x: i32, y: i32, b: &Brush) {
    let Some(target) = get(img, x, y) else { return };
    let (w, h) = (img.width() as i32, img.height() as i32);
    let mut seen = vec![false; (w * h) as usize];
    let mut region = vec![];
    let mut stack = vec![(x, y)];
    while let Some((x, y)) = stack.pop() {
        if x < 0 || y < 0 || x >= w || y >= h || seen[(y * w + x) as usize] {
            continue;
        }
        seen[(y * w + x) as usize] = true;
        if get(img, x, y) == Some(target) {
            region.push((x, y));
            stack.extend([(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)]);
        }
    }
    for (x, y) in region {
        b.paint(img, x, y);
    }
}

/// Coordinates further out than this are rejected (they would only waste time off-canvas).
const MAX_COORD: f32 = 8192.0;

fn check_coords<'a>(pts: impl IntoIterator<Item = &'a [i32; 2]>) -> Result<()> {
    for p in pts {
        if p[0].unsigned_abs() as f32 > MAX_COORD || p[1].unsigned_abs() as f32 > MAX_COORD {
            bail!("coordinate {p:?} is far outside any canvas (limit ±{MAX_COORD})");
        }
    }
    Ok(())
}

/// Intersection of [start, start+len) with [0, max).
fn span(start: i32, len: i32, max: u32) -> std::ops::Range<i32> {
    let a = start.max(0);
    let b = start.saturating_add(len.max(0)).min(max as i32);
    a..b.max(a)
}

/// Drop consecutive duplicates and the elbow pixel of every L-shaped corner, so strokes are
/// one clean pixel wide (no doubled corners / jaggies).
pub fn pixel_perfect(pts: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut v: Vec<(i32, i32)> = vec![];
    for p in pts {
        if v.last() != Some(p) {
            v.push(*p);
        }
    }
    let mut out: Vec<(i32, i32)> = vec![];
    for i in 0..v.len() {
        // skip v[i] when its neighbours already touch (diagonally at a turn, or side by side
        // at a hairpin): the path stays connected and loses the doubled corner
        if let (Some(a), Some(c)) = (out.last(), v.get(i + 1)) {
            let (dx, dy) = ((a.0 - c.0).abs(), (a.1 - c.1).abs());
            if (dx == 1 && dy == 1) || dx + dy == 1 {
                continue;
            }
        }
        out.push(v[i]);
    }
    out
}

fn catmull(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let (t2, t3) = (t * t, t * t * t);
    let f = |i: usize| {
        0.5 * (2.0 * p1[i] + (-p0[i] + p2[i]) * t + (2.0 * p0[i] - 5.0 * p1[i] + 4.0 * p2[i] - p3[i]) * t2 + (-p0[i] + 3.0 * p1[i] - 3.0 * p2[i] + p3[i]) * t3)
    };
    [f(0), f(1)]
}

fn bezier(c: &[[f32; 2]], t: f32) -> [f32; 2] {
    // de Casteljau
    let mut p = c.to_vec();
    while p.len() > 1 {
        p = p.windows(2).map(|w| [w[0][0] + (w[1][0] - w[0][0]) * t, w[0][1] + (w[1][1] - w[0][1]) * t]).collect();
    }
    p[0]
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Dense float samples along a curve (pixel-index coordinates).
pub fn curve_samples(points: &[[f32; 2]], kind: &str, closed: bool) -> Result<Vec<[f32; 2]>> {
    let n = points.len();
    if n > 1024 {
        bail!("curve has {n} points (limit 1024)");
    }
    for p in points {
        if !p[0].is_finite() || !p[1].is_finite() || p[0].abs() > MAX_COORD || p[1].abs() > MAX_COORD {
            bail!("curve point {p:?} is not a usable coordinate (finite, within ±{MAX_COORD})");
        }
    }
    let mut out = vec![];
    let seg = |a: [f32; 2], b: [f32; 2], f: &dyn Fn(f32) -> [f32; 2], out: &mut Vec<[f32; 2]>| {
        let steps = ((dist(a, b) * 4.0).ceil() as usize).clamp(8, 20_000);
        for i in 0..=steps {
            out.push(f(i as f32 / steps as f32));
        }
    };
    match kind {
        "spline" => {
            if n < 2 {
                bail!("curve needs at least 2 points");
            }
            let at = |i: i32| -> [f32; 2] {
                if closed {
                    points[i.rem_euclid(n as i32) as usize]
                } else {
                    points[i.clamp(0, n as i32 - 1) as usize]
                }
            };
            let segs = if closed { n } else { n - 1 };
            for i in 0..segs as i32 {
                let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
                seg(p1, p2, &|t| catmull(p0, p1, p2, p3, t), &mut out);
            }
        }
        "bezier" => {
            let chunks: Vec<Vec<[f32; 2]>> = match n {
                3 | 4 => vec![points.to_vec()],
                _ if n >= 7 && (n - 1).is_multiple_of(3) => (0..(n - 1) / 3).map(|i| points[i * 3..i * 3 + 4].to_vec()).collect(),
                _ => bail!("bezier needs 3 (quadratic), 4 (cubic) or 7, 10, 13... (chained cubic) points, got {n}"),
            };
            for c in &chunks {
                // the control polygon's length bounds the curve's length -> sample density
                let len: f32 = c.windows(2).map(|w| dist(w[0], w[1])).sum();
                seg([0.0, 0.0], [len, 0.0], &|t| bezier(c, t), &mut out);
            }
            if closed && out.len() > 1 {
                let (a, b) = (out[out.len() - 1], out[0]);
                seg(a, b, &|t| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], &mut out);
            }
        }
        k => bail!("unknown curve kind '{k}' (spline, bezier)"),
    }
    if out.len() > 400_000 {
        bail!("curve is too long ({} samples); use fewer or closer points", out.len());
    }
    for p in &out {
        if !p[0].is_finite() || !p[1].is_finite() || p[0].abs() > MAX_COORD || p[1].abs() > MAX_COORD {
            bail!("curve leaves the drawable range (±{MAX_COORD})");
        }
    }
    Ok(out)
}

/// Connected integer path through float samples (gaps bridged with Bresenham).
fn samples_to_path(s: &[[f32; 2]]) -> Vec<(i32, i32)> {
    let mut path: Vec<(i32, i32)> = vec![];
    for p in s {
        let q = (p[0].round() as i32, p[1].round() as i32);
        match path.last().copied() {
            Some(l) if l == q => {}
            Some(l) if (l.0 - q.0).abs() > 1 || (l.1 - q.1).abs() > 1 => {
                path.extend(line_pts([l.0, l.1], [q.0, q.1]).into_iter().skip(1));
            }
            _ => path.push(q),
        }
    }
    path
}

/// Scanline fill of a float polygon whose vertices are pixel indices (pixel centers).
fn fill_poly_f(img: &mut RgbaImage, pts: &[[f32; 2]], b: &Brush) {
    if pts.len() < 3 {
        return;
    }
    let miny = pts.iter().map(|p| p[1]).fold(f32::MAX, f32::min).floor() as i32;
    let maxy = pts.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil() as i32;
    let n = pts.len();
    for y in span(miny, maxy - miny + 1, img.height()) {
        let yc = y as f32;
        let mut xs = vec![];
        for i in 0..n {
            let (a, c) = (pts[i], pts[(i + 1) % n]);
            if (a[1] <= yc && c[1] > yc) || (c[1] <= yc && a[1] > yc) {
                xs.push(a[0] + (yc - a[1]) / (c[1] - a[1]) * (c[0] - a[0]));
            }
        }
        xs.sort_by(f32::total_cmp);
        for pair in xs.chunks(2) {
            if let [l, r] = pair {
                for x in span(l.ceil() as i32, r.floor() as i32 - l.ceil() as i32 + 1, img.width()) {
                    b.paint(img, x, y);
                }
            }
        }
    }
}

fn region_rect(img: &RgbaImage, rect: &Option<[i32; 4]>) -> (std::ops::Range<i32>, std::ops::Range<i32>) {
    let [x, y, w, h] = rect.unwrap_or([0, 0, img.width() as i32, img.height() as i32]);
    (span(x, w, img.width()), span(y, h, img.height()))
}

fn color_set(colors: &Option<Vec<String>>, pal: &Palette) -> Result<Option<Vec<Px>>> {
    colors.as_ref().map(|cs| cs.iter().map(|c| parse_color(c, pal)).collect::<Result<Vec<_>>>()).transpose()
}

pub fn apply(img: &mut RgbaImage, op: &Op, pal: &Palette) -> Result<()> {
    let col = |s: &str| parse_color(s, pal);
    match op {
        Op::Pixels { points, color } => {
            let c = col(color)?;
            for p in points {
                put(img, p[0], p[1], c);
            }
        }
        Op::Line { points, color, clean } => {
            check_coords(points)?;
            let c = col(color)?;
            let mut path: Vec<(i32, i32)> = vec![];
            match points.len() {
                0 => {}
                1 => path.push((points[0][0], points[0][1])),
                _ => {
                    for w in points.windows(2) {
                        path.extend(line_pts(w[0], w[1]));
                    }
                }
            }
            if *clean {
                path = pixel_perfect(&path);
            }
            for (x, y) in path {
                put(img, x, y, c);
            }
        }
        Op::Curve { points, color, kind, closed, fill, stroke, clean, pat } => {
            let samples = curve_samples(points, kind.as_deref().unwrap_or("spline"), *closed)?;
            let b = Brush::new(color, pat, pal)?;
            if *fill {
                if !*closed {
                    bail!("fill needs closed: true");
                }
                fill_poly_f(img, &samples, &b);
            }
            let edge = match stroke.as_deref() {
                Some("none") => None,
                Some(s) => Some(col(s)?),
                None if *fill && pat.pattern.is_some() => None,
                None => Some(b.c1),
            };
            if let Some(ec) = edge {
                let mut path = samples_to_path(&samples);
                if *clean {
                    if *closed {
                        // treat the loop as a ring: drop the repeated start, then clean across the seam too
                        while path.len() > 1 && path.first() == path.last() {
                            path.pop();
                        }
                        let n = path.len();
                        if n > 4 {
                            let mut ring = path.clone();
                            ring.extend_from_slice(&path[..2.min(n)]);
                            ring.insert(0, path[n - 1]);
                            let cleaned = pixel_perfect(&ring);
                            // cleaned starts with the wrap-around pixel and ends with the first two again
                            let core: Vec<(i32, i32)> = cleaned.into_iter().skip(1).collect();
                            let mut seen = std::collections::HashSet::new();
                            path = core.into_iter().filter(|p| seen.insert(*p)).collect();
                        } else {
                            path = pixel_perfect(&path);
                        }
                    } else {
                        path = pixel_perfect(&path);
                    }
                }
                for (x, y) in path {
                    put(img, x, y, ec);
                }
            }
        }
        Op::Mirror { axis, at, from } => {
            let horizontal = match axis.as_deref().unwrap_or("x") {
                "x" | "h" | "horizontal" => true,
                "y" | "v" | "vertical" => false,
                a => bail!("mirror axis must be 'x' or 'y', got '{a}'"),
            };
            let (w, h) = (img.width() as i32, img.height() as i32);
            let at = at.unwrap_or(if horizontal { w as f32 / 2.0 } else { h as f32 / 2.0 });
            let from_low = match from.as_deref() {
                None | Some("left") | Some("top") => true,
                Some("right") | Some("bottom") => false,
                Some(f) => bail!("mirror from must be left/right/top/bottom, got '{f}'"),
            };
            let src = img.clone();
            for y in 0..h {
                for x in 0..w {
                    let c = if horizontal { x as f32 + 0.5 } else { y as f32 + 0.5 };
                    if (from_low && c >= at) || (!from_low && c <= at) {
                        continue;
                    }
                    let t = (2.0 * at - c - 0.5).round() as i32;
                    let p = *src.get_pixel(x as u32, y as u32);
                    if horizontal { put(img, t, y, p) } else { put(img, x, t, p) }
                }
            }
        }
        Op::Shade { light, depth, highlight, steps, colors, rect, edge } => {
            let [lx, ly] = light.unwrap_or([-1.0, -1.0]);
            let len = (lx * lx + ly * ly).sqrt();
            if len < 1e-6 {
                bail!("light direction can't be [0, 0]");
            }
            let (dx, dy) = (-lx / len, -ly / len); // toward the shadow side
            let depth = depth.unwrap_or(1).min(64) as i32;
            let hi = (*highlight).min(64) as i32;
            let steps = steps.unwrap_or(1);
            let only = color_set(colors, pal)?;
            let mode = edge.as_deref().unwrap_or("ramp");
            if !matches!(mode, "ramp" | "color" | "alpha") {
                bail!("edge must be ramp, color or alpha");
            }
            let src = img.clone();
            let ramp_name = |p: Px| pal.entry_of(p).and_then(|e| e.ramp.clone());
            let (xr, yr) = region_rect(img, rect);
            for y in yr {
                for x in xr.clone() {
                    let p = *src.get_pixel(x as u32, y as u32);
                    if p[3] == 0 || only.as_ref().is_some_and(|o| !o.contains(&p)) {
                        continue;
                    }
                    let Some(rn) = ramp_name(p) else { continue };
                    let same = |qx: i32, qy: i32| match get(&src, qx, qy) {
                        Some(q) if q[3] > 0 => match mode {
                            "alpha" => true,
                            "color" => q == p,
                            _ => ramp_name(q).as_deref() == Some(rn.as_str()),
                        },
                        _ => false,
                    };
                    let hits = |sign: f32, n_max: i32| {
                        (1..=n_max).any(|n| !same(x + (sign * dx * n as f32).round() as i32, y + (sign * dy * n as f32).round() as i32))
                    };
                    let new = if hits(1.0, depth) {
                        pal.ramp_step(p, -steps)
                    } else if hi > 0 && hits(-1.0, hi) {
                        pal.ramp_step(p, steps)
                    } else {
                        None
                    };
                    if let Some(n) = new {
                        put(img, x, y, n);
                    }
                }
            }
        }
        Op::Ramp { steps, rect, points, colors } => {
            let only = color_set(colors, pal)?;
            let ok = |p: Px| p[3] > 0 && only.as_ref().is_none_or(|o| o.contains(&p));
            let mut targets: Vec<(i32, i32)> = vec![];
            if let Some(pts) = points {
                targets.extend(pts.iter().map(|p| (p[0], p[1])));
            }
            if rect.is_some() || points.is_none() {
                let (xr, yr) = region_rect(img, rect);
                for y in yr {
                    targets.extend(xr.clone().map(|x| (x, y)));
                }
            }
            let mut no_ramp = 0;
            for (x, y) in targets {
                if let Some(p) = get(img, x, y).filter(|p| ok(*p)) {
                    match pal.ramp_step(p, *steps) {
                        Some(n) => put(img, x, y, n),
                        None => no_ramp += 1,
                    }
                }
            }
            let _ = no_ramp;
        }
        Op::Unjag { colors, level, passes } => {
            let only = color_set(colors, pal)?;
            let level = level.unwrap_or(0.3).clamp(0.0, 1.0);
            let is_line = |p: Option<Px>| {
                p.is_some_and(|p| p[3] > 0 && only.as_ref().map_or(crate::color::lightness(p) < level, |o| o.contains(&p)))
            };
            let (w, h) = (img.width() as i32, img.height() as i32);
            for _ in 0..passes.unwrap_or(2).clamp(1, 8) {
                // in place, so each fix sees the previous one (two neighbouring elbows of a
                // staircase must not both go)
                let mut changed = false;
                for y in 0..h - 1 {
                    for x in 0..w - 1 {
                        let cells = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
                        let line: Vec<bool> = cells.iter().map(|(cx, cy)| is_line(get(img, *cx, *cy))).collect();
                        if line.iter().filter(|b| **b).count() != 3 {
                            continue;
                        }
                        let missing = line.iter().position(|b| !*b).unwrap();
                        let (ex, ey) = cells[3 - missing];
                        let (mx, my) = cells[missing];
                        // only thin line work: an elbow inside a dark area or a thick stroke has
                        // more than the two L neighbours and stays (else dark fills erode)
                        let n4 = [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().filter(|(dx, dy)| is_line(get(img, ex + dx, ey + dy))).count();
                        if n4 != 2 {
                            continue;
                        }
                        // the two remaining line pixels touch diagonally, so the line stays connected
                        let fill = get(img, mx, my).unwrap_or(CLEAR);
                        put(img, ex, ey, fill);
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
        }
        Op::Contour { of, color, against, selout, diagonal } => {
            let of_c = col(of)?;
            let line_c = match (color, selout) {
                (Some(c), _) => col(c)?,
                (None, true) => pal
                    .ramp_of(of_c)
                    .map(|(r, _)| r[0])
                    .filter(|d| *d != of_c)
                    .or(pal.selout(of_c))
                    .ok_or_else(|| anyhow::anyhow!("no darker shade for '{of}'; pass color"))?,
                (None, false) => bail!("contour needs color (or selout: true)"),
            };
            let ag = against.as_deref().map(col).transpose()?;
            let src = img.clone();
            let mut n = vec![(1, 0), (-1, 0), (0, 1), (0, -1)];
            if *diagonal {
                n.extend([(1, 1), (-1, 1), (1, -1), (-1, -1)]);
            }
            for (x, y, p) in src.enumerate_pixels() {
                if *p != of_c {
                    continue;
                }
                let touches = n.iter().any(|(dx, dy)| {
                    get(&src, x as i32 + dx, y as i32 + dy).is_some_and(|q| q[3] > 0 && q != of_c && q != line_c && ag.is_none_or(|a| q == a))
                });
                if touches {
                    put(img, x as i32, y as i32, line_c);
                }
            }
        }
        Op::Rect { x, y, w, h, color, fill, pat } => {
            let b = Brush::new(color, pat, pal)?;
            for py in span(*y, *h, img.height()) {
                for px in span(*x, *w, img.width()) {
                    if *fill || py == *y || py == y.saturating_add(*h).saturating_sub(1) || px == *x || px == x.saturating_add(*w).saturating_sub(1) {
                        b.paint(img, px, py);
                    }
                }
            }
        }
        Op::Ellipse { x, y, w, h, color, fill, pat } => ellipse(img, *x, *y, *w, *h, &Brush::new(color, pat, pal)?, *fill),
        Op::Polygon { points, color, fill, pat } => {
            check_coords(points)?;
            polygon(img, points, &Brush::new(color, pat, pal)?, *fill)
        }
        Op::Fill { x, y, color, pat } => flood(img, *x, *y, &Brush::new(color, pat, pal)?),
        Op::Rows { x, y, rows, legend, erase_dots } => {
            let mut leg: HashMap<char, Px> = HashMap::new();
            for (k, v) in legend {
                let mut chars = k.chars();
                let (Some(ch), None) = (chars.next(), chars.next()) else {
                    bail!("legend keys must be single characters, got '{k}'");
                };
                leg.insert(ch, col(v)?);
            }
            for (j, row) in rows.iter().enumerate() {
                for (i, ch) in row.chars().enumerate() {
                    let (px, py) = (x + i as i32, y + j as i32);
                    let c = if let Some(c) = leg.get(&ch) {
                        *c
                    } else {
                        match ch {
                            '.' | ' ' => {
                                if *erase_dots && ch == '.' {
                                    CLEAR
                                } else {
                                    continue;
                                }
                            }
                            '_' => CLEAR,
                            _ => match pal.by_key(ch) {
                                Some(c) => c,
                                None => bail!("row {j} col {i}: '{ch}' is not a palette key or legend char (palette: {})", pal.text()),
                            },
                        }
                    };
                    put(img, px, py, c);
                }
            }
        }
        Op::Replace { from, to } => {
            let (f, t) = (col(from)?, col(to)?);
            for p in img.pixels_mut() {
                if *p == f || (f[3] == 0 && p[3] == 0) {
                    *p = t;
                }
            }
        }
        Op::Outline { color, diagonal, selout } => {
            let fallback = match color {
                Some(c) => Some(col(c)?),
                None if *selout => None,
                None => bail!("outline needs a color (or selout: true)"),
            };
            let mut targets = vec![];
            for py in 0..img.height() as i32 {
                for px in 0..img.width() as i32 {
                    if get(img, px, py).is_some_and(|p| p[3] > 0) {
                        continue;
                    }
                    let mut n = vec![(1, 0), (-1, 0), (0, 1), (0, -1)];
                    if *diagonal {
                        n.extend([(1, 1), (-1, 1), (1, -1), (-1, -1)]);
                    }
                    let neigh: Vec<Px> = n.iter().filter_map(|(dx, dy)| get(img, px + dx, py + dy)).filter(|p| p[3] > 0).collect();
                    if neigh.is_empty() {
                        continue;
                    }
                    let c = if *selout {
                        // most common neighbour color decides the ramp
                        let mut best = neigh[0];
                        let mut bc = 0;
                        for a in &neigh {
                            let cnt = neigh.iter().filter(|b| *b == a).count();
                            if cnt > bc {
                                bc = cnt;
                                best = *a;
                            }
                        }
                        match pal.selout(best).or(fallback).or(pal.darkest()) {
                            Some(c) => c,
                            None => bail!("selout needs a palette (or a fallback color)"),
                        }
                    } else {
                        fallback.unwrap()
                    };
                    targets.push((px, py, c));
                }
            }
            for (px, py, c) in targets {
                put(img, px, py, c);
            }
        }
        Op::Shift { dx, dy, wrap } => {
            check_coords([&[*dx, *dy]])?;
            let (w, h) = (img.width() as i32, img.height() as i32);
            let mut out = RgbaImage::new(w as u32, h as u32);
            for py in 0..h {
                for px in 0..w {
                    let (mut nx, mut ny) = (px + dx, py + dy);
                    if *wrap {
                        nx = nx.rem_euclid(w);
                        ny = ny.rem_euclid(h);
                    }
                    put(&mut out, nx, ny, *img.get_pixel(px as u32, py as u32));
                }
            }
            *img = out;
        }
        Op::Flip { h, v } => {
            if *h {
                image::imageops::flip_horizontal_in_place(img);
            }
            if *v {
                image::imageops::flip_vertical_in_place(img);
            }
        }
        Op::Clear { rect } => match rect {
            None => img.pixels_mut().for_each(|p| *p = CLEAR),
            Some([x, y, w, h]) => {
                for py in span(*y, *h, img.height()) {
                    for px in span(*x, *w, img.width()) {
                        put(img, px, py, CLEAR);
                    }
                }
            }
        },
        Op::Copy { from: [x, y, w, h], to: [tx, ty] } => {
            check_coords([&[*tx, *ty], &[*x, *y]])?;
            let mut buf = vec![];
            // the part of the source rect that lies on the image, as offsets from (x, y)
            for sy in span(*y, *h, img.height()) {
                for sx in span(*x, *w, img.width()) {
                    if let Some(p) = get(img, sx, sy) {
                        buf.push((sx - x, sy - y, p));
                    }
                }
            }
            for (px, py, p) in buf {
                if p[3] > 0 {
                    put(img, tx + px, ty + py, p);
                }
            }
        }
    }
    Ok(())
}

/// Text dump of an image region in `rows` format using palette keys.
pub fn to_rows(img: &RgbaImage, pal: &Palette, region: Option<[u32; 4]>) -> (Vec<String>, usize) {
    let [rx, ry, rw, rh] = region.unwrap_or([0, 0, img.width(), img.height()]);
    let mut off = 0;
    let rows = (ry..(ry + rh).min(img.height()))
        .map(|y| {
            (rx..(rx + rw).min(img.width()))
                .map(|x| {
                    let p = *img.get_pixel(x, y);
                    if p[3] == 0 {
                        return '.';
                    }
                    pal.key_of(p).unwrap_or_else(|| {
                        off += 1;
                        '?'
                    })
                })
                .collect()
        })
        .collect();
    (rows, off)
}
