use std::borrow::Cow;

use anyhow::Result;
use image::{Rgba, RgbaImage};

use crate::color::Px;
use crate::model::{Frame, Part, Pose, Sprite};

pub fn over(dst: &mut Px, src: Px) {
    let sa = src[3] as f32 / 255.0;
    if sa <= 0.0 {
        return;
    }
    if sa >= 1.0 {
        *dst = src;
        return;
    }
    let da = dst[3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    for i in 0..3 {
        dst[i] = ((src[i] as f32 * sa + dst[i] as f32 * da * (1.0 - sa)) / oa).round() as u8;
    }
    dst[3] = (oa * 255.0).round() as u8;
}

pub fn paste(dst: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32) {
    for (sx, sy, p) in src.enumerate_pixels() {
        let (dx, dy) = (x + sx as i32, y + sy as i32);
        if dx >= 0 && dy >= 0 && (dx as u32) < dst.width() && (dy as u32) < dst.height() {
            over(dst.get_pixel_mut(dx as u32, dy as u32), *p);
        }
    }
}

/// EPX / Scale2x — used to build a RotSprite-like upscaled source for clean rotations.
pub fn scale2x(img: &RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut out = RgbaImage::new(w * 2, h * 2);
    let at = |x: i64, y: i64| *img.get_pixel(x.clamp(0, w as i64 - 1) as u32, y.clamp(0, h as i64 - 1) as u32);
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let e = at(x, y);
            let (b, d, f, hh) = (at(x, y - 1), at(x - 1, y), at(x + 1, y), at(x, y + 1));
            let e0 = if d == b && b != f && d != hh { d } else { e };
            let e1 = if b == f && b != d && f != hh { f } else { e };
            let e2 = if d == hh && d != b && hh != f { d } else { e };
            let e3 = if hh == f && d != hh && b != f { f } else { e };
            let (ox, oy) = (x as u32 * 2, y as u32 * 2);
            out.put_pixel(ox, oy, e0);
            out.put_pixel(ox + 1, oy, e1);
            out.put_pixel(ox, oy + 1, e2);
            out.put_pixel(ox + 1, oy + 1, e3);
        }
    }
    out
}

pub fn blit_pose(out: &mut RgbaImage, part: &Part, p: &Pose) {
    let rot = p.rot.rem_euclid(360.0);
    let exact = (rot - (rot / 90.0).round() * 90.0).abs() < 1e-3;
    let (src, k): (Cow<RgbaImage>, f32) = if exact {
        (Cow::Borrowed(&part.img), 1.0)
    } else {
        (Cow::Owned(scale2x(&scale2x(&scale2x(&part.img)))), 8.0)
    };
    let (mut s, mut c) = rot.to_radians().sin_cos();
    for v in [&mut s, &mut c] {
        if v.abs() < 1e-5 {
            *v = 0.0;
        }
        if (v.abs() - 1.0).abs() < 1e-5 {
            *v = v.signum();
        }
    }
    let (pw, ph) = (part.img.width() as f32, part.img.height() as f32);
    let [pvx, pvy] = part.pivot;
    let fx = if p.flip_h { -1.0 } else { 1.0 };
    let fy = if p.flip_v { -1.0 } else { 1.0 };
    let (px, py) = (p.x as f32, p.y as f32);

    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (cx, cy) in [(0.0, 0.0), (pw, 0.0), (0.0, ph), (pw, ph)] {
        let (ox, oy) = ((cx - pvx) * fx, (cy - pvy) * fy);
        let (dx, dy) = (c * ox - s * oy + px, s * ox + c * oy + py);
        minx = minx.min(dx);
        maxx = maxx.max(dx);
        miny = miny.min(dy);
        maxy = maxy.max(dy);
    }
    let x0 = (minx.floor() as i32 - 1).max(0);
    let y0 = (miny.floor() as i32 - 1).max(0);
    let x1 = (maxx.ceil() as i32 + 1).min(out.width() as i32);
    let y1 = (maxy.ceil() as i32 + 1).min(out.height() as i32);
    for dy in y0..y1 {
        for dx in x0..x1 {
            let ux = dx as f32 + 0.5 - px;
            let uy = dy as f32 + 0.5 - py;
            let ox = c * ux + s * uy;
            let oy = -s * ux + c * uy;
            let sx = (ox * fx + pvx) * k;
            let sy = (oy * fy + pvy) * k;
            if sx < 0.0 || sy < 0.0 {
                continue;
            }
            let (ix, iy) = (sx.floor() as u32, sy.floor() as u32);
            if ix >= src.width() || iy >= src.height() {
                continue;
            }
            over(out.get_pixel_mut(dx as u32, dy as u32), *src.get_pixel(ix, iy));
        }
    }
}

