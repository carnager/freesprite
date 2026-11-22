use anyhow::{Result, anyhow, bail};
use image::Rgba;

pub type Px = Rgba<u8>;
pub const CLEAR: Px = Rgba([0, 0, 0, 0]);

/// Characters used for palette indices in the `rows` pixel format.
pub const ROW_CHARS: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

pub fn parse_hex(s: &str) -> Result<Px> {
    let h = s.trim().trim_start_matches('#');
    if !h.is_ascii() {
        bail!("bad color '{s}'");
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16);
    let nib = |i: usize| u8::from_str_radix(&h[i..i + 1].repeat(2), 16);
    let c = match h.len() {
        3 => Rgba([nib(0)?, nib(1)?, nib(2)?, 255]),
        6 => Rgba([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Rgba([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => bail!("bad color '{s}' (use #rgb, #rrggbb or #rrggbbaa)"),
    };
    Ok(c)
}

pub fn to_hex(c: Px) -> String {
    if c[3] == 255 {
        format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
    }
}

/// Accepts a palette key (single char), "#hex", "@N" (palette index) or "transparent".
pub fn parse_color(s: &str, pal: &Palette) -> Result<Px> {
    let palette = pal.colors();
    let t = s.trim();
    if t.eq_ignore_ascii_case("transparent") || t.eq_ignore_ascii_case("none") {
        return Ok(CLEAR);
    }
    if let Some(i) = t.strip_prefix('@') {
        let i: usize = i.parse().map_err(|_| anyhow!("bad palette index '{t}'"))?;
        return palette
            .get(i)
            .copied()
            .ok_or_else(|| anyhow!("palette index {i} out of range (palette has {} colors)", palette.len()));
    }
    let mut ch = t.chars();
    if let (Some(k), None) = (ch.next(), ch.next()) {
        return pal.by_key(k).ok_or_else(|| anyhow!("no palette key '{k}' (palette: {})", pal.text()));
    }
    parse_hex(t)
}

fn dist(a: [u8; 3], b: [u8; 3]) -> u32 {
    let d = |i: usize| (a[i] as i32 - b[i] as i32).pow(2) as u32;
    // rough perceptual weighting
    2 * d(0) + 4 * d(1) + 3 * d(2)
}

pub fn nearest(c: Px, palette: &[Px]) -> Option<usize> {
    let rgb = [c[0], c[1], c[2]];
    palette
        .iter()
        .enumerate()
        .filter(|(_, p)| p[3] > 0)
        .min_by_key(|(_, p)| dist(rgb, [p[0], p[1], p[2]]))
        .map(|(i, _)| i)
}

/// Median-cut quantization of opaque pixels down to `n` colors.
pub fn median_cut(px: Vec<[u8; 3]>, n: usize) -> Vec<Px> {
    if px.is_empty() || n == 0 {
        return vec![];
    }
    let mut boxes: Vec<Vec<[u8; 3]>> = vec![px];
    while boxes.len() < n {
        let mut best: Option<(usize, usize)> = None;
        let mut best_range = 0u32;
        for (i, b) in boxes.iter().enumerate() {
            if b.len() < 2 {
                continue;
            }
            for ch in 0..3 {
                let (mn, mx) = b.iter().fold((255u8, 0u8), |(a, z), p| (a.min(p[ch]), z.max(p[ch])));
                // weight by population so big boxes split first
                let r = (mx - mn) as u32 * (b.len() as f64).sqrt() as u32;
                if mx > mn && r > best_range {
                    best_range = r;
                    best = Some((i, ch));
                }
            }
        }
        let Some((i, ch)) = best else { break };
        let mut b = boxes.swap_remove(i);
        b.sort_unstable_by_key(|p| p[ch]);
        let hi = b.split_off(b.len() / 2);
        boxes.push(b);
        boxes.push(hi);
    }
    let mut out: Vec<Px> = boxes
        .iter()
        .map(|b| {
            let mut s = [0u64; 3];
            for p in b {
                for i in 0..3 {
                    s[i] += p[i] as u64;
                }
            }
            let n = b.len() as u64;
            Rgba([(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8, 255])
        })
        .collect();
    // sort dark to light for a tidier palette
    out.sort_by_key(|c| c[0] as u32 * 2 + c[1] as u32 * 4 + c[2] as u32 * 3);
    out.dedup();
    out
}

/// A palette entry: a single-char key used in text rows (e.g. 'o' outline, 'A' skin base),
/// optional name, optional ramp group (for shading / sel-out).
#[derive(Clone, Debug, PartialEq)]
pub struct PalEntry {
    pub key: char,
    pub color: Px,
    pub name: Option<String>,
    pub ramp: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Palette(pub Vec<PalEntry>);

pub const RESERVED_KEYS: &str = "._ ";

fn lum(c: Px) -> u32 {
    c[0] as u32 * 2 + c[1] as u32 * 4 + c[2] as u32 * 3
}

impl Palette {
    pub fn colors(&self) -> Vec<Px> {
        self.0.iter().map(|e| e.color).collect()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn by_key(&self, k: char) -> Option<Px> {
        self.0.iter().find(|e| e.key == k).map(|e| e.color)
    }
    pub fn entry_of(&self, p: Px) -> Option<&PalEntry> {
        self.0.iter().find(|e| e.color == p)
    }
    pub fn key_of(&self, p: Px) -> Option<char> {
        self.entry_of(p).map(|e| e.key)
    }
    pub fn next_free_key(&self) -> Option<char> {
        ROW_CHARS.chars().find(|c| !self.0.iter().any(|e| e.key == *c))
    }
    pub fn from_colors(cols: Vec<Px>) -> Palette {
        let mut p = Palette::default();
        for c in cols {
            p.push(c, None, None);
        }
        p
    }
    pub fn push(&mut self, color: Px, name: Option<String>, ramp: Option<String>) -> bool {
        match self.next_free_key() {
            Some(key) => {
                self.0.push(PalEntry { key, color, name, ramp });
                true
            }
            None => false,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for e in &self.0 {
            if RESERVED_KEYS.contains(e.key) || e.key.is_control() {
                bail!("palette key '{}' is reserved ('.', '_' and space have fixed meanings)", e.key);
            }
            if !seen.insert(e.key) {
                bail!("duplicate palette key '{}'", e.key);
            }
        }
        Ok(())
    }
    pub fn text(&self) -> String {
        self.0
            .iter()
            .map(|e| {
                let mut t = format!("{}={}", e.key, to_hex(e.color));
                match (&e.name, &e.ramp) {
                    (Some(n), Some(r)) => t += &format!("({n}, ramp {r})"),
                    (Some(n), None) => t += &format!("({n})"),
                    (None, Some(r)) => t += &format!("(ramp {r})"),
                    _ => {}
                }
                t
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    /// Sel-out color for an outline next to `p`: the darkest member of p's ramp,
    /// or (no ramp) the palette color closest to a darkened p.
    pub fn selout(&self, p: Px) -> Option<Px> {
        if let Some(r) = self.entry_of(p).and_then(|e| e.ramp.clone()) {
            let d = self.0.iter().filter(|e| e.ramp.as_deref() == Some(&r)).map(|e| e.color).min_by_key(|c| lum(*c));
            if d.is_some_and(|d| lum(d) < lum(p)) {
                return d;
            }
        }
        // no (darker) ramp member: nearest palette color to a darkened version, if it is actually darker
        let dark = Rgba([(p[0] as u32 * 45 / 100) as u8, (p[1] as u32 * 45 / 100) as u8, (p[2] as u32 * 45 / 100) as u8, 255]);
        let cols = self.colors();
        nearest(dark, &cols).map(|i| cols[i]).filter(|c| lum(*c) < lum(p))
    }
    pub fn darkest(&self) -> Option<Px> {
        self.0.iter().map(|e| e.color).filter(|c| c[3] > 0).min_by_key(|c| lum(*c))
    }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
pub enum PalSpec {
    Hex(String),
    Full { key: Option<String>, color: String, name: Option<String>, ramp: Option<String> },
}

/// Build a palette from specs: "#hex", "k=#hex" or {key,color,name,ramp}. Missing keys are auto-assigned.
pub fn palette_from_specs(specs: &[PalSpec]) -> Result<Palette> {
    let mut items: Vec<(Option<char>, Px, Option<String>, Option<String>)> = vec![];
    for s in specs {
        let (k, c, n, r) = match s {
            PalSpec::Hex(h) => match h.split_once('=') {
                Some((k, c)) => (Some(k.to_string()), c.to_string(), None, None),
                None => (None, h.clone(), None, None),
            },
            PalSpec::Full { key, color, name, ramp } => (key.clone(), color.clone(), name.clone(), ramp.clone()),
        };
        let key = match k {
            None => None,
            Some(k) => {
                let mut ch = k.chars();
                match (ch.next(), ch.next()) {
                    (Some(c), None) => Some(c),
                    _ => bail!("palette key must be one character, got '{k}'"),
                }
            }
        };
        items.push((key, parse_hex(&c)?, n, r));
    }
    let mut pal = Palette::default();
    // explicit keys first so auto keys don't steal them
    for (k, c, n, r) in &items {
        if let Some(k) = k {
            pal.0.push(PalEntry { key: *k, color: *c, name: n.clone(), ramp: r.clone() });
        }
    }
    let mut ordered = vec![];
    let mut explicit = pal.0.clone().into_iter();
    for (k, c, n, r) in items {
        if k.is_some() {
            ordered.push(explicit.next().unwrap());
        } else {
            let key = ROW_CHARS
                .chars()
                .find(|ch| !pal.0.iter().any(|e| e.key == *ch) && !ordered.iter().any(|e: &PalEntry| e.key == *ch))
                .ok_or_else(|| anyhow!("too many colors (max {})", ROW_CHARS.len()))?;
            ordered.push(PalEntry { key, color: c, name: n, ramp: r });
        }
    }
    let pal = Palette(ordered);
    pal.validate()?;
    Ok(pal)
}

impl serde::Serialize for Palette {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|e| {
            let mut m = serde_json::Map::new();
            m.insert("key".into(), e.key.to_string().into());
            m.insert("color".into(), to_hex(e.color).into());
            if let Some(n) = &e.name {
                m.insert("name".into(), n.clone().into());
            }
            if let Some(r) = &e.ramp {
                m.insert("ramp".into(), r.clone().into());
            }
            serde_json::Value::Object(m)
        }))
    }
}

impl<'de> serde::Deserialize<'de> for Palette {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Palette, D::Error> {
        let specs: Vec<PalSpec> = Vec::deserialize(d)?;
        palette_from_specs(&specs).map_err(serde::de::Error::custom)
    }
}

fn nearest_f(c: [f32; 3], palette: &[Px]) -> usize {
    let mut best = (0usize, f32::MAX);
    for (i, p) in palette.iter().enumerate() {
        if p[3] == 0 {
            continue;
        }
        let d = |k: usize| (c[k] - p[k] as f32).powi(2);
        let v = 2.0 * d(0) + 4.0 * d(1) + 3.0 * d(2);
        if v < best.1 {
            best = (i, v);
        }
    }
    best.0
}

const BAYER4: [[f32; 4]; 4] = [[0., 8., 2., 10.], [12., 4., 14., 6.], [3., 11., 1., 9.], [15., 7., 13., 5.]];

/// Snap opaque pixels to `pal`. dither: "none" | "bayer" (ordered, retro VGA look) | "floyd" (error diffusion).
pub fn quantize_img(img: &mut image::RgbaImage, pal: &[Px], dither: &str, strength: f32) -> Result<()> {
    if pal.is_empty() {
        bail!("palette is empty");
    }
    let (w, h) = img.dimensions();
    match dither {
        "none" => {
            for p in img.pixels_mut() {
                if p[3] > 0 {
                    let i = nearest_f([p[0] as f32, p[1] as f32, p[2] as f32], pal);
                    let a = p[3];
                    *p = pal[i];
                    p[3] = a;
                }
            }
        }
        "bayer" => {
            // spread roughly the gap between neighbouring palette colors
            let spread = 48.0 / (pal.len() as f32).cbrt() * 2.0 * strength;
            for (x, y, p) in img.enumerate_pixels_mut() {
                if p[3] == 0 {
                    continue;
                }
                let t = (BAYER4[(y % 4) as usize][(x % 4) as usize] + 0.5) / 16.0 - 0.5;
                let c = [p[0] as f32 + t * spread, p[1] as f32 + t * spread, p[2] as f32 + t * spread];
                let a = p[3];
                *p = pal[nearest_f(c, pal)];
                p[3] = a;
            }
        }
        "floyd" => {
            let mut buf: Vec<[f32; 3]> = img.pixels().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    let a = img.get_pixel(x, y)[3];
                    if a == 0 {
                        continue;
                    }
                    let old = buf[i];
                    let n = pal[nearest_f(old, pal)];
                    let err = [0, 1, 2].map(|k| (old[k] - n[k] as f32) * strength);
                    let mut px = n;
                    px[3] = a;
                    img.put_pixel(x, y, px);
                    let mut spread = |dx: i32, dy: i32, f: f32| {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx >= 0 && ny >= 0 && (nx as u32) < w && (ny as u32) < h {
                            let j = (ny as u32 * w + nx as u32) as usize;
                            for k in 0..3 {
                                buf[j][k] = (buf[j][k] + err[k] * f).clamp(0.0, 255.0);
                            }
                        }
                    };
                    spread(1, 0, 7.0 / 16.0);
                    spread(-1, 1, 3.0 / 16.0);
                    spread(0, 1, 5.0 / 16.0);
                    spread(1, 1, 1.0 / 16.0);
                }
            }
        }
        d => bail!("unknown dither '{d}' (none, bayer, floyd)"),
    }
    Ok(())
}

/// Color pairs (from -> to) for every key present in both palettes with a different color.
pub fn remap_pairs(from: &Palette, to: &Palette) -> Vec<(Px, Px)> {
    from.0
        .iter()
        .filter_map(|e| to.by_key(e.key).filter(|n| *n != e.color).map(|n| (e.color, n)))
        .collect()
}

pub fn apply_remap(img: &mut image::RgbaImage, pairs: &[(Px, Px)]) {
    if pairs.is_empty() {
        return;
    }
    for p in img.pixels_mut() {
        if let Some((_, n)) = pairs.iter().find(|(o, _)| o == p) {
            *p = *n;
        }
    }
}
