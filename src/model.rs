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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Pose {
    pub part: String,
    /// Stable name of this placement (a "bone"). Defaults to the part name. Children reference
    /// it as `parent`, rig frames override it by slot, and swapping the part keeps the slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// Slot of the parent placement. With a parent, x/y are the offset of this part's pivot from
    /// the parent's pivot in the parent's (rotated, flipped) space, rot adds to the parent's
    /// rotation and flips combine. Moving or rotating the parent carries the child along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
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
    /// keep the transform (children still follow) but don't draw the part
    #[serde(default, skip_serializing_if = "is_false")]
    pub hide: bool,
}

impl Pose {
    pub fn slot(&self) -> &str {
        self.slot.as_deref().unwrap_or(&self.part)
    }
}

/// A per-frame change to one rig slot. Offsets and rotation are relative to the rest pose.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Key {
    pub slot: String,
    #[serde(default, skip_serializing_if = "is_zero_i")]
    pub dx: i32,
    #[serde(default, skip_serializing_if = "is_zero_i")]
    pub dy: i32,
    /// degrees added to the rest rotation (clockwise)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rot: f32,
    /// draw this part instead (mouth_closed -> mouth_open, arm -> arm_bent)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// true hides the slot (children still follow), false shows a slot hidden in the rest pose
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flip_h: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flip_v: Option<bool>,
    /// new draw index (0 = back)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<usize>,
}

impl Key {
    pub fn is_noop(&self) -> bool {
        *self == Key { slot: self.slot.clone(), ..Default::default() }
    }
}

fn is_zero_i(v: &i32) -> bool {
    *v == 0
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

/// A frame is either explicit (`poses`, drawn back to front) or, when `poses` is empty and the
/// sprite has a rig, the rig's rest pose with `keys` applied.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    #[serde(default = "default_duration")]
    pub duration_ms: u32,
    /// draw order: first = back, last = front
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub poses: Vec<Pose>,
    /// changes to the rig's rest pose (rig frames only)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Key>,
    /// mirror the whole frame around the origin's x (walk_right -> walk_left)
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_h: bool,
}

impl Frame {
    pub fn rig(duration_ms: u32, keys: Vec<Key>) -> Frame {
        Frame { duration_ms, poses: vec![], keys, flip_h: false }
    }
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
    /// Rest pose: every slot of the character, back to front, with parent links. Frames without
    /// explicit poses are drawn from this plus their keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rig: Vec<Pose>,
    #[serde(default)]
    pub animations: BTreeMap<String, Animation>,
}

/// Placements per frame / rig. Far beyond any character, small enough for quadratic checks.
pub const MAX_POSES: usize = 512;
/// Pose coordinates: canvases are at most 1024 px, so this leaves room without overflow risk.
pub const MAX_POS: u32 = 16_384;

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

    pub fn is_rig_frame(&self, f: &Frame) -> bool {
        f.poses.is_empty() && !self.rig.is_empty()
    }

    /// The placements a frame draws, back to front: its explicit poses, or the rig with the
    /// frame's keys applied.
    pub fn frame_poses(&self, f: &Frame) -> Result<Vec<Pose>> {
        if !self.is_rig_frame(f) {
            if !f.keys.is_empty() {
                bail!("frame has keys but explicit poses too (keys only apply to rig frames, i.e. frames without poses)");
            }
            return Ok(f.poses.clone());
        }
        let mut poses = self.rig.clone();
        for k in &f.keys {
            let i = poses.iter().position(|p| p.slot() == k.slot).with_context(|| {
                format!("key for unknown slot '{}' (rig slots: {})", k.slot, self.rig.iter().map(|p| p.slot().to_string()).collect::<Vec<_>>().join(", "))
            })?;
            let p = &mut poses[i];
            if p.slot.is_none() {
                p.slot = Some(p.part.clone());
            }
            p.x = p.x.saturating_add(k.dx);
            p.y = p.y.saturating_add(k.dy);
            p.rot += k.rot;
            if let Some(part) = &k.part {
                p.part = part.clone();
            }
            if let Some(h) = k.hide {
                p.hide = h;
            }
            if let Some(v) = k.flip_h {
                p.flip_h = v;
            }
            if let Some(v) = k.flip_v {
                p.flip_v = v;
            }
            if let Some(z) = k.z {
                let pose = poses.remove(i);
                let z = z.min(poses.len());
                poses.insert(z, pose);
            }
        }
        Ok(poses)
    }

    /// Checks parts exist, slots are unique, parents exist and there are no parent cycles.
    pub fn validate_poses(&self, poses: &[Pose], what: &str) -> Result<()> {
        if poses.len() > MAX_POSES {
            bail!("{what}: {} placements (limit {MAX_POSES})", poses.len());
        }
        // slots must be unique where something refers to them: the rig (keys) and frames
        // with parents. Plain explicit frames may place the same part twice (two eyes).
        let unique = what == "rig" || poses.iter().any(|p| p.parent.is_some());
        let mut slots = std::collections::HashSet::new();
        for p in poses {
            self.part(&p.part).with_context(|| format!("{what}: slot '{}'", p.slot()))?;
            check_name("slot", p.slot())?;
            if p.x.unsigned_abs() > MAX_POS || p.y.unsigned_abs() > MAX_POS || !p.rot.is_finite() {
                bail!("{what}: slot '{}' position/rotation out of range (|x|, |y| <= {MAX_POS})", p.slot());
            }
            if !slots.insert(p.slot().to_string()) && unique {
                bail!("{what}: slot '{}' is used twice (give one of them a distinct 'slot' name)", p.slot());
            }
        }
        for p in poses {
            let mut cur = p;
            let mut depth = 0;
            while let Some(par) = &cur.parent {
                cur = poses.iter().find(|q| q.slot() == par).with_context(|| {
                    format!("{what}: slot '{}' has parent '{par}', which is not placed in this frame", p.slot())
                })?;
                depth += 1;
                if depth > poses.len() {
                    bail!("{what}: parent cycle through slot '{}'", p.slot());
                }
            }
        }
        Ok(())
    }

    pub fn validate_frame(&self, f: &Frame) -> Result<()> {
        if f.poses.is_empty() && self.rig.is_empty() && !f.keys.is_empty() {
            bail!("frame has keys but the sprite has no rig (set one with rig_set, or give explicit poses)");
        }
        for k in &f.keys {
            if let Some(p) = &k.part {
                self.part(p).with_context(|| format!("key for slot '{}'", k.slot))?;
            }
        }
        let poses = self.frame_poses(f)?;
        self.validate_poses(&poses, "frame")
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let parts_dir = dir.join("parts");
        std::fs::create_dir_all(&parts_dir).with_context(|| format!("creating {}", parts_dir.display()))?;
        for (name, part) in &self.parts {
            let path = parts_dir.join(format!("{name}.png"));
            let tmp = parts_dir.join(format!(".{name}.png.tmp"));
            part.img.save_with_format(&tmp, image::ImageFormat::Png)?;
            std::fs::rename(&tmp, &path)?;
        }
        // sprite.json last and atomically: a crash never leaves a half-written project file
        let json = serde_json::to_string_pretty(self)?;
        let tmp = dir.join(".sprite.json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, dir.join("sprite.json"))?;
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