pub fn compose(s: &Sprite, f: &Frame) -> Result<RgbaImage> {
    let mut out = RgbaImage::new(s.width, s.height);
    for pose in &f.poses {
        blit_pose(&mut out, s.part(&pose.part)?, pose);
    }
    Ok(out)
}

pub fn fade(img: &RgbaImage, a: f32) -> RgbaImage {
    let mut o = img.clone();
    for p in o.pixels_mut() {
        p[3] = (p[3] as f32 * a) as u8;
    }
    o
}

pub fn upscale(img: &RgbaImage, k: u32) -> RgbaImage {
    image::imageops::resize(img, img.width() * k, img.height() * k, image::imageops::FilterType::Nearest)
}

/// Solid color, or a checkerboard when `bg` is None.
pub fn backdrop(w: u32, h: u32, bg: Option<Px>, cell: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| match bg {
        Some(c) => c,
        None => {
            if ((x / cell) + (y / cell)) % 2 == 0 {
                Rgba([200, 200, 205, 255])
            } else {
                Rgba([232, 232, 236, 255])
            }
        }
    })
}

pub fn present(img: &RgbaImage, k: u32, bg: Option<Px>) -> RgbaImage {
    let up = upscale(img, k);
    let mut b = backdrop(up.width(), up.height(), bg, (k * 2).max(4));
    paste(&mut b, &up, 0, 0);
    b
}

/// Draws a small cross centered on continuous coordinate (x,y) of an image scaled by k.
pub fn marker(img: &mut RgbaImage, x: f32, y: f32, k: u32, c: Px) {
    let (cx, cy) = ((x * k as f32) as i32, (y * k as f32) as i32);
    let r = (k as i32 * 2).max(3);
    for d in -r..=r {
        for (px, py) in [(cx + d, cy), (cx, cy + d)] {
            if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                img.put_pixel(px as u32, py as u32, c);
            }
        }
    }
}

/// Lays frames out left-to-right, rows top-to-bottom, scaled by k, separated by a gap.
pub fn grid(rows: &[Vec<RgbaImage>], k: u32, bg: Option<Px>) -> RgbaImage {
    let gap = 4;
    let cw = rows.iter().flatten().map(|i| i.width()).max().unwrap_or(1) * k;
    let ch = rows.iter().flatten().map(|i| i.height()).max().unwrap_or(1) * k;
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1) as u32;
    let n = rows.len().max(1) as u32;
    let mut out = RgbaImage::from_pixel(cols * (cw + gap) + gap, n * (ch + gap) + gap, Rgba([60, 60, 70, 255]));
    for (j, row) in rows.iter().enumerate() {
        for (i, img) in row.iter().enumerate() {
            let cell = present(img, k, bg);
            let (x, y) = (gap + i as u32 * (cw + gap), gap + j as u32 * (ch + gap));
            image::imageops::replace(&mut out, &cell, x as i64, y as i64);
        }
    }
    out
}

pub fn auto_scale(w: u32, h: u32, across: u32, down: u32) -> u32 {
    let by_w = 1600 / (w * across.max(1)).max(1);
    let by_h = 1200 / (h * down.max(1)).max(1);
    by_w.min(by_h).min(12).max(1)
}

/// Box-filter resize (premultiplied alpha). Good for downscaling; acts like nearest when upscaling.
pub fn box_resize(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let (sw, sh) = img.dimensions();
    RgbaImage::from_fn(w, h, |x, y| {
        let x0 = (x as u64 * sw as u64 / w as u64) as u32;
        let x1 = (((x as u64 + 1) * sw as u64 / w as u64) as u32).max(x0 + 1).min(sw);
        let y0 = (y as u64 * sh as u64 / h as u64) as u32;
        let y1 = (((y as u64 + 1) * sh as u64 / h as u64) as u32).max(y0 + 1).min(sh);
        let mut acc = [0u64; 4];
        let mut n = 0u64;
        for sy in y0..y1 {
            for sx in x0..x1 {
                let p = img.get_pixel(sx, sy);
                let a = p[3] as u64;
                acc[0] += p[0] as u64 * a;
                acc[1] += p[1] as u64 * a;
                acc[2] += p[2] as u64 * a;
                acc[3] += a;
                n += 1;
            }
        }
        if acc[3] == 0 {
            return Rgba([0, 0, 0, 0]);
        }
        Rgba([
            (acc[0] / acc[3]) as u8,
            (acc[1] / acc[3]) as u8,
            (acc[2] / acc[3]) as u8,
            (acc[3] / n) as u8,
        ])
    })
}

// ---------- review sheet ----------

const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// Draws digits with a 3x5 font at `scale`; returns the width used.
pub fn digits(img: &mut RgbaImage, x: i32, y: i32, n: u32, scale: u32, c: Px) -> i32 {
    let s = n.to_string();
    let sc = scale as i32;
    for (i, ch) in s.chars().enumerate() {
        let g = DIGITS[ch.to_digit(10).unwrap() as usize];
        let ox = x + i as i32 * 4 * sc;
        for (row, bits) in g.iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) != 0 {
                    for dy in 0..sc {
                        for dx in 0..sc {
                            let (px, py) = (ox + col * sc + dx, y + row as i32 * sc + dy);
                            if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                                img.put_pixel(px as u32, py as u32, c);
                            }
                        }
                    }
                }
            }
        }
    }
    s.len() as i32 * 4 * sc
}

