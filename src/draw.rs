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
    /// Bresenham line (inclusive), optionally through several points.
    Line { points: Vec<[i32; 2]>, color: String },
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
        for y in miny..=maxy {
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
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            for pair in xs.chunks(2) {
                if let [l, r] = pair {
                    let x0 = (l - 0.5).ceil() as i32;
                    let x1 = (r - 0.5).floor() as i32;
                    for x in x0..=x1 {
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
    for py in y..y + h {
        for px in x..x + w {
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

pub fn apply(img: &mut RgbaImage, op: &Op, pal: &Palette) -> Result<()> {
    let col = |s: &str| parse_color(s, pal);
    match op {
        Op::Pixels { points, color } => {
            let c = col(color)?;
            for p in points {
                put(img, p[0], p[1], c);
            }
        }
        Op::Line { points, color } => {
            let c = col(color)?;
            match points.len() {
                0 => {}
                1 => put(img, points[0][0], points[0][1], c),
                _ => {
                    for w in points.windows(2) {
                        for (x, y) in line_pts(w[0], w[1]) {
                            put(img, x, y, c);
                        }
                    }
                }
            }
        }
        Op::Rect { x, y, w, h, color, fill, pat } => {
            let b = Brush::new(color, pat, pal)?;
            for py in *y..y + h {
                for px in *x..x + w {
                    if *fill || py == *y || py == y + h - 1 || px == *x || px == x + w - 1 {
                        b.paint(img, px, py);
                    }
                }
            }
        }
        Op::Ellipse { x, y, w, h, color, fill, pat } => ellipse(img, *x, *y, *w, *h, &Brush::new(color, pat, pal)?, *fill),
        Op::Polygon { points, color, fill, pat } => polygon(img, points, &Brush::new(color, pat, pal)?, *fill),
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
                for py in *y..y + h {
                    for px in *x..x + w {
                        put(img, px, py, CLEAR);
                    }
                }
            }
        },
        Op::Copy { from: [x, y, w, h], to: [tx, ty] } => {
            let mut buf = vec![];
            for py in 0..*h {
                for px in 0..*w {
                    if let Some(p) = get(img, x + px, y + py) {
                        buf.push((px, py, p));
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
