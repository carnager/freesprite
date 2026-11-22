use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::color::Palette;

/// A reusable piece of pixel art (head, arm, leg...). `pivot` is the point
/// (in part pixel coordinates; .5 = pixel center) that a pose places on the canvas
/// and that rotation happens around.
#[derive(Clone, Serialize, Deserialize)]
pub struct Part {
    pub pivot: [f32; 2],
    #[serde(skip)]
    pub img: RgbaImage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pose {
    pub part: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    /// degrees, clockwise
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rot: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_h: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_v: bool,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn default_duration() -> u32 {
    100
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    #[serde(default = "default_duration")]
    pub duration_ms: u32,
    /// draw order: first = back, last = front
    #[serde(default)]
    pub poses: Vec<Pose>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Animation {
    #[serde(rename = "loop", default = "yes")]
    pub looped: bool,
    #[serde(default)]
    pub frames: Vec<Frame>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Sprite {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// anchor point (usually between the feet) used for placement in the game
    pub origin: [i32; 2],
    #[serde(default)]
    pub palette: Palette,
    /// named lighting variants (same keys, different colors), e.g. day / dusk / tavern
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub palette_variants: BTreeMap<String, Palette>,
    /// optional face crop [x, y, w, h] for dialog portraits
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portrait: Option<[u32; 4]>,
    #[serde(default)]
    pub parts: BTreeMap<String, Part>,
    #[serde(default)]
    pub animations: BTreeMap<String, Animation>,
}

pub fn check_name(kind: &str, s: &str) -> Result<()> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("invalid {kind} name '{s}' (use letters, digits, _ and -)");
    }
    Ok(())
}

impl Sprite {
    pub fn part(&self, name: &str) -> Result<&Part> {
        self.parts.get(name).with_context(|| {
            format!("no part '{name}' (parts: {})", self.parts.keys().cloned().collect::<Vec<_>>().join(", "))
        })
    }
    pub fn part_mut(&mut self, name: &str) -> Result<&mut Part> {
        let keys = self.parts.keys().cloned().collect::<Vec<_>>().join(", ");
        self.parts.get_mut(name).with_context(|| format!("no part '{name}' (parts: {keys})"))
    }
    pub fn anim(&self, name: &str) -> Result<&Animation> {
        self.animations.get(name).with_context(|| {
            format!("no animation '{name}' (animations: {})", self.animations.keys().cloned().collect::<Vec<_>>().join(", "))
        })
    }
    pub fn anim_mut(&mut self, name: &str) -> Result<&mut Animation> {
        let keys = self.animations.keys().cloned().collect::<Vec<_>>().join(", ");
        self.animations.get_mut(name).with_context(|| format!("no animation '{name}' (animations: {keys})"))
    }

    /// Color remap for a named palette variant (None/"base" = no remap).
    pub fn variant_remap(&self, v: Option<&str>) -> Result<Vec<(crate::color::Px, crate::color::Px)>> {
        match v {
            None | Some("base") => Ok(vec![]),
            Some(n) => {
                let to = self.palette_variants.get(n).with_context(|| {
                    format!("no palette variant '{n}' (variants: {})", self.palette_variants.keys().cloned().collect::<Vec<_>>().join(", "))
                })?;
                Ok(crate::color::remap_pairs(&self.palette, to))
            }
        }
    }

    pub fn validate_frame(&self, f: &Frame) -> Result<()> {
        for p in &f.poses {
            self.part(&p.part)?;
        }
        Ok(())
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let parts_dir = dir.join("parts");
        std::fs::create_dir_all(&parts_dir).with_context(|| format!("creating {}", parts_dir.display()))?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(dir.join("sprite.json"), json)?;
        for (name, part) in &self.parts {
            part.img.save(parts_dir.join(format!("{name}.png")))?;
        }
        for entry in std::fs::read_dir(&parts_dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "png") {
                let stem = path.file_stem().unwrap().to_string_lossy().to_string();
                if !self.parts.contains_key(&stem) {
                    std::fs::remove_file(&path)?;
                }
            }
        }
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Sprite> {
        let path = dir.join("sprite.json");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut s: Sprite = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        for (name, part) in s.parts.iter_mut() {
            let p = dir.join("parts").join(format!("{name}.png"));
            part.img = image::open(&p).with_context(|| format!("loading {}", p.display()))?.to_rgba8();
        }
        Ok(s)
    }
}