fn blend(dst: &mut RgbaImage, x: u32, y: u32, c: Px) {
    if x < dst.width() && y < dst.height() {
        over(dst.get_pixel_mut(x, y), c);
    }
}

/// Zoomed image with pixel grid (every 4th line stronger) and coordinate labels every 4 px.
pub fn zoom_grid(img: &RgbaImage, k: u32, mark: Option<[f32; 2]>) -> RgbaImage {
    let (w, h) = img.dimensions();
    let (ml, mt) = (4 * 4 * 2 + 6, 5 * 2 + 6);
    let mut out = RgbaImage::from_pixel(w * k + ml + 2, h * k + mt + 2, Rgba([40, 40, 48, 255]));
    let z = present(img, k, None);
    image::imageops::replace(&mut out, &z, ml as i64, mt as i64);
    if k >= 4 {
        for i in 0..=w {
            let strong = i % 4 == 0;
            let c = if strong { Rgba([0, 0, 0, 110]) } else { Rgba([0, 0, 0, 35]) };
            for y in 0..h * k {
                blend(&mut out, ml + (i * k).min(w * k - 1), mt + y, c);
            }
        }
        for j in 0..=h {
            let strong = j % 4 == 0;
            let c = if strong { Rgba([0, 0, 0, 110]) } else { Rgba([0, 0, 0, 35]) };
            for x in 0..w * k {
                blend(&mut out, ml + x, mt + (j * k).min(h * k - 1), c);
            }
        }
    }
    let lc = Rgba([220, 220, 230, 255]);
    for i in (0..w).step_by(4) {
        digits(&mut out, (ml + i * k) as i32 + 1, 2, i, 2, lc);
    }
    for j in (0..h).step_by(4) {
        digits(&mut out, 2, (mt + j * k) as i32 + 1, j, 2, lc);
    }
    if let Some([mx, my]) = mark {
        let mut sub = RgbaImage::new(w * k, h * k);
        marker(&mut sub, mx, my, k, Rgba([255, 0, 200, 255]));
        paste(&mut out, &sub, ml as i32, mt as i32);
    }
    out
}

pub fn silhouette(img: &RgbaImage, k: u32) -> RgbaImage {
    let s = RgbaImage::from_fn(img.width(), img.height(), |x, y| {
        if img.get_pixel(x, y)[3] > 0 { Rgba([0, 0, 0, 255]) } else { Rgba([255, 255, 255, 255]) }
    });
    upscale(&s, k)
}

pub fn review(img: &RgbaImage, strip: Option<&[RgbaImage]>, zoom: Option<u32>, mark: Option<[f32; 2]>) -> RgbaImage {
    let (w, h) = img.dimensions();
    let k = zoom.unwrap_or_else(|| (720 / w.max(h).max(1)).clamp(2, 12));
    let pad = 8u32;
    let one = present(img, 1, None);
    let sil = silhouette(img, 2);
    let zg = zoom_grid(img, k, mark);
    let strip_img = strip.filter(|f| f.len() > 1).map(|f| {
        let sk = (1400 / (w * f.len() as u32).max(1)).clamp(1, 4);
        grid(&[f.to_vec()], sk, None)
    });
    let top_w = pad + one.width() + pad + sil.width() + pad + zg.width() + pad;
    let top_h = zg.height().max(sil.height()).max(one.height()) + 2 * pad;
    let (sw, sh) = strip_img.as_ref().map(|s| (s.width() + 2 * pad, s.height() + pad)).unwrap_or((0, 0));
    let mut out = RgbaImage::from_pixel(top_w.max(sw), top_h + sh, Rgba([28, 28, 34, 255]));
    let mut x = pad;
    for panel in [&one, &sil, &zg] {
        image::imageops::replace(&mut out, panel, x as i64, pad as i64);
        x += panel.width() + pad;
    }
    if let Some(s) = strip_img {
        image::imageops::replace(&mut out, &s, pad as i64, top_h as i64);
    }
    out
}

/// Composes a frame onto a canvas enlarged by `m` on each side (to detect clipping).
pub fn compose_margin(s: &Sprite, f: &Frame, m: i32) -> Result<RgbaImage> {
    let mut out = RgbaImage::new(s.width + 2 * m as u32, s.height + 2 * m as u32);
    for pose in &f.poses {
        let mut p = pose.clone();
        p.x += m;
        p.y += m;
        blit_pose(&mut out, s.part(&pose.part)?, &p);
    }
    Ok(out)
}
