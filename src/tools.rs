use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use image::{Rgba, RgbaImage};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::color::{
    CLEAR, PalEntry, PalSpec, Palette, Px, apply_remap, hue_ramp, median_cut, nearest, outline_shade, palette_from_specs, parse_color, parse_hex, quantize_img,
    to_hex,
};
use crate::draw::{self, Op};
use crate::export;
use crate::model::{Animation, Frame, Key, Part, Pose, Sprite, check_name};
use crate::render;

pub const INSTRUCTIONS: &str = "freesprite is a parts-based pixel-art sprite editor for point-and-click adventure characters (Day of the Tentacle style: \
big expressive heads, rubbery cartoon shapes, cel shading, lip-synced talk).\n\
Concepts: a sprite has a fixed canvas, an origin (the line under the feet, used for placement in the game), a palette of one-char keys grouped into ramps, \
parts (reusable pixel images with a pivot: head, torso, arm_l, mouth_a...) and animations made of frames.\n\
Rig: rig_set defines the rest pose. Every slot places a part, and children (mouth, eyes, hat on the head; head and arms on the torso) hang from their parent's pivot. \
Frames without poses are drawn from the rig plus their keys ({slot, dx, dy, rot, part swap, hide}), so moving the torso moves everything attached to it \
and an animation only lists what changes. Explicit frames (a full poses list) still work.\n\
Workflow: sprite_new -> palette_ramp for each material (skin, hair, clothes; hue-shifted shading) plus an outline key -> \
part_new + draw: block in with curve (closed+fill) / ellipse / polygon, mirror for symmetry, shade for cel shading from the light, \
contour for inner lines, outline (selout) last, then refine single pixels with rows -> rig_set -> anim_generate (idle, walk) and anim_talk (lip-sync from text) \
-> pose_set to refine -> review (silhouette, value study, zoom, strip) and lint after every step -> export for Godot.\n\
DOTT tips: heads about a third of the body height, and readable faces with big eyes and nose. Keep one light direction for every part (shade light default top-left). \
Check the value panel so face and hands separate from clothes. Draw variant parts (arm_bent, head_turned, mouth_*) instead of rotating small detailed parts, \
because rotation suits limbs and noise shows up on small striped shapes. Use anim_copy mirror=true for the other walk direction, and part_get to read exact pixels before patching. \
Every change autosaves; undo/redo exist.";

pub enum Out {
    Text(String),
    Image(RgbaImage),
}

pub struct Doc {
    pub sprite: Sprite,
    pub dir: PathBuf,
    undo: Vec<Sprite>,
    redo: Vec<Sprite>,
}

#[derive(Default)]
pub struct Workspace {
    docs: HashMap<String, Doc>,
}

fn parse<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| anyhow!("invalid arguments: {e}"))
}

pub fn expand(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest);
        }
    PathBuf::from(p)
}

fn png_b64(img: &RgbaImage) -> String {
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
        .expect("png encode");
    B64.encode(buf)
}

fn opt_color(s: &Option<String>, pal: &Palette) -> Result<Option<Px>> {
    match s.as_deref() {
        None | Some("checker") => Ok(None),
        Some(c) => Ok(Some(parse_color(c, pal)?)),
    }
}

fn part_preview(s: &Sprite, name: &str) -> Result<Out> {
    let p = s.part(name)?;
    let k = render::auto_scale(p.img.width(), p.img.height(), 1, 1).clamp(4, 16);
    let mut img = render::present(&p.img, k, None);
    render::marker(&mut img, p.pivot[0], p.pivot[1], k, Rgba([255, 0, 200, 255]));
    Ok(Out::Image(img))
}

/// One frame, scaled up, with the origin (magenta) and every placement's pivot (cyan).
fn pose_sheet(s: &Sprite, f: &Frame) -> Result<Out> {
    let img = render::compose(s, f)?;
    let k = render::auto_scale(s.width, s.height, 1, 1).min(8);
    let mut out = render::present(&img, k, None);
    render::marker(&mut out, s.origin[0] as f32, s.origin[1] as f32, k, Rgba([255, 0, 200, 255]));
    for (_, x) in render::frame_xforms(s, f)? {
        render::marker(&mut out, x.x, x.y, k, Rgba([0, 200, 255, 255]));
    }
    Ok(Out::Image(out))
}

/// The first `max` frames of an animation side by side.
fn anim_strip(s: &Sprite, anim: &str, max: usize) -> Result<Out> {
    let frames = s.anim(anim)?.frames.iter().take(max).map(|f| render::compose(s, f)).collect::<Result<Vec<_>>>()?;
    let k = render::auto_scale(s.width, s.height, frames.len() as u32, 1).min(4);
    Ok(Out::Image(render::grid(&[frames], k, None)))
}

/// Close-up strip around one rig slot (e.g. the mouth), so small changes are visible.
fn slot_strip(s: &Sprite, anim: &str, slot: &str, max: usize) -> Result<Out> {
    let rest = Frame::rig(100, vec![]);
    let (_, x) = render::frame_xforms(s, &rest)?.into_iter().find(|(p, _)| p.slot() == slot).ok_or_else(|| anyhow!("no slot '{slot}'"))?;
    let (w, h) = (24u32.min(s.width), 20u32.min(s.height));
    let cx = (x.x as i64 - w as i64 / 2).clamp(0, (s.width - w) as i64) as u32;
    let cy = (x.y as i64 - h as i64 / 2).clamp(0, (s.height - h) as i64) as u32;
    let frames = s
        .anim(anim)?
        .frames
        .iter()
        .take(max)
        .map(|f| render::compose(s, f).map(|img| image::imageops::crop_imm(&img, cx, cy, w, h).to_image()))
        .collect::<Result<Vec<_>>>()?;
    let rows: Vec<Vec<RgbaImage>> = frames.chunks(12).map(|c| c.to_vec()).collect();
    Ok(Out::Image(render::grid(&rows, 5, None)))
}

fn quantize_to(img: &mut RgbaImage, pal: &[Px]) -> Result<()> {
    if pal.is_empty() {
        bail!("palette is empty; set one first (palette_set / palette_from_image)");
    }
    if pal.iter().all(|c| c[3] == 0) {
        bail!("palette is empty; set one first (palette_set / palette_from_image)");
    }
    for p in img.pixels_mut() {
        if p[3] > 0 {
            let a = p[3];
            let mut c = pal[nearest(*p, pal).unwrap()];
            c[3] = a;
            *p = c;
        }
    }
    Ok(())
}

fn threshold(img: &mut RgbaImage, t: u8) {
    for p in img.pixels_mut() {
        if p[3] < t {
            *p = CLEAR;
        } else {
            p[3] = 255;
        }
    }
}

// ---------- argument structs ----------

#[derive(Deserialize)]
struct SpriteNew {
    name: String,
    dir: String,
    width: u32,
    height: u32,
    origin: Option<[i32; 2]>,
    #[serde(default)]
    palette: Vec<PalSpec>,
}
#[derive(Deserialize)]
struct DirArg {
    dir: String,
}
#[derive(Deserialize)]
struct SpriteEdit {
    sprite: String,
    width: Option<u32>,
    height: Option<u32>,
    origin: Option<[i32; 2]>,
    #[serde(default)]
    offset: [i32; 2],
    portrait: Option<[u32; 4]>,
}
#[derive(Deserialize)]
struct PaletteSet {
    sprite: String,
    colors: Vec<PalSpec>,
}
#[derive(Deserialize)]
struct PaletteVariant {
    sprite: String,
    name: String,
    #[serde(default)]
    colors: Vec<PalSpec>,
    #[serde(default)]
    remove: bool,
}
#[derive(Deserialize)]
struct PaletteFromImage {
    sprite: String,
    image: String,
    colors: usize,
    region: Option<[u32; 4]>,
    #[serde(default)]
    append: bool,
    key_color: Option<String>,
    key_tolerance: Option<i32>,
}
#[derive(Deserialize)]
struct PartNew {
    sprite: String,
    part: String,
    width: u32,
    height: u32,
    pivot: Option<[f32; 2]>,
}
#[derive(Deserialize)]
struct PartEdit {
    sprite: String,
    part: String,
    pivot: Option<[f32; 2]>,
    rename: Option<String>,
    resize: Option<Resize>,
}
#[derive(Deserialize)]
struct Resize {
    width: u32,
    height: u32,
    #[serde(default)]
    offset_x: i32,
    #[serde(default)]
    offset_y: i32,
}
#[derive(Deserialize)]
struct PartRef {
    sprite: String,
    part: String,
}
#[derive(Deserialize)]
struct PartGet {
    sprite: String,
    part: String,
    region: Option<[u32; 4]>,
}
#[derive(Deserialize)]
struct PartDuplicate {
    sprite: String,
    part: String,
    new_name: String,
    #[serde(default)]
    flip_h: bool,
    #[serde(default)]
    flip_v: bool,
}
#[derive(Deserialize)]
struct PartImport {
    sprite: String,
    part: String,
    image: String,
    region: Option<[u32; 4]>,
    size: Option<[u32; 2]>,
    #[serde(default)]
    quantize: bool,
    alpha_threshold: Option<u8>,
    key_color: Option<String>,
    key_tolerance: Option<i32>,
    #[serde(default = "yes")]
    key_spill: bool,
    #[serde(default)]
    trim: bool,
    #[serde(default)]
    drop_islands: bool,
    downscale: Option<String>,
    ink: Option<f32>,
    ink_level: Option<f32>,
    pivot: Option<[f32; 2]>,
}

/// "box" (default) or "lineart" resizing; validated so typos don't silently fall back.
fn resize_with(img: &RgbaImage, w: u32, h: u32, mode: Option<&str>, ink: Option<f32>, ink_level: Option<f32>) -> Result<RgbaImage> {
    match mode.unwrap_or("box") {
        "box" => Ok(render::box_resize(img, w, h)),
        "lineart" => {
            let mut o = render::LineArt::default();
            if let Some(v) = ink {
                o.ink = v.clamp(0.01, 1.0);
            }
            if let Some(v) = ink_level {
                o.ink_level = v.clamp(0.0, 1.0);
            }
            Ok(render::lineart_resize(img, w, h, o))
        }
        m => bail!("downscale must be 'box' or 'lineart', got '{m}'"),
    }
}
#[derive(Deserialize)]
struct PartExtract {
    sprite: String,
    from: String,
    part: String,
    region: [u32; 4],
    #[serde(default)]
    cut: bool,
    pivot: Option<[f32; 2]>,
}
#[derive(Deserialize)]
struct DrawArgs {
    sprite: String,
    part: String,
    ops: Vec<Op>,
    #[serde(default = "yes")]
    preview: bool,
}
fn yes() -> bool {
    true
}
#[derive(Deserialize)]
struct AnimSet {
    sprite: String,
    anim: String,
    #[serde(rename = "loop", default = "yes")]
    looped: bool,
    frames: Vec<Frame>,
}
#[derive(Deserialize)]
struct FrameSet {
    sprite: String,
    anim: String,
    index: usize,
    frame: Frame,
    #[serde(default)]
    insert: bool,
}
#[derive(Deserialize)]
struct FrameClone {
    sprite: String,
    anim: String,
    index: usize,
    to: Option<usize>,
    #[serde(default = "one")]
    count: usize,
}
fn one() -> usize {
    1
}
#[derive(Deserialize)]
struct ReviewArgs {
    sprite: String,
    part: Option<String>,
    anim: Option<String>,
    #[serde(default)]
    frame: usize,
    zoom: Option<u32>,
    #[serde(default)]
    grid_text: bool,
    palette: Option<String>,
}
#[derive(Deserialize)]
struct LintArgs {
    sprite: String,
    outline: Option<String>,
}
#[derive(Deserialize)]
struct FrameDelete {
    sprite: String,
    anim: String,
    index: usize,
}
#[derive(Deserialize)]
struct PoseSet {
    sprite: String,
    anim: String,
    #[serde(default)]
    frames: Vec<usize>,
    /// explicit frames: the part to change; rig frames: the slot (same as `slot`)
    part: Option<String>,
    slot: Option<String>,
    x: Option<i32>,
    y: Option<i32>,
    #[serde(default)]
    dx: i32,
    #[serde(default)]
    dy: i32,
    rot: Option<f32>,
    flip_h: Option<bool>,
    flip_v: Option<bool>,
    #[serde(default)]
    remove: bool,
    z: Option<usize>,
    replace_with: Option<String>,
}
#[derive(Deserialize)]
struct AnimRef {
    sprite: String,
    anim: String,
}
#[derive(Deserialize)]
struct AnimCopy {
    sprite: String,
    from: String,
    to: String,
    #[serde(default)]
    mirror: bool,
    #[serde(default)]
    reverse: bool,
}
#[derive(Deserialize)]
struct RenderArgs {
    sprite: String,
    part: Option<String>,
    anim: Option<String>,
    frame: Option<usize>,
    scale: Option<u32>,
    #[serde(default)]
    onion: bool,
    background: Option<String>,
    #[serde(default)]
    markers: bool,
    scene: Option<Scene>,
    palette: Option<String>,
}
#[derive(Deserialize)]
struct Scene {
    image: String,
    x: i32,
    y: i32,
}
#[derive(Deserialize)]
struct ExportArgs {
    sprite: String,
    dir: String,
    godot_res_dir: Option<String>,
    #[serde(default = "yes")]
    gif: bool,
}
#[derive(Deserialize)]
struct GifArgs {
    sprite: String,
    anim: String,
    path: String,
    scale: Option<u32>,
    background: Option<String>,
    palette: Option<String>,
    scene: Option<GifSceneArgs>,
}
#[derive(Deserialize)]
struct GifSceneArgs {
    image: String,
    x: i32,
    y: i32,
    #[serde(default)]
    step: [i32; 2],
    #[serde(default = "one_u32")]
    loops: u32,
}
fn one_u32() -> u32 {
    1
}

#[derive(Deserialize)]
struct Pixelate {
    image: String,
    output: String,
    width: u32,
    height: Option<u32>,
    colors: Option<usize>,
    sprite_palette: Option<String>,
    dither: Option<String>,
    dither_strength: Option<f32>,
    downscale: Option<String>,
    ink: Option<f32>,
    ink_level: Option<f32>,
}
#[derive(Deserialize)]
struct SpriteRef {
    sprite: String,
}
#[derive(Deserialize)]
struct RigSet {
    sprite: String,
    poses: Vec<Pose>,
}
#[derive(Deserialize)]
struct PaletteRamp {
    sprite: String,
    name: String,
    base: String,
    keys: String,
    base_index: Option<usize>,
    hue_shift: Option<f32>,
    step: Option<f32>,
    /// extra key for a deep colored outline shade in the same ramp (what selout picks)
    outline: Option<String>,
}
#[derive(Deserialize)]
struct AnimTalk {
    sprite: String,
    text: String,
    anim: Option<String>,
    slot: Option<String>,
    #[serde(default)]
    visemes: HashMap<String, String>,
    ms: Option<u32>,
    head: Option<String>,
    #[serde(rename = "loop", default)]
    looped: bool,
}
#[derive(Deserialize)]
struct Blink {
    slot: String,
    /// swap to this part (eyes -> eyes_closed); without it the slot is shown (lids hidden in the rig)
    part: Option<String>,
}
#[derive(Deserialize)]
struct AnimGenerate {
    sprite: String,
    preset: String,
    anim: Option<String>,
    ms: Option<u32>,
    frames: Option<usize>,
    // idle
    body: Option<Vec<String>>,
    #[serde(default)]
    lag: Vec<String>,
    blink: Option<Blink>,
    // walk
    legs: Option<[String; 2]>,
    arms: Option<[String; 2]>,
    swing: Option<f32>,
    arm_swing: Option<f32>,
}

impl Workspace {
    pub fn call(&mut self, name: &str, args: Value) -> Value {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.dispatch(name, args)));
        let res = res.unwrap_or_else(|p| {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            Err(anyhow!("internal error: {msg}"))
        });
        match res {
            Ok(outs) => {
                let content: Vec<Value> = outs
                    .into_iter()
                    .map(|o| match o {
                        Out::Text(t) => json!({"type": "text", "text": t}),
                        Out::Image(img) => json!({"type": "image", "data": png_b64(&img), "mimeType": "image/png"}),
                    })
                    .collect();
                json!({"content": content, "isError": false})
            }
            Err(e) => json!({"content": [{"type": "text", "text": format!("error: {e:#}")}], "isError": true}),
        }
    }

    fn doc(&mut self, name: &str) -> Result<&mut Doc> {
        if !self.docs.contains_key(name) {
            let dir = expand(name);
            if dir.join("sprite.json").exists() {
                // already open under its name? reuse it (keeps the undo history)
                if let Ok(canon) = std::fs::canonicalize(&dir) {
                    let hit = self.docs.iter().find(|(_, d)| std::fs::canonicalize(&d.dir).is_ok_and(|c| c == canon)).map(|(k, _)| k.clone());
                    if let Some(k) = hit {
                        return Ok(self.docs.get_mut(&k).unwrap());
                    }
                }
                let sprite = Sprite::load(&dir)?;
                let n = sprite.name.clone();
                if let Some(other) = self.docs.get(&n) {
                    bail!(
                        "a different sprite named '{n}' is already open from {}; give one of them another name (sprite_open replaces it explicitly)",
                        other.dir.display()
                    );
                }
                self.docs.insert(n.clone(), Doc { sprite, dir, undo: vec![], redo: vec![] });
                return Ok(self.docs.get_mut(&n).unwrap());
            }
            bail!(
                "no open sprite '{name}' (open: [{}]); use sprite_open or sprite_new",
                self.docs.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        Ok(self.docs.get_mut(name).unwrap())
    }

    fn mutate(&mut self, name: &str, f: impl FnOnce(&mut Sprite) -> Result<Vec<Out>>) -> Result<Vec<Out>> {
        let doc = self.doc(name)?;
        let backup = doc.sprite.clone();
        // a panic inside f must not leave a half-applied change in memory
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut doc.sprite))).unwrap_or_else(|p| {
            let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            Err(anyhow!("internal error: {msg}"))
        });
        match res {
            Ok(o) => {
                if let Err(e) = doc.sprite.save(&doc.dir) {
                    doc.sprite = backup;
                    // put the files back in line with the restored sprite (best effort)
                    let _ = doc.sprite.save(&doc.dir);
                    return Err(e.context("saving failed; the change was rolled back"));
                }
                doc.undo.push(backup);
                if doc.undo.len() > 200 {
                    doc.undo.remove(0);
                }
                doc.redo.clear();
                Ok(o)
            }
            Err(e) => {
                doc.sprite = backup;
                Err(e)
            }
        }
    }

    fn info(s: &Sprite, dir: &std::path::Path) -> String {
        let mut t = format!(
            "sprite '{}' at {}\ncanvas {}x{}, origin {:?}\npalette ({}): {}\nparts:\n",
            s.name,
            dir.display(),
            s.width,
            s.height,
            s.origin,
            s.palette.len(),
            s.palette.text()
        );
        if let Some(p) = s.portrait {
            t += &format!("portrait crop {p:?}\n");
        }
        for (n, p) in &s.parts {
            t += &format!("  {n}: {}x{} pivot {:?}\n", p.img.width(), p.img.height(), p.pivot);
        }
        if !s.rig.is_empty() {
            t += "rig (rest pose, back to front):\n";
            for p in &s.rig {
                t += &format!("  {}\n", serde_json::to_string(p).unwrap_or_default());
            }
        }
        t += "animations:\n";
        for (n, a) in &s.animations {
            let d: Vec<String> = a.frames.iter().map(|f| f.duration_ms.to_string()).collect();
            let rig = a.frames.iter().filter(|f| s.is_rig_frame(f)).count();
            t += &format!(
                "  {n}: {} frames{}, loop={}, durations ms [{}]\n",
                a.frames.len(),
                if rig > 0 { format!(" ({rig} rig frames)") } else { String::new() },
                a.looped,
                d.join(",")
            );
        }
        t
    }

    /// Re-validates every animation against the current parts and rig.
    fn check_anims(s: &Sprite) -> Result<()> {
        for (n, a) in &s.animations {
            for (i, f) in a.frames.iter().enumerate() {
                s.validate_frame(f).map_err(|e| anyhow!("animation '{n}' frame {i}: {e:#}"))?;
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, name: &str, args: Value) -> Result<Vec<Out>> {
        match name {
            "sprite_new" => {
                let a: SpriteNew = parse(args)?;
                check_name("sprite", &a.name)?;
                if a.width == 0 || a.height == 0 || a.width > 1024 || a.height > 1024 {
                    bail!("canvas must be 1..=1024 pixels per side");
                }
                let dir = expand(&a.dir);
                if dir.join("sprite.json").exists() {
                    bail!("{} already contains a sprite; use sprite_open", dir.display());
                }
                let palette = palette_from_specs(&a.palette)?;
                let sprite = Sprite {
                    name: a.name.clone(),
                    width: a.width,
                    height: a.height,
                    origin: a.origin.unwrap_or([a.width as i32 / 2, a.height as i32]),
                    palette,
                    portrait: None,
                    palette_variants: Default::default(),
                    parts: Default::default(),
                    rig: vec![],
                    animations: Default::default(),
                };
                sprite.save(&dir)?;
                let t = Self::info(&sprite, &dir);
                self.docs.insert(a.name, Doc { sprite, dir, undo: vec![], redo: vec![] });
                Ok(vec![Out::Text(format!("created\n{t}"))])
            }
            "sprite_open" => {
                let a: DirArg = parse(args)?;
                let dir = expand(&a.dir);
                let sprite = Sprite::load(&dir)?;
                let t = Self::info(&sprite, &dir);
                self.docs.insert(sprite.name.clone(), Doc { sprite, dir, undo: vec![], redo: vec![] });
                Ok(vec![Out::Text(t)])
            }
            "sprite_info" => {
                let a: SpriteRef = parse(args)?;
                let d = self.doc(&a.sprite)?;
                Ok(vec![Out::Text(Self::info(&d.sprite, &d.dir))])
            }
            "sprite_edit" => {
                let a: SpriteEdit = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    if let Some(o) = a.origin {
                        s.origin = o;
                    }
                    if let Some(p) = a.portrait {
                        s.portrait = Some(p);
                    }
                    let (w, h) = (a.width.unwrap_or(s.width), a.height.unwrap_or(s.height));
                    if w == 0 || h == 0 || w > 1024 || h > 1024 {
                        bail!("canvas must be 1..=1024 pixels per side");
                    }
                    s.width = w;
                    s.height = h;
                    let [ox, oy] = a.offset;
                    if ox.unsigned_abs() > 4096 || oy.unsigned_abs() > 4096 {
                        bail!("offset out of range");
                    }
                    if ox != 0 || oy != 0 {
                        s.origin[0] += ox;
                        s.origin[1] += oy;
                        // only roots: children are offsets from their parent and move with it
                        let shift = |p: &mut Pose| {
                            if p.parent.is_none() {
                                p.x += ox;
                                p.y += oy;
                            }
                        };
                        s.rig.iter_mut().for_each(shift);
                        for an in s.animations.values_mut() {
                            for f in &mut an.frames {
                                f.poses.iter_mut().for_each(shift);
                            }
                        }
                    }
                    if let Some(o) = a.origin
                        && (o[0].unsigned_abs() > 8192 || o[1].unsigned_abs() > 8192)
                    {
                        bail!("origin out of range");
                    }
                    Ok(vec![Out::Text(format!("canvas {}x{}, origin {:?}", s.width, s.height, s.origin))])
                })
            }
            "palette_set" => {
                let a: PaletteSet = parse(args)?;
                let pal = palette_from_specs(&a.colors)?;
                self.mutate(&a.sprite, |s| {
                    let old = std::mem::replace(&mut s.palette, pal);
                    // palette swap: pixels whose key survives get the new color
                    let mut remap = vec![];
                    for e in &old.0 {
                        if let Some(n) = s.palette.by_key(e.key)
                            && n != e.color {
                                remap.push((e.color, n));
                            }
                    }
                    let mut changed = 0;
                    if !remap.is_empty() {
                        for p in s.parts.values_mut() {
                            for px in p.img.pixels_mut() {
                                if let Some((_, n)) = remap.iter().find(|(o, _)| o == px) {
                                    *px = *n;
                                    changed += 1;
                                }
                            }
                        }
                    }
                    Ok(vec![Out::Text(format!(
                        "palette: {}{}",
                        s.palette.text(),
                        if changed > 0 { format!("\nrecolored {changed} pixels (keys kept, colors changed)") } else { String::new() }
                    ))])
                })
            }
            "palette_variant" => {
                let a: PaletteVariant = parse(args)?;
                check_name("variant", &a.name)?;
                self.mutate(&a.sprite, |s| {
                    if a.remove {
                        s.palette_variants.remove(&a.name).ok_or_else(|| anyhow!("no variant '{}'", a.name))?;
                        return Ok(vec![Out::Text(format!("removed variant '{}'", a.name))]);
                    }
                    // start from the existing variant (or the base palette) and override given keys
                    let mut v = s.palette_variants.get(&a.name).cloned().unwrap_or_else(|| s.palette.clone());
                    let over = palette_from_specs(&a.colors)?;
                    for e in over.0 {
                        if !a.colors.iter().any(|c| matches!(c, PalSpec::Full { key: Some(_), .. }) || matches!(c, PalSpec::Hex(h) if h.contains('='))) {
                            bail!("variant colors need explicit keys ('k=#hex' or {{key, color}})");
                        }
                        match v.0.iter_mut().find(|x| x.key == e.key) {
                            Some(x) => x.color = e.color,
                            None => bail!("key '{}' is not in the base palette", e.key),
                        }
                    }
                    let t = v.text();
                    s.palette_variants.insert(a.name.clone(), v);
                    Ok(vec![Out::Text(format!(
                        "variant '{}': {t}\nuse palette='{}' in render/review/export_gif; export writes a sheet per variant",
                        a.name, a.name
                    ))])
                })
            }
            "palette_from_image" => {
                let a: PaletteFromImage = parse(args)?;
                let mut img = image::open(expand(&a.image))?.to_rgba8();
                if let Some([x, y, w, h]) = a.region {
                    img = image::imageops::crop_imm(&img, x, y, w, h).to_image();
                }
                if let Some(k) = &a.key_color {
                    // background out first, so it neither becomes a palette color nor bleeds into edges
                    let kc = parse_hex(k).map_err(|_| anyhow!("key_color must be #hex here (the sprite's palette is being replaced)"))?;
                    render::key_out(&mut img, kc, a.key_tolerance.unwrap_or(40).clamp(0, 765), true);
                }
                // subsample large images for speed
                while img.width() * img.height() > 400_000 {
                    img = render::box_resize(&img, img.width() / 2, img.height() / 2);
                }
                let px: Vec<[u8; 3]> = img.pixels().filter(|p| p[3] > 127).map(|p| [p[0], p[1], p[2]]).collect();
                let cols = median_cut(px, a.colors.clamp(1, 62));
                self.mutate(&a.sprite, |s| {
                    if a.append {
                        for c in cols {
                            if s.palette.entry_of(c).is_none() && !s.palette.push(c, None, None) {
                                bail!("palette full");
                            }
                        }
                    } else {
                        s.palette = Palette::from_colors(cols);
                    }
                    let colors = s.palette.colors();
                    let swatch = RgbaImage::from_fn(colors.len().max(1) as u32, 1, |x, _| colors.get(x as usize).copied().unwrap_or(CLEAR));
                    Ok(vec![
                        Out::Text(format!("palette ({}): {}", s.palette.len(), s.palette.text())),
                        Out::Image(render::upscale(&swatch, 24)),
                    ])
                })
            }
            "part_new" => {
                let a: PartNew = parse(args)?;
                check_name("part", &a.part)?;
                if a.width == 0 || a.height == 0 || a.width > 1024 || a.height > 1024 {
                    bail!("part must be 1..=1024 pixels per side");
                }
                self.mutate(&a.sprite, |s| {
                    if s.parts.contains_key(&a.part) {
                        bail!("part '{}' exists (use part_edit / draw clear / part_delete)", a.part);
                    }
                    let pivot = a.pivot.unwrap_or([a.width as f32 / 2.0, a.height as f32 / 2.0]);
                    s.parts.insert(a.part.clone(), Part { pivot, img: RgbaImage::new(a.width, a.height) });
                    Ok(vec![Out::Text(format!("part '{}' {}x{} pivot {:?}", a.part, a.width, a.height, pivot))])
                })
            }
            "part_edit" => {
                let a: PartEdit = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    let mut name = a.part.clone();
                    if let Some(new) = &a.rename {
                        check_name("part", new)?;
                        if s.parts.contains_key(new) {
                            bail!("part '{new}' already exists");
                        }
                        let p = s.parts.remove(&a.part).ok_or_else(|| anyhow!("no part '{}'", a.part))?;
                        s.parts.insert(new.clone(), p);
                        // the slot name stays the old part name so parents and keys keep working
                        let rename = |p: &mut Pose| {
                            if p.part == a.part {
                                if p.slot.is_none() {
                                    p.slot = Some(a.part.clone());
                                }
                                p.part = new.clone();
                            }
                        };
                        s.rig.iter_mut().for_each(rename);
                        for an in s.animations.values_mut() {
                            for f in &mut an.frames {
                                f.poses.iter_mut().for_each(rename);
                                for k in &mut f.keys {
                                    if k.part.as_deref() == Some(a.part.as_str()) {
                                        k.part = Some(new.clone());
                                    }
                                }
                            }
                        }
                        name = new.clone();
                    }
                    let p = s.part_mut(&name)?;
                    if let Some(r) = &a.resize {
                        if r.width == 0 || r.height == 0 || r.width > 1024 || r.height > 1024 {
                            bail!("part must be 1..=1024 pixels per side");
                        }
                        let mut img = RgbaImage::new(r.width, r.height);
                        image::imageops::replace(&mut img, &p.img, r.offset_x as i64, r.offset_y as i64);
                        p.img = img;
                        p.pivot[0] += r.offset_x as f32;
                        p.pivot[1] += r.offset_y as f32;
                    }
                    if let Some(pv) = a.pivot {
                        p.pivot = pv;
                    }
                    let t = format!("part '{name}' {}x{} pivot {:?}", p.img.width(), p.img.height(), p.pivot);
                    Ok(vec![Out::Text(t), part_preview(s, &name)?])
                })
            }
            "part_delete" => {
                let a: PartRef = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    s.part(&a.part)?;
                    let mut used: Vec<String> = s
                        .animations
                        .iter()
                        .filter(|(_, an)| {
                            an.frames.iter().any(|f| f.poses.iter().any(|p| p.part == a.part) || f.keys.iter().any(|k| k.part.as_deref() == Some(a.part.as_str())))
                        })
                        .map(|(n, _)| format!("animation '{n}'"))
                        .collect();
                    if s.rig.iter().any(|p| p.part == a.part) {
                        used.insert(0, "the rig".into());
                    }
                    if !used.is_empty() {
                        bail!("part '{}' is used by {}", a.part, used.join(", "));
                    }
                    s.parts.remove(&a.part);
                    Ok(vec![Out::Text(format!("deleted part '{}'", a.part))])
                })
            }
            "part_duplicate" => {
                let a: PartDuplicate = parse(args)?;
                check_name("part", &a.new_name)?;
                self.mutate(&a.sprite, |s| {
                    if s.parts.contains_key(&a.new_name) {
                        bail!("part '{}' already exists", a.new_name);
                    }
                    let mut p = s.part(&a.part)?.clone();
                    if a.flip_h {
                        image::imageops::flip_horizontal_in_place(&mut p.img);
                        p.pivot[0] = p.img.width() as f32 - p.pivot[0];
                    }
                    if a.flip_v {
                        image::imageops::flip_vertical_in_place(&mut p.img);
                        p.pivot[1] = p.img.height() as f32 - p.pivot[1];
                    }
                    s.parts.insert(a.new_name.clone(), p);
                    Ok(vec![Out::Text(format!("'{}' -> '{}'", a.part, a.new_name)), part_preview(s, &a.new_name)?])
                })
            }
            "part_import" => {
                let a: PartImport = parse(args)?;
                check_name("part", &a.part)?;
                let mut img = image::open(expand(&a.image))?.to_rgba8();
                if let Some([x, y, w, h]) = a.region {
                    img = image::imageops::crop_imm(&img, x, y, w, h).to_image();
                }
                self.mutate(&a.sprite, |s| {
                    if let Some(k) = &a.key_color {
                        let kc = parse_color(k, &s.palette)?;
                        render::key_out(&mut img, kc, a.key_tolerance.unwrap_or(40).clamp(0, 765), a.key_spill);
                    }
                    if a.drop_islands {
                        // pieces under 2% of the main figure: neighbours on a model sheet, specks
                        render::drop_islands(&mut img, 0.02);
                    }
                    if a.trim {
                        let (w, h) = img.dimensions();
                        let opaque = |x: u32, y: u32| img.get_pixel(x, y)[3] > 127;
                        let ys: Vec<u32> = (0..h).filter(|y| (0..w).any(|x| opaque(x, *y))).collect();
                        let xs: Vec<u32> = (0..w).filter(|x| (0..h).any(|y| opaque(*x, y))).collect();
                        let (Some(y0), Some(y1), Some(x0), Some(x1)) = (ys.first(), ys.last(), xs.first(), xs.last()) else {
                            bail!("nothing left to import after keying (check key_color / region)");
                        };
                        img = image::imageops::crop_imm(&img, *x0, *y0, x1 - x0 + 1, y1 - y0 + 1).to_image();
                    }
                    if let Some([w, h]) = a.size {
                        let (sw, sh) = img.dimensions();
                        // a 0 keeps the aspect ratio
                        let (w, h) = match (w, h) {
                            (0, 0) => bail!("size needs at least one non-zero side"),
                            (0, h) => (((sw as u64 * h as u64) / sh.max(1) as u64).max(1) as u32, h),
                            (w, 0) => (w, ((sh as u64 * w as u64) / sw.max(1) as u64).max(1) as u32),
                            wh => wh,
                        };
                        if w > 1024 || h > 1024 {
                            bail!("size must be 1..=1024");
                        }
                        img = resize_with(&img, w, h, a.downscale.as_deref(), a.ink, a.ink_level)?;
                    } else if a.downscale.is_some() {
                        bail!("downscale needs size");
                    }
                    if img.width() > 1024 || img.height() > 1024 {
                        bail!("imported image is {}x{}; pass size to scale it down", img.width(), img.height());
                    }
                    threshold(&mut img, a.alpha_threshold.unwrap_or(128));
                    if a.quantize {
                        quantize_to(&mut img, &s.palette.colors())?;
                    }
                    let (w, h) = img.dimensions();
                    // replacing a part keeps where its pivot sits (scaled to the new size), so the
                    // rig and animations don't jump; a new part gets the center
                    let pivot = a.pivot.unwrap_or_else(|| match s.parts.get(&a.part) {
                        Some(old) => {
                            let (ow, oh) = (old.img.width().max(1) as f32, old.img.height().max(1) as f32);
                            [(old.pivot[0] / ow * w as f32).round(), (old.pivot[1] / oh * h as f32).round()]
                        }
                        None => [w as f32 / 2.0, h as f32 / 2.0],
                    });
                    s.parts.insert(a.part.clone(), Part { pivot, img });
                    Ok(vec![Out::Text(format!("imported '{}' {w}x{h} pivot {pivot:?}", a.part)), part_preview(s, &a.part)?])
                })
            }
            "part_extract" => {
                let a: PartExtract = parse(args)?;
                check_name("part", &a.part)?;
                self.mutate(&a.sprite, |s| {
                    let [x, y, w, h] = a.region;
                    if w == 0 || h == 0 {
                        bail!("region must be non-empty");
                    }
                    let src = s.part_mut(&a.from)?;
                    let img = image::imageops::crop_imm(&src.img, x, y, w, h).to_image();
                    if a.cut {
                        for py in y..(y + h).min(src.img.height()) {
                            for px in x..(x + w).min(src.img.width()) {
                                src.img.put_pixel(px, py, CLEAR);
                            }
                        }
                    }
                    let pivot = a.pivot.unwrap_or([img.width() as f32 / 2.0, img.height() as f32 / 2.0]);
                    s.parts.insert(a.part.clone(), Part { pivot, img });
                    Ok(vec![Out::Text(format!("extracted '{}' from '{}'", a.part, a.from)), part_preview(s, &a.part)?])
                })
            }
            "part_get" => {
                let a: PartGet = parse(args)?;
                let d = self.doc(&a.sprite)?;
                let p = d.sprite.part(&a.part)?;
                let (rows, off) = draw::to_rows(&p.img, &d.sprite.palette, a.region);
                let mut t = format!(
                    "part '{}' {}x{} pivot {:?}{}\npalette: {}\n'.' = transparent{}\n```\n{}\n```",
                    a.part,
                    p.img.width(),
                    p.img.height(),
                    p.pivot,
                    a.region.map(|r| format!(", region {r:?} (patch with rows op at x={}, y={})", r[0], r[1])).unwrap_or_default(),
                    d.sprite.palette.text(),
                    if off > 0 { format!(", '?' = off-palette color ({off} px)") } else { String::new() },
                    rows.join("\n")
                );
                if off > 0 {
                    t += "\n(use draw with a replace op or part_import quantize to snap to palette)";
                }
                Ok(vec![Out::Text(t), part_preview(&d.sprite, &a.part)?])
            }
            "draw" => {
                let a: DrawArgs = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    let pal = s.palette.clone();
                    let p = s.part_mut(&a.part)?;
                    let off_palette = |img: &RgbaImage| if pal.is_empty() { 0 } else { img.pixels().filter(|px| px[3] > 0 && pal.entry_of(**px).is_none()).count() };
                    let before = off_palette(&p.img);
                    for (i, op) in a.ops.iter().enumerate() {
                        draw::apply(&mut p.img, op, &pal).map_err(|e| anyhow!("op {i}: {e}"))?;
                    }
                    let after = off_palette(&p.img);
                    let mut msg = format!("applied {} ops to '{}'", a.ops.len(), a.part);
                    if after > before {
                        msg += &format!(
                            "\nwarning: {} new off-palette pixels (a #hex color or legend that isn't a palette key); add it to the palette or use a key",
                            after - before
                        );
                    }
                    let mut out = vec![Out::Text(msg)];
                    if a.preview {
                        out.push(part_preview(s, &a.part)?);
                    }
                    Ok(out)
                })
            }
            "anim_set" => {
                let a: AnimSet = parse(args)?;
                check_name("animation", &a.anim)?;
                self.mutate(&a.sprite, |s| {
                    for f in &a.frames {
                        s.validate_frame(f)?;
                    }
                    let n = a.frames.len();
                    s.animations.insert(a.anim.clone(), Animation { looped: a.looped, frames: a.frames });
                    Ok(vec![Out::Text(format!("animation '{}' set: {n} frames (render it to check)", a.anim))])
                })
            }
            "frame_set" => {
                let a: FrameSet = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    s.validate_frame(&a.frame)?;
                    let an = s.anim_mut(&a.anim)?;
                    if a.insert || a.index == an.frames.len() {
                        if a.index > an.frames.len() {
                            bail!("index {} out of range (0..={})", a.index, an.frames.len());
                        }
                        an.frames.insert(a.index, a.frame);
                    } else {
                        let len = an.frames.len();
                        *an.frames.get_mut(a.index).ok_or_else(|| anyhow!("index {} out of range ({len} frames)", a.index))? = a.frame;
                    }
                    Ok(vec![Out::Text(format!("'{}' now has {} frames", a.anim, an.frames.len()))])
                })
            }
            "frame_clone" => {
                let a: FrameClone = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    let an = s.anim_mut(&a.anim)?;
                    let f = an.frames.get(a.index).cloned().ok_or_else(|| anyhow!("frame {} out of range", a.index))?;
                    let to = a.to.unwrap_or(a.index + 1).min(an.frames.len());
                    for _ in 0..a.count.clamp(1, 64) {
                        an.frames.insert(to, f.clone());
                    }
                    Ok(vec![Out::Text(format!("'{}' now has {} frames", a.anim, an.frames.len()))])
                })
            }
            "review" => {
                let a: ReviewArgs = parse(args)?;
                let d = self.doc(&a.sprite)?;
                let s = &d.sprite;
                let remap = s.variant_remap(a.palette.as_deref())?;
                let (mut img, mut strip, label, origin) = if let Some(part) = &a.part {
                    let p = s.part(part)?;
                    (p.img.clone(), None, format!("part '{part}'"), Some(p.pivot))
                } else {
                    let an_name = match &a.anim {
                        Some(n) => n.clone(),
                        None => s.animations.keys().next().cloned().ok_or_else(|| anyhow!("no animations; pass part"))?,
                    };
                    let an = s.anim(&an_name)?;
                    let frames = an.frames.iter().map(|f| render::compose(s, f)).collect::<Result<Vec<_>>>()?;
                    let img = frames.get(a.frame).cloned().ok_or_else(|| anyhow!("frame {} out of range ({} frames)", a.frame, frames.len()))?;
                    (img, Some(frames), format!("'{an_name}' frame {}", a.frame), Some([s.origin[0] as f32, s.origin[1] as f32]))
                };
                apply_remap(&mut img, &remap);
                if let Some(st) = strip.as_mut() {
                    st.iter_mut().for_each(|i| apply_remap(i, &remap));
                }
                let sheet = render::review(&img, strip.as_deref(), a.zoom, origin);
                let mut out = vec![Out::Text(format!(
                    "review of {label}: [1:1] [silhouette 2x] [zoom with grid, labels every 4px, magenta = pivot/origin]{}",
                    if strip.is_some() { " / bottom: all frames 3x, left to right" } else { "" }
                ))];
                if a.grid_text {
                    let (rows, off) = draw::to_rows(&img, &s.palette, None);
                    out.push(Out::Text(format!(
                        "pixels{}:\n```\n{}\n```",
                        if off > 0 { format!(" ({off} off-palette '?')") } else { String::new() },
                        rows.join("\n")
                    )));
                }
                out.push(Out::Image(sheet));
                Ok(out)
            }
            "lint" => {
                let a: LintArgs = parse(args)?;
                let d = self.doc(&a.sprite)?;
                let s = &d.sprite;
                let outline = match &a.outline {
                    Some(c) => Some(parse_color(c, &s.palette)?),
                    None => None,
                };
                Ok(vec![Out::Text(crate::lint::lint(s, outline)?)])
            }
            "frame_delete" => {
                let a: FrameDelete = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    let an = s.anim_mut(&a.anim)?;
                    if a.index >= an.frames.len() {
                        bail!("index {} out of range ({} frames)", a.index, an.frames.len());
                    }
                    an.frames.remove(a.index);
                    Ok(vec![Out::Text(format!("'{}' now has {} frames", a.anim, an.frames.len()))])
                })
            }
            "pose_set" => {
                let a: PoseSet = parse(args)?;
                let target = a.slot.clone().or(a.part.clone()).ok_or_else(|| anyhow!("pass part (explicit frames) or slot (rig frames)"))?;
                let lim = crate::model::MAX_POS as i32;
                if [a.x.unwrap_or(0), a.y.unwrap_or(0), a.dx, a.dy].iter().any(|v| v.unsigned_abs() > lim as u32) || a.rot.is_some_and(|r| !r.is_finite()) {
                    bail!("x/y/dx/dy must be within ±{lim} and rot finite");
                }
                self.mutate(&a.sprite, |s| {
                    if let Some(r) = &a.replace_with {
                        s.part(r)?;
                    }
                    let rig = s.rig.clone();
                    let rig_frame: Vec<bool> = s.anim(&a.anim)?.frames.iter().map(|f| s.is_rig_frame(f)).collect();
                    if rig_frame.contains(&false) && a.slot.is_none() {
                        s.part(&target)?;
                    }
                    let an = s.anim_mut(&a.anim)?;
                    let idx: Vec<usize> = if a.frames.is_empty() { (0..an.frames.len()).collect() } else { a.frames.clone() };
                    let mut changed = 0;
                    for i in idx {
                        let len = an.frames.len();
                        let f = an.frames.get_mut(i).ok_or_else(|| anyhow!("frame {i} out of range ({len} frames)"))?;
                        if rig_frame[i] {
                            // rig frame: edit the slot's key (offsets/rotation relative to the rest pose)
                            let rest = rig.iter().find(|p| p.slot() == target).ok_or_else(|| {
                                anyhow!("no rig slot '{target}' (slots: {})", rig.iter().map(|p| p.slot().to_string()).collect::<Vec<_>>().join(", "))
                            })?;
                            let ki = match f.keys.iter().position(|k| k.slot == target) {
                                Some(ki) => ki,
                                None => {
                                    f.keys.push(Key { slot: target.clone(), ..Default::default() });
                                    f.keys.len() - 1
                                }
                            };
                            let k = &mut f.keys[ki];
                            if a.remove {
                                k.hide = Some(true);
                            }
                            if let Some(v) = a.x {
                                k.dx = v - rest.x;
                            }
                            if let Some(v) = a.y {
                                k.dy = v - rest.y;
                            }
                            k.dx += a.dx;
                            k.dy += a.dy;
                            if let Some(v) = a.rot {
                                k.rot = v - rest.rot;
                            }
                            if a.flip_h.is_some() {
                                k.flip_h = a.flip_h;
                            }
                            if a.flip_v.is_some() {
                                k.flip_v = a.flip_v;
                            }
                            if let Some(r) = &a.replace_with {
                                k.part = (r != &rest.part).then(|| r.clone());
                            }
                            if a.z.is_some() {
                                k.z = a.z;
                            }
                            if k.is_noop() {
                                f.keys.remove(ki);
                            }
                            changed += 1;
                            continue;
                        }
                        let pos = f.poses.iter().position(|p| p.part == target || p.slot.as_deref() == Some(target.as_str()));
                        if a.remove {
                            if let Some(pi) = pos {
                                if f.poses.len() == 1 && !rig.is_empty() {
                                    // an empty explicit frame would silently turn into a rig frame
                                    bail!("removing '{target}' would leave frame {i} without poses, which draws the rig; delete the frame (frame_delete) or hide the part instead");
                                }
                                f.poses.remove(pi);
                                changed += 1;
                            }
                            continue;
                        }
                        let pi = match pos {
                            Some(pi) => pi,
                            None => {
                                f.poses.push(Pose { part: target.clone(), ..Default::default() });
                                f.poses.len() - 1
                            }
                        };
                        let p = &mut f.poses[pi];
                        if let Some(v) = a.x {
                            p.x = v;
                        }
                        if let Some(v) = a.y {
                            p.y = v;
                        }
                        p.x += a.dx;
                        p.y += a.dy;
                        if let Some(v) = a.rot {
                            p.rot = v;
                        }
                        if let Some(v) = a.flip_h {
                            p.flip_h = v;
                        }
                        if let Some(v) = a.flip_v {
                            p.flip_v = v;
                        }
                        if let Some(r) = &a.replace_with {
                            p.part = r.clone();
                        }
                        if let Some(z) = a.z {
                            let pose = f.poses.remove(pi);
                            let z = z.min(f.poses.len());
                            f.poses.insert(z, pose);
                        }
                        changed += 1;
                    }
                    Self::check_anims(s)?;
                    Ok(vec![Out::Text(format!("updated '{target}' in {changed} frame(s) of '{}'", a.anim))])
                })
            }
            "anim_delete" => {
                let a: AnimRef = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    s.anim(&a.anim)?;
                    s.animations.remove(&a.anim);
                    Ok(vec![Out::Text(format!("deleted animation '{}'", a.anim))])
                })
            }
            "anim_copy" => {
                let a: AnimCopy = parse(args)?;
                check_name("animation", &a.to)?;
                self.mutate(&a.sprite, |s| {
                    let mut an = s.anim(&a.from)?.clone();
                    if a.mirror {
                        // mirror the composed frame around the origin, so the feet stay put
                        for f in &mut an.frames {
                            f.flip_h = !f.flip_h;
                        }
                    }
                    if a.reverse {
                        an.frames.reverse();
                    }
                    let n = an.frames.len();
                    s.animations.insert(a.to.clone(), an);
                    Ok(vec![Out::Text(format!("'{}' -> '{}' ({n} frames)", a.from, a.to))])
                })
            }
            "render" => {
                let a: RenderArgs = parse(args)?;
                let d = self.doc(&a.sprite)?;
                let s = &d.sprite;
                let bg = opt_color(&a.background, &s.palette)?;
                let mark = Rgba([255, 0, 200, 255]);
                let remap = s.variant_remap(a.palette.as_deref())?;
                let compose = |f: &Frame| -> Result<RgbaImage> {
                    let mut i = render::compose(s, f)?;
                    apply_remap(&mut i, &remap);
                    Ok(i)
                };

                if let Some(part) = &a.part {
                    let p = s.part(part)?;
                    let k = a.scale.unwrap_or_else(|| render::auto_scale(p.img.width(), p.img.height(), 1, 1));
                    let mut pimg = p.img.clone();
                    apply_remap(&mut pimg, &remap);
                    let mut img = render::present(&pimg, k, bg);
                    render::marker(&mut img, p.pivot[0], p.pivot[1], k, mark);
                    return Ok(vec![Out::Text(format!("part '{part}' (magenta = pivot)")), Out::Image(img)]);
                }

                match (&a.anim, a.frame) {
                    (Some(an_name), Some(fi)) => {
                        let an = s.anim(an_name)?;
                        let f = an.frames.get(fi).ok_or_else(|| anyhow!("frame {fi} out of range ({} frames)", an.frames.len()))?;
                        let mut img = compose(f)?;
                        if let Some(sc) = &a.scene {
                            let mut bgimg = image::open(expand(&sc.image))?.to_rgba8();
                            render::paste(&mut bgimg, &img, sc.x - s.origin[0], sc.y - s.origin[1]);
                            let k = a.scale.unwrap_or_else(|| (1600 / bgimg.width().max(1)).clamp(1, 8));
                            return Ok(vec![
                                Out::Text(format!("'{an_name}' frame {fi} in scene at ({}, {})", sc.x, sc.y)),
                                Out::Image(render::upscale(&bgimg, k)),
                            ]);
                        }
                        if a.onion && an.frames.len() > 1 {
                            let prev_i = if fi == 0 { an.frames.len() - 1 } else { fi - 1 };
                            let mut base = render::fade(&compose(&an.frames[prev_i])?, 0.3);
                            render::paste(&mut base, &img, 0, 0);
                            img = base;
                        }
                        let k = a.scale.unwrap_or_else(|| render::auto_scale(s.width, s.height, 1, 1));
                        let mut out = render::present(&img, k, bg);
                        if a.markers {
                            render::marker(&mut out, s.origin[0] as f32, s.origin[1] as f32, k, mark);
                            for (_, x) in render::frame_xforms(s, f)? {
                                let px = if f.flip_h { 2.0 * s.origin[0] as f32 - x.x } else { x.x };
                                render::marker(&mut out, px, x.y, k, Rgba([0, 200, 255, 255]));
                            }
                        }
                        Ok(vec![
                            Out::Text(format!(
                                "'{an_name}' frame {fi}/{}{}{}",
                                an.frames.len(),
                                if a.onion { " (faded = previous frame)" } else { "" },
                                if a.markers { " (magenta = origin, cyan = pose positions)" } else { "" }
                            )),
                            Out::Image(out),
                        ])
                    }
                    (Some(an_name), None) => {
                        let an = s.anim(an_name)?;
                        let frames = an.frames.iter().map(&compose).collect::<Result<Vec<_>>>()?;
                        let k = a.scale.unwrap_or_else(|| render::auto_scale(s.width, s.height, frames.len() as u32, 1));
                        Ok(vec![
                            Out::Text(format!("'{an_name}': {} frames left to right", frames.len())),
                            Out::Image(render::grid(&[frames], k, bg)),
                        ])
                    }
                    (None, _) => {
                        if s.animations.is_empty() {
                            // overview of parts
                            let parts: Vec<RgbaImage> = s.parts.values().map(|p| p.img.clone()).collect();
                            if parts.is_empty() {
                                bail!("sprite is empty");
                            }
                            let mw = parts.iter().map(|i| i.width()).max().unwrap();
                            let mh = parts.iter().map(|i| i.height()).max().unwrap();
                            let k = a.scale.unwrap_or_else(|| render::auto_scale(mw, mh, parts.len() as u32, 1));
                            let names = s.parts.keys().cloned().collect::<Vec<_>>().join(", ");
                            return Ok(vec![Out::Text(format!("parts: {names}")), Out::Image(render::grid(&[parts], k, bg))]);
                        }
                        let mut rows = vec![];
                        for an in s.animations.values() {
                            rows.push(an.frames.iter().map(&compose).collect::<Result<Vec<_>>>()?);
                        }
                        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(1) as u32;
                        let k = a.scale.unwrap_or_else(|| render::auto_scale(s.width, s.height, cols, rows.len() as u32));
                        let names = s.animations.keys().cloned().collect::<Vec<_>>().join(", ");
                        Ok(vec![Out::Text(format!("rows top to bottom: {names}")), Out::Image(render::grid(&rows, k, bg))])
                    }
                }
            }
            "export" => {
                let a: ExportArgs = parse(args)?;
                let d = self.doc(&a.sprite)?;
                let dir = expand(&a.dir);
                let mut t = export::export(&d.sprite, &dir, a.godot_res_dir.as_deref())?;
                if a.gif {
                    for (n, an) in &d.sprite.animations {
                        if !an.frames.is_empty() {
                            let path = dir.join(format!("{}_{n}.gif", d.sprite.name));
                            export::gif(&d.sprite, n, &path, 4, None, None, None)?;
                            t += &format!("\n  {} (4x preview)", path.file_name().unwrap().to_string_lossy());
                        }
                    }
                }
                Ok(vec![Out::Text(t)])
            }
            "export_gif" => {
                let a: GifArgs = parse(args)?;
                let d = self.doc(&a.sprite)?;
                let bg = opt_color(&a.background, &d.sprite.palette)?;
                let path = expand(&a.path);
                let room = a.scene.as_ref().map(|sc| image::open(expand(&sc.image)).map(|i| i.to_rgba8())).transpose()?;
                let scene = match (&a.scene, &room) {
                    (Some(sc), Some(img)) => Some(export::GifScene { image: img, x: sc.x, y: sc.y, step: sc.step, loops: sc.loops.clamp(1, 50) }),
                    _ => None,
                };
                let scale = a.scale.unwrap_or(if scene.is_some() { 3 } else { 4 }).clamp(1, 16);
                export::gif(&d.sprite, &a.anim, &path, scale, bg, a.palette.as_deref(), scene)?;
                Ok(vec![Out::Text(format!("wrote {}", path.display()))])
            }
            "pixelate" => {
                let a: Pixelate = parse(args)?;
                let src = image::open(expand(&a.image))?.to_rgba8();
                let h = a.height.unwrap_or_else(|| (a.width as u64 * src.height() as u64 / src.width() as u64) as u32);
                if a.width == 0 || h == 0 || a.width > 4096 || h > 4096 {
                    bail!("bad target size {}x{h}", a.width);
                }
                let mut img = resize_with(&src, a.width, h, a.downscale.as_deref(), a.ink, a.ink_level)?;
                let pal: Option<Vec<Px>> = match (&a.sprite_palette, a.colors) {
                    (Some(sp), _) => Some(self.doc(sp)?.sprite.palette.colors()),
                    (None, Some(n)) => {
                        let px = img.pixels().filter(|p| p[3] > 127).map(|p| [p[0], p[1], p[2]]).collect();
                        Some(median_cut(px, n.clamp(2, 256)))
                    }
                    (None, None) => None,
                };
                let dither = a.dither.as_deref().unwrap_or("none");
                if let Some(p) = &pal {
                    quantize_img(&mut img, p, dither, a.dither_strength.unwrap_or(if dither == "floyd" { 0.75 } else { 1.0 }).clamp(0.0, 2.0))?;
                } else if dither != "none" {
                    bail!("dither needs colors or sprite_palette");
                }
                let out = expand(&a.output);
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                img.save(&out)?;
                let k = (1600 / a.width).clamp(1, 8);
                Ok(vec![
                    Out::Text(format!(
                        "wrote {} ({}x{h}{})",
                        out.display(),
                        a.width,
                        pal.map(|p| format!(", {} colors", p.len())).unwrap_or_default()
                    )),
                    Out::Image(render::upscale(&img, k)),
                ])
            }
            "rig_set" => {
                let a: RigSet = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    s.validate_poses(&a.poses, "rig")?;
                    let mut warn = String::new();
                    if s.rig.is_empty() {
                        let blank: Vec<String> = s
                            .animations
                            .iter()
                            .flat_map(|(n, an)| an.frames.iter().enumerate().filter(|(_, f)| f.poses.is_empty()).map(move |(i, _)| format!("{n}[{i}]")))
                            .collect();
                        if !blank.is_empty() {
                            warn = format!("\nnote: frames without poses now draw the rig: {}", blank.join(", "));
                        }
                    }
                    s.rig = a.poses.clone();
                    Self::check_anims(s)?;
                    let f = Frame::rig(100, vec![]);
                    let mut tree = String::new();
                    fn walk(s: &Sprite, parent: Option<&str>, depth: usize, out: &mut String) {
                        for p in s.rig.iter().filter(|p| p.parent.as_deref() == parent) {
                            *out += &format!(
                                "{}{} ({}{}) at {},{}{}\n",
                                "  ".repeat(depth + 1),
                                p.slot(),
                                p.part,
                                if p.hide { ", hidden" } else { "" },
                                p.x,
                                p.y,
                                if p.rot != 0.0 { format!(" rot {}", p.rot) } else { String::new() }
                            );
                            walk(s, Some(p.slot()), depth + 1, out);
                        }
                    }
                    walk(s, None, 0, &mut tree);
                    Ok(vec![
                        Out::Text(format!(
                            "rig set: {} slots (x,y of children are offsets from the parent's pivot)\n{tree}frames without poses are drawn from this rest pose plus their keys{warn}",
                            s.rig.len()
                        )),
                        pose_sheet(s, &f)?,
                    ])
                })
            }
            "palette_ramp" => {
                let a: PaletteRamp = parse(args)?;
                check_name("ramp", &a.name)?;
                let keys: Vec<char> = a.keys.chars().collect();
                if keys.len() < 2 || keys.len() > 12 {
                    bail!("keys: 2 to 12 characters, one per shade, dark to light");
                }
                let bi = a.base_index.unwrap_or(keys.len() / 2);
                if bi >= keys.len() {
                    bail!("base_index {bi} out of range (0..{})", keys.len());
                }
                let base = parse_hex(&a.base)?;
                let mut cols = hue_ramp(base, keys.len(), bi, a.hue_shift.unwrap_or(12.0), a.step.unwrap_or(0.1).clamp(0.02, 0.3));
                let mut keys = keys;
                if let Some(o) = &a.outline {
                    let mut ch = o.chars();
                    let (Some(k), None) = (ch.next(), ch.next()) else { bail!("outline must be one key character") };
                    if keys.contains(&k) {
                        bail!("outline key '{k}' is also a shade key");
                    }
                    let oc = outline_shade(base);
                    if crate::color::lightness(oc) >= crate::color::lightness(cols[0]) {
                        bail!("the ramp's darkest shade is already darker than an outline would be; skip outline and use the darkest key");
                    }
                    keys.insert(0, k);
                    cols.insert(0, oc);
                }
                self.mutate(&a.sprite, |s| {
                    let mut recolored = 0;
                    for (i, (k, c)) in keys.iter().zip(&cols).enumerate() {
                        if let Some(other) = s.palette.0.iter().find(|e| e.color == *c && e.key != *k) {
                            bail!("shade {i} ({}) equals the color of key '{}'; change step or hue_shift", to_hex(*c), other.key);
                        }
                        let label = Some(match (a.outline.is_some(), i) {
                            (true, 0) => format!("{} outline", a.name),
                            (true, i) => format!("{} {}", a.name, i - 1),
                            (false, i) => format!("{} {i}", a.name),
                        });
                        match s.palette.0.iter_mut().find(|e| e.key == *k) {
                            Some(e) => {
                                if e.color != *c {
                                    let old = e.color;
                                    for p in s.parts.values_mut() {
                                        for px in p.img.pixels_mut().filter(|px| **px == old) {
                                            *px = *c;
                                            recolored += 1;
                                        }
                                    }
                                }
                                e.color = *c;
                                e.ramp = Some(a.name.clone());
                                e.name = label;
                            }
                            None => s.palette.0.push(PalEntry { key: *k, color: *c, name: label, ramp: Some(a.name.clone()) }),
                        }
                    }
                    // keys left over from an earlier, longer version of this ramp leave it
                    let mut dropped = vec![];
                    for e in s.palette.0.iter_mut().filter(|e| e.ramp.as_deref() == Some(a.name.as_str()) && !keys.contains(&e.key)) {
                        e.ramp = None;
                        dropped.push(e.key.to_string());
                    }
                    s.palette.validate()?;
                    let swatch = RgbaImage::from_fn(cols.len() as u32, 1, |x, _| cols[x as usize]);
                    Ok(vec![
                        Out::Text(format!(
                            "ramp '{}' (dark -> light): {}{}{}",
                            a.name,
                            keys.iter().zip(&cols).map(|(k, c)| format!("{k}={}", to_hex(*c))).collect::<Vec<_>>().join(" "),
                            if recolored > 0 { format!("\nrecolored {recolored} pixels") } else { String::new() },
                            if dropped.is_empty() { String::new() } else { format!("\nkeys {} are no longer part of ramp '{}'", dropped.join(", "), a.name) }
                        )),
                        Out::Image(render::upscale(&swatch, 32)),
                    ])
                })
            }
            "anim_talk" => {
                let a: AnimTalk = parse(args)?;
                let name = a.anim.clone().unwrap_or_else(|| "talk".into());
                check_name("animation", &name)?;
                let slot = a.slot.clone().unwrap_or_else(|| "mouth".into());
                let ms = a.ms.unwrap_or(75).clamp(30, 1000);
                self.mutate(&a.sprite, |s| {
                    if s.rig.is_empty() {
                        bail!("anim_talk works on a rig: set one with rig_set (with a '{slot}' slot)");
                    }
                    let rest = s.rig.iter().find(|p| p.slot() == slot).cloned().ok_or_else(|| {
                        anyhow!("no rig slot '{slot}' (slots: {}); pass slot", s.rig.iter().map(|p| p.slot().to_string()).collect::<Vec<_>>().join(", "))
                    })?;
                    if let Some(h) = &a.head
                        && !s.rig.iter().any(|p| p.slot() == h) {
                            bail!("no rig slot '{h}' for head");
                        }
                    for p in a.visemes.values() {
                        s.part(p)?;
                    }
                    let prefixes: Vec<String> = {
                        let mut v = vec![slot.clone()];
                        if let Some((pre, _)) = rest.part.rsplit_once('_') {
                            v.push(pre.to_string());
                        }
                        v
                    };
                    let find = |class: &str| -> Option<String> {
                        if let Some((_, p)) = a.visemes.iter().find(|(k, _)| k.eq_ignore_ascii_case(class)) {
                            return Some(p.clone());
                        }
                        prefixes.iter().map(|pre| format!("{pre}_{}", class.to_lowercase())).find(|n| s.parts.contains_key(n))
                    };
                    let rest_part = find("rest").unwrap_or_else(|| rest.part.clone());
                    let chains: [(&str, &[&str]); 7] = [
                        ("A", &["A", "O", "E", "C"]),
                        ("E", &["E", "C", "A"]),
                        ("O", &["O", "A", "E"]),
                        ("M", &["M"]),
                        ("F", &["F", "E", "M"]),
                        ("L", &["L", "C", "E"]),
                        ("C", &["C", "E", "A"]),
                    ];
                    let mut map: HashMap<&str, String> = HashMap::new();
                    for (class, chain) in chains {
                        if let Some(p) = chain.iter().find_map(|c| find(c)) {
                            map.insert(class, p);
                        }
                    }
                    map.entry("M").or_insert_with(|| rest_part.clone());
                    if !["A", "E", "O"].iter().any(|c| map.contains_key(c)) {
                        bail!(
                            "no open mouth parts found: name them {0}_a, {0}_e, {0}_o (and optionally {0}_m, {0}_f, {0}_l, {0}_c, {0}_rest) or pass visemes {{\"A\": part, ...}}",
                            prefixes[prefixes.len() - 1]
                        );
                    }
                    let mut seq: Vec<(&str, u32)> = vec![];
                    for ch in a.text.chars() {
                        let (class, d) = match ch.to_lowercase().next().unwrap_or(ch) {
                            'a' => ("A", ms),
                            'e' | 'i' | 'y' | 'ä' => ("E", ms),
                            'o' | 'u' | 'w' | 'q' | 'ö' | 'ü' => ("O", ms),
                            'm' | 'b' | 'p' => ("M", ms),
                            'f' | 'v' => ("F", ms),
                            'l' | 't' | 'd' | 'n' | 'r' => ("L", ms),
                            ',' | ';' | ':' | '-' | '–' | '—' => ("rest", ms * 3),
                            '.' | '!' | '?' | '…' => ("rest", ms * 5),
                            c if c.is_whitespace() => ("rest", ms),
                            c if c.is_alphanumeric() => ("C", ms),
                            _ => continue,
                        };
                        match seq.last_mut() {
                            Some((c, dur)) if *c == class => *dur += d,
                            _ => seq.push((class, d)),
                        }
                    }
                    while seq.first().is_some_and(|(c, _)| *c == "rest") {
                        seq.remove(0);
                    }
                    match seq.last_mut() {
                        Some((c, d)) if *c == "rest" => *d = (*d).max(250),
                        _ => seq.push(("rest", 250)),
                    }
                    if seq.len() > 2000 {
                        bail!("text too long ({} mouth changes); split it into several lines", seq.len());
                    }
                    let frames: Vec<Frame> = seq
                        .iter()
                        .map(|(class, d)| {
                            let part = if *class == "rest" { rest_part.clone() } else { map.get(class).cloned().unwrap_or_else(|| rest_part.clone()) };
                            let mut keys = vec![];
                            if part != rest.part {
                                keys.push(Key { slot: slot.clone(), part: Some(part), ..Default::default() });
                            }
                            if let Some(h) = &a.head
                                && matches!(*class, "A" | "O") {
                                    keys.push(Key { slot: h.clone(), dy: -1, ..Default::default() });
                                }
                            Frame::rig(*d, keys)
                        })
                        .collect();
                    let total: u32 = frames.iter().map(|f| f.duration_ms).sum();
                    let visemes = seq.iter().map(|(c, _)| if *c == "rest" { "_" } else { c }).collect::<Vec<_>>().join("");
                    let used: Vec<String> = {
                        let mut v: Vec<String> = map.iter().map(|(k, p)| format!("{k}={p}")).collect();
                        v.sort();
                        v
                    };
                    let n = frames.len();
                    s.animations.insert(name.clone(), Animation { looped: a.looped, frames });
                    Self::check_anims(s)?;
                    Ok(vec![
                        Out::Text(format!(
                            "'{name}': {n} frames, {total} ms, visemes {visemes} ('_' = rest {rest_part})\nmouths: {}\nimage: close-up of the first frames, 12 per row\ntweak with pose_set (slot '{slot}'), or retime with frame durations",
                            used.join(", ")
                        )),
                        slot_strip(s, &name, &slot, 36)?,
                    ])
                })
            }
            "anim_generate" => {
                let a: AnimGenerate = parse(args)?;
                self.mutate(&a.sprite, |s| {
                    if s.rig.is_empty() {
                        bail!("anim_generate works on a rig: set one with rig_set first");
                    }
                    let slots: Vec<String> = s.rig.iter().map(|p| p.slot().to_string()).collect();
                    let need = |sl: &str| -> Result<()> {
                        if slots.iter().any(|x| x == sl) { Ok(()) } else { bail!("no rig slot '{sl}' (slots: {})", slots.join(", ")) }
                    };
                    let descends = |child: &str, anc: &str| -> bool {
                        let mut cur = s.rig.iter().find(|p| p.slot() == child);
                        let mut guard = 0;
                        while let Some(p) = cur {
                            match &p.parent {
                                Some(par) if par == anc => return true,
                                Some(par) => cur = s.rig.iter().find(|q| q.slot() == par),
                                None => return false,
                            }
                            guard += 1;
                            if guard > slots.len() {
                                return false;
                            }
                        }
                        false
                    };
                    let (name, frames, note) = match a.preset.as_str() {
                        "idle" => {
                            let name = a.anim.clone().unwrap_or_else(|| "idle".into());
                            let ms = a.ms.unwrap_or(180).clamp(40, 2000);
                            let body = match &a.body {
                                Some(b) => b.clone(),
                                None => match ["torso", "body", "chest", "upper_body"].iter().find(|n| slots.iter().any(|x| x == *n)) {
                                    Some(n) => vec![n.to_string()],
                                    None => bail!("pass body: the slots that rise when breathing (usually the torso; its children follow)"),
                                },
                            };
                            for b in body.iter().chain(&a.lag) {
                                need(b)?;
                            }
                            // 1px breath: rest, rest, up x4, rest, rest
                            let lift = [0, 0, -1, -1, -1, -1, 0, 0];
                            let mut frames = vec![];
                            for i in 0..lift.len() {
                                let prev = lift[(i + lift.len() - 1) % lift.len()];
                                let mut keys: Vec<Key> = body.iter().filter(|_| lift[i] != 0).map(|b| Key { slot: b.clone(), dy: lift[i], ..Default::default() }).collect();
                                for l in &a.lag {
                                    // loose bits (hat, hair, ears) arrive one frame late
                                    let dy = if body.iter().any(|b| descends(l, b)) { prev - lift[i] } else { prev };
                                    if dy != 0 {
                                        keys.push(Key { slot: l.clone(), dy, ..Default::default() });
                                    }
                                }
                                frames.push(Frame::rig(ms, keys));
                            }
                            if let Some(bl) = &a.blink {
                                need(&bl.slot)?;
                                if let Some(p) = &bl.part {
                                    s.part(p)?;
                                }
                                let mut f = frames[5].clone();
                                f.duration_ms = 110;
                                f.keys.push(Key {
                                    slot: bl.slot.clone(),
                                    part: bl.part.clone(),
                                    hide: if bl.part.is_none() { Some(false) } else { None },
                                    ..Default::default()
                                });
                                frames.insert(6, f);
                            }
                            (name, frames, String::new())
                        }
                        "walk" => {
                            let name = a.anim.clone().unwrap_or_else(|| "walk".into());
                            let legs = a.legs.clone().ok_or_else(|| anyhow!("walk needs legs: [near_leg_slot, far_leg_slot]"))?;
                            for l in &legs {
                                need(l)?;
                            }
                            if let Some(ar) = &a.arms {
                                for x in ar {
                                    need(x)?;
                                }
                            }
                            let n = a.frames.unwrap_or(8).clamp(4, 32);
                            let ms = a.ms.unwrap_or(100).clamp(30, 2000);
                            let swing = a.swing.unwrap_or(22.0).clamp(0.0, 60.0);
                            let arm_swing = a.arm_swing.unwrap_or(18.0).clamp(0.0, 90.0);
                            let leg_rest = s.rig.iter().find(|p| p.slot() == legs[0]).unwrap();
                            let lp = s.part(&leg_rest.part)?;
                            let leg_len = (lp.img.height() as f32 - lp.pivot[1]).max(1.0);
                            let roots: Vec<String> = s.rig.iter().filter(|p| p.parent.is_none()).map(|p| p.slot().to_string()).collect();
                            let mut frames = vec![];
                            // lowest opaque row of a frame (measured, so rotated toes and heels count)
                            let lowest = |f: &Frame| -> Result<Option<i32>> {
                                let img = render::compose_margin(s, f, 64)?;
                                Ok((0..img.height()).rev().find(|y| (0..img.width()).any(|x| img.get_pixel(x, *y)[3] > 0)).map(|y| y as i32 - 64))
                            };
                            for i in 0..n {
                                let sa = (i as f32 / n as f32 * std::f32::consts::TAU).sin();
                                let ang = swing * sa;
                                let mut keys = vec![
                                    Key { slot: legs[0].clone(), rot: ang, ..Default::default() },
                                    Key { slot: legs[1].clone(), rot: -ang, ..Default::default() },
                                ];
                                if let Some(ar) = &a.arms {
                                    keys.push(Key { slot: ar[0].clone(), rot: -arm_swing * sa, ..Default::default() });
                                    keys.push(Key { slot: ar[1].clone(), rot: arm_swing * sa, ..Default::default() });
                                }
                                // a swung leg changes how low the feet reach: move everything so the lowest
                                // pixel sits on the floor row (origin y - 1) in every frame
                                let drop = match lowest(&Frame::rig(ms, keys.clone()))? {
                                    Some(low) => s.origin[1] - 1 - low,
                                    None => 0,
                                };
                                if drop != 0 {
                                    for r in &roots {
                                        match keys.iter_mut().find(|k| &k.slot == r) {
                                            Some(k) => k.dy += drop,
                                            None => keys.push(Key { slot: r.clone(), dy: drop, ..Default::default() }),
                                        }
                                    }
                                }
                                keys.retain(|k| !k.is_noop());
                                frames.push(Frame::rig(ms, keys));
                            }
                            let stride = 2.0 * leg_len * swing.to_radians().sin();
                            let speed = 2.0 * stride / (n as f32 * ms as f32 / 1000.0);
                            (
                                name,
                                frames,
                                format!(
                                    "\nmove the character about {speed:.0} px/s ({:.1} px per frame) in the game so the feet don't slide; anim_copy mirror=true makes the other direction",
                                    2.0 * stride / n as f32
                                ),
                            )
                        }
                        p => bail!("unknown preset '{p}' (idle, walk)"),
                    };
                    check_name("animation", &name)?;
                    let count = frames.len();
                    s.animations.insert(name.clone(), Animation { looped: true, frames });
                    Self::check_anims(s)?;
                    Ok(vec![
                        Out::Text(format!(
                            "'{name}': {count} rig frames generated{note}\nrefine with pose_set (slot=..., frames=[...]) or swap parts for drawn poses"
                        )),
                        anim_strip(s, &name, 32)?,
                    ])
                })
            }
            "undo" | "redo" => {
                let a: SpriteRef = parse(args)?;
                let d = self.doc(&a.sprite)?;
                let (from, to) = if name == "undo" { (&mut d.undo, &mut d.redo) } else { (&mut d.redo, &mut d.undo) };
                let prev = from.pop().ok_or_else(|| anyhow!("nothing to {name}"))?;
                to.push(std::mem::replace(&mut d.sprite, prev));
                d.sprite.save(&d.dir)?;
                Ok(vec![Out::Text(format!("{name} done ({} more undo, {} redo)", d.undo.len(), d.redo.len()))])
            }
            _ => bail!("unknown tool '{name}'"),
        }
    }
}

// ---------- tool schemas ----------

fn tool(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": desc,
        "inputSchema": {"type": "object", "properties": props, "required": required}
    })
}

pub fn definitions() -> Vec<Value> {
    let sprite = json!({"type": "string", "description": "sprite name (or its directory path)"});
    let pt2i = json!({"type": "array", "items": {"type": "integer"}, "minItems": 2, "maxItems": 2});
    let pt2f = json!({"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2});
    let rect = json!({"type": "array", "items": {"type": "integer"}, "minItems": 4, "maxItems": 4, "description": "[x, y, w, h]"});
    let color = json!({"type": "string", "description": "palette key (one char), #rrggbb, #rrggbbaa, @N (palette index) or 'transparent'"});
    let pal_items = json!({"type": "array", "description": "entries: {key, color, name?, ramp?} or 'k=#hex' or '#hex' (auto key). Keys '.', '_' and space are reserved.", "items": {"anyOf": [
        {"type": "string"},
        {"type": "object", "properties": {"key": {"type": "string"}, "color": {"type": "string"}, "name": {"type": "string"}, "ramp": {"type": "string"}}, "required": ["color"]}
    ]}});
    let pose = json!({"type": "object", "properties": {
        "part": {"type": "string"},
        "slot": {"type": "string", "description": "stable name of this placement (default: the part name); parents and keys refer to it"},
        "parent": {"type": "string", "description": "slot this part hangs from: x/y become the offset from the parent's pivot, rotation and flips are inherited"},
        "x": {"type": "integer", "description": "canvas x where the part's pivot goes (or offset from the parent's pivot)"},
        "y": {"type": "integer"},
        "rot": {"type": "number", "description": "degrees clockwise around the pivot"},
        "flip_h": {"type": "boolean"}, "flip_v": {"type": "boolean"},
        "hide": {"type": "boolean", "description": "don't draw (children still follow), e.g. blink lids in the rest pose"}
    }, "required": ["part"]});
    let key = json!({"type": "object", "properties": {
        "slot": {"type": "string"},
        "dx": {"type": "integer"}, "dy": {"type": "integer"},
        "rot": {"type": "number", "description": "degrees added to the rest rotation"},
        "part": {"type": "string", "description": "draw this part instead (mouth_a, eyes_closed, arm_bent)"},
        "hide": {"type": "boolean", "description": "true hides, false shows a slot hidden in the rest pose"},
        "flip_h": {"type": "boolean"}, "flip_v": {"type": "boolean"},
        "z": {"type": "integer", "description": "new draw index (0 = back)"}
    }, "required": ["slot"]});
    let frame = json!({"type": "object", "description": "Explicit frame: poses (back to front). Rig frame: no poses, only keys = changes to the rig's rest pose.", "properties": {
        "duration_ms": {"type": "integer", "default": 100},
        "poses": {"type": "array", "items": pose, "description": "back to front"},
        "keys": {"type": "array", "items": key, "description": "rig frames: changes to the rest pose; slots without a key stay at rest"},
        "flip_h": {"type": "boolean", "description": "mirror the whole frame around the origin x"}
    }});
    let op = json!({"type": "object", "description":
        "Drawing op, discriminated by 'op':\n\
         {op:'rows', x?, y?, rows:[str], legend?:{char:color}, erase_dots?} — paint/patch a pixel block; chars = palette keys, legend chars override, '.'/' ' = unchanged (erase if erase_dots), '_' = erase\n\
         {op:'pixels', points:[[x,y]...], color}\n\
         {op:'line', points:[[x,y],[x,y],...], color, clean?} — polyline; clean = pixel-perfect (no doubled corners)\n\
         {op:'curve', points:[[x,y]...], color, kind?:'spline'|'bezier', closed?, fill?, stroke?, clean? (default true), pattern?, color2?} — smooth organic line \
         through the points (spline) or with control points (bezier: 3, 4, 7, 10.. points); closed+fill paints the inside with color and the edge with stroke ('none' = no edge)\n\
         {op:'mirror', axis?:'x'|'y', at?, from?:'left'|'right'|'top'|'bottom'} — copy one half onto the other (symmetric faces/bodies), at = mirror line (default middle)\n\
         {op:'shade', light?:[dx,dy] (default [-1,-1] = top-left), depth?:1, highlight?:0, steps?:1, colors?, rect?, edge?:'ramp'|'color'|'alpha'} — \
         cel shading along palette ramps: pixels within depth of their region's far side get darker, within highlight of the lit side lighter\n\
         {op:'ramp', steps, rect?, points?, colors?} — step pixels along their ramp (negative = darker)\n\
         {op:'unjag', colors?, level?, passes?} — remove doubled corners from line work (after a lineart import): the elbow of every L of line pixels takes the color of the cell it cuts off\n\
         {op:'contour', of, color?|selout?, against?, diagonal?} — inner lines: pixels of color 'of' touching another color become color (selout: darkest of of's ramp)\n\
         {op:'rect', x,y,w,h, color, fill?, pattern?, color2?}\n\
         {op:'ellipse', x,y,w,h, color, fill?, pattern?, color2?} — bounding box\n\
         {op:'polygon', points:[[x,y]...], color, fill? (default true), pattern?, color2?}\n\
         {op:'fill', x,y, color, pattern?, color2?} — flood fill\n\
         pattern = 'checker' | 'bayer:N' (N 0-16 density) | 'hlines' | 'vlines': dither; 'on' pixels get color, others color2 (or untouched)\n\
         {op:'replace', from, to} — recolor\n\
         {op:'outline', color?, diagonal?, selout?} — outline around opaque pixels; selout=true uses the darkest shade of each neighbor's ramp (color = fallback)\n\
         {op:'shift', dx, dy, wrap?}\n\
         {op:'flip', h?, v?}\n\
         {op:'clear', rect?:[x,y,w,h]}\n\
         {op:'copy', from:[x,y,w,h], to:[x,y]}",
        "properties": {"op": {"type": "string", "enum": ["rows","pixels","line","curve","rect","ellipse","polygon","fill","replace","outline","unjag","contour","shade","ramp","mirror","shift","flip","clear","copy"]}},
        "required": ["op"]});

    vec![
        tool("sprite_new", "Create a sprite project (a directory with sprite.json + parts/*.png; autosaved on every change).", json!({
            "name": {"type": "string"},
            "dir": {"type": "string", "description": "directory for the project (absolute path recommended)"},
            "width": {"type": "integer"}, "height": {"type": "integer"},
            "origin": {"description": "anchor point, default [width/2, height] (bottom center)", "type": "array", "items": {"type": "integer"}},
            "palette": pal_items
        }), &["name", "dir", "width", "height"]),
        tool("sprite_open", "Open an existing sprite project directory.", json!({"dir": {"type": "string"}}), &["dir"]),
        tool("sprite_info", "Describe a sprite: canvas, origin, palette, parts, animations.", json!({"sprite": sprite}), &["sprite"]),
        tool("sprite_edit", "Change canvas size / origin / portrait crop. offset shifts all poses and the origin.", json!({
            "sprite": sprite, "width": {"type": "integer"}, "height": {"type": "integer"},
            "origin": pt2i, "offset": pt2i, "portrait": rect
        }), &["sprite"]),
        tool("palette_set", "Replace the palette. Give every color a semantic one-char key (e.g. o=outline, a/A/1 = skin shadow/base/highlight) and optionally a name and a ramp group (used by selout outlines). Keys whose color changes recolor existing pixels (palette swap, e.g. night lighting).", json!({"sprite": sprite, "colors": pal_items}), &["sprite", "colors"]),
        tool("palette_variant", "Create/update a named palette variant (lighting: day, dusk, tavern, moonlit...). Starts from the base palette (or the existing variant) and overrides the given keys. Use palette='<name>' in render/review/export_gif; export writes one sheet per variant.", json!({
            "sprite": sprite, "name": {"type": "string"}, "colors": pal_items, "remove": {"type": "boolean"}
        }), &["sprite", "name"]),
        tool("palette_from_image", "Extract a palette from an image (e.g. a game background) with median cut, so the character matches the scene.", json!({
            "sprite": sprite, "image": {"type": "string"}, "colors": {"type": "integer", "description": "max 62"},
            "region": rect, "append": {"type": "boolean", "description": "add to existing palette instead of replacing"},
            "key_color": {"type": "string", "description": "#hex background to ignore (with its fringes), e.g. the magenta behind a model sheet"},
            "key_tolerance": {"type": "integer", "description": "summed RGB distance still counted as the key (default 40)"}
        }), &["sprite", "image", "colors"]),
        tool("part_new", "Create an empty part.", json!({
            "sprite": sprite, "part": {"type": "string"}, "width": {"type": "integer"}, "height": {"type": "integer"},
            "pivot": {"description": "rotation/placement point in part pixels, default center; e.g. shoulder joint for an arm", "type": "array", "items": {"type": "number"}}
        }), &["sprite", "part", "width", "height"]),
        tool("part_edit", "Rename a part, set its pivot, or resize its canvas (offset moves existing pixels and pivot).", json!({
            "sprite": sprite, "part": {"type": "string"}, "pivot": pt2f, "rename": {"type": "string"},
            "resize": {"type": "object", "properties": {"width": {"type": "integer"}, "height": {"type": "integer"}, "offset_x": {"type": "integer"}, "offset_y": {"type": "integer"}}, "required": ["width", "height"]}
        }), &["sprite", "part"]),
        tool("part_delete", "Delete an unused part.", json!({"sprite": sprite, "part": {"type": "string"}}), &["sprite", "part"]),
        tool("part_duplicate", "Copy a part (optionally mirrored) under a new name — e.g. to make a variant like arm_bent.", json!({
            "sprite": sprite, "part": {"type": "string"}, "new_name": {"type": "string"}, "flip_h": {"type": "boolean"}, "flip_v": {"type": "boolean"}
        }), &["sprite", "part", "new_name"]),
        tool("part_import", "Create/replace a part from an image file (e.g. one figure of a generated model sheet): crop to region, key out the background, trim to the drawing, scale down and snap to the palette. For cartoon art with dark outlines use downscale='lineart': a box filter turns outlines into mush, lineart keeps them crisp and takes each pixel's dominant color.", json!({
            "sprite": sprite, "part": {"type": "string"}, "image": {"type": "string"}, "region": rect,
            "size": {"type": "array", "items": {"type": "integer"}, "description": "[w, h] target size; 0 for one side keeps the aspect ratio (e.g. [0, 100])"},
            "downscale": {"type": "string", "enum": ["box", "lineart"], "description": "box (default, for painted or soft art) or lineart (outlined cartoon art)"},
            "ink": {"type": "number", "description": "lineart: share of a block that must be ink to become ink (default 0.22; lower = bolder lines)"},
            "ink_level": {"type": "number", "description": "lineart: perceived lightness 0-1 below which a pixel counts as ink (default 0.3)"},
            "quantize": {"type": "boolean", "description": "snap colors to the sprite palette"},
            "alpha_threshold": {"type": "integer", "description": "alpha below this becomes transparent (default 128)"},
            "key_color": {"type": "string", "description": "make this background color transparent (e.g. #ff00ff)"},
            "key_tolerance": {"type": "integer", "description": "summed RGB distance still counted as the key (default 40)"},
            "key_spill": {"type": "boolean", "description": "also remove pixels of the key's hue: anti-aliased fringes and key/outline mixes (default true for saturated keys)"},
            "trim": {"type": "boolean", "description": "crop to the drawing after keying, so region can be rough"},
            "drop_islands": {"type": "boolean", "description": "remove detached pieces under 2% of the main figure (bits of neighbouring figures on a model sheet) before trimming"},
            "pivot": pt2f
        }), &["sprite", "part", "image"]),
        tool("part_extract", "Copy a rectangle of one part into a new part (cut=true erases it from the source). Use to split an imported full character into limbs.", json!({
            "sprite": sprite, "from": {"type": "string"}, "part": {"type": "string"}, "region": rect, "cut": {"type": "boolean"}, "pivot": pt2f
        }), &["sprite", "from", "part", "region"]),
        tool("part_get", "Read a part's exact pixels as text rows (palette keys, '.' = transparent) plus a preview. Use region to read just e.g. the face, then patch it back with a rows op at the same x,y.", json!({"sprite": sprite, "part": {"type": "string"}, "region": rect}), &["sprite", "part"]),
        tool("draw", "Apply a batch of drawing ops to a part (coordinates are part-local). Returns a preview (magenta = pivot).", json!({
            "sprite": sprite, "part": {"type": "string"},
            "ops": {"type": "array", "items": op},
            "preview": {"type": "boolean", "default": true}
        }), &["sprite", "part", "ops"]),
        tool("anim_set", "Create or replace an animation with all its frames.", json!({
            "sprite": sprite, "anim": {"type": "string"}, "loop": {"type": "boolean", "default": true},
            "frames": {"type": "array", "items": frame}
        }), &["sprite", "anim", "frames"]),
        tool("frame_set", "Replace frame at index, or insert (insert=true or index == frame count).", json!({
            "sprite": sprite, "anim": {"type": "string"}, "index": {"type": "integer"}, "frame": frame, "insert": {"type": "boolean"}
        }), &["sprite", "anim", "index", "frame"]),
        tool("frame_clone", "Duplicate a frame (count times), inserted at 'to' (default right after it). Then tweak with pose_set.", json!({
            "sprite": sprite, "anim": {"type": "string"}, "index": {"type": "integer"}, "to": {"type": "integer"}, "count": {"type": "integer"}
        }), &["sprite", "anim", "index"]),
        tool("review", "One control sheet for self-review: 1:1 size, black silhouette (does the pose read?), value study (grayscale by perceived lightness: do face, hands and clothes separate?), zoom with pixel grid + coordinate labels, and (for animations) all frames in a strip. grid_text=true also returns the composed frame as text rows.", json!({
            "sprite": sprite, "part": {"type": "string"}, "anim": {"type": "string"}, "frame": {"type": "integer"},
            "zoom": {"type": "integer"}, "grid_text": {"type": "boolean"}, "palette": {"type": "string", "description": "palette variant name"}
        }), &["sprite"]),
        tool("lint", "Check for common pixel-art problems: orphan pixels, doubled outline corners (jaggies), off-palette colors, parts clipped by the canvas, feet not touching the origin line / drifting between frames.", json!({
            "sprite": sprite, "outline": {"type": "string", "description": "outline color/key (default: darkest palette color)"}
        }), &["sprite"]),
        tool("frame_delete", "Delete a frame.", json!({"sprite": sprite, "anim": {"type": "string"}, "index": {"type": "integer"}}), &["sprite", "anim", "index"]),
        tool("pose_set", "Tweak one part's pose across frames (all frames if 'frames' omitted): absolute x/y/rot/flips, relative dx/dy, swap the part (replace_with, e.g. mouth_closed -> mouth_open), reorder (z = draw index), or remove. Explicit frames: pass part (adds the pose if missing). Rig frames: pass slot; this edits the frame's key, x/y/rot are the slot's local values (offset from rest is stored), remove hides it.", json!({
            "sprite": sprite, "anim": {"type": "string"}, "frames": {"type": "array", "items": {"type": "integer"}},
            "part": {"type": "string"}, "slot": {"type": "string"}, "x": {"type": "integer"}, "y": {"type": "integer"}, "dx": {"type": "integer"}, "dy": {"type": "integer"},
            "rot": {"type": "number"}, "flip_h": {"type": "boolean"}, "flip_v": {"type": "boolean"},
            "replace_with": {"type": "string"}, "z": {"type": "integer"}, "remove": {"type": "boolean"}
        }), &["sprite", "anim"]),
        tool("rig_set", "Set the character's rest pose: every slot (back to front) with its part, position and parent. Children hang from their parent's pivot, so moving or turning the torso carries head, arms, mouth and hat along. Frames without poses are then drawn from the rig plus their keys, so animations only list what changes. Returns the rest pose with pivots marked.", json!({
            "sprite": sprite, "poses": {"type": "array", "items": pose, "description": "back to front; roots use canvas coordinates, children offsets from their parent's pivot"}
        }), &["sprite", "poses"]),
        tool("palette_ramp", "Add or replace a hue-shifted shading ramp built from one base color: shadows drift cool (violet), highlights warm (yellow) with less chroma. This is the standard pixel-art ramp instead of flat darker/lighter copies. keys = one char per shade, dark to light. Existing keys are recolored in place.", json!({
            "sprite": sprite, "name": {"type": "string", "description": "ramp group, e.g. skin, jacket"},
            "base": {"type": "string", "description": "#rrggbb, the mid tone"},
            "keys": {"type": "string", "description": "e.g. 'abcd' (dark -> light)"},
            "base_index": {"type": "integer", "description": "which shade is the base (default: middle)"},
            "hue_shift": {"type": "number", "description": "degrees of hue drift per step (default 12)"},
            "step": {"type": "number", "description": "lightness between shades, OKLab L (default 0.1)"},
            "outline": {"type": "string", "description": "extra key for a deep tinted outline shade in this ramp; outline selout then draws colored DOTT-style lines that hold against any background"}
        }), &["sprite", "name", "base", "keys"]),
        tool("anim_talk", "Lip-sync: build a talk animation from a line of dialogue on a rig. Letters map to mouth shapes (A, E, O, M, F, L, C, rest) found as parts named <slot>_a, <slot>_e, <slot>_o, <slot>_m, <slot>_f, <slot>_l, <slot>_c, <slot>_rest (missing ones fall back to similar shapes), or given via visemes. Punctuation adds pauses; head bobs on open vowels.", json!({
            "sprite": sprite, "text": {"type": "string"}, "anim": {"type": "string", "description": "default 'talk'"},
            "slot": {"type": "string", "description": "mouth slot in the rig (default 'mouth')"},
            "visemes": {"type": "object", "description": "{\"A\": \"mouth_open\", \"M\": \"mouth_closed\", ...}"},
            "ms": {"type": "integer", "description": "time per letter (default 75)"},
            "head": {"type": "string", "description": "slot to bob 1px on A/O"},
            "loop": {"type": "boolean", "default": false}
        }), &["sprite", "text"]),
        tool("anim_generate", "Generate a starting animation on a rig, then refine it. preset 'idle': 1px breathing of the body slots, with lag slots (hat, hair) arriving a frame late and an optional blink (swap to a closed-eyes part, or show a hidden lids slot). preset 'walk' (side view): legs swing in opposite directions, arms counter-swing, and the body drops so the feet stay on the floor; it reports the walk speed that avoids sliding.", json!({
            "sprite": sprite, "preset": {"type": "string", "enum": ["idle", "walk"]}, "anim": {"type": "string"},
            "ms": {"type": "integer", "description": "frame duration (idle 180, walk 100)"},
            "body": {"type": "array", "items": {"type": "string"}, "description": "idle: slots that rise (default torso/body)"},
            "lag": {"type": "array", "items": {"type": "string"}, "description": "idle: slots that follow one frame late"},
            "blink": {"type": "object", "properties": {"slot": {"type": "string"}, "part": {"type": "string"}}, "required": ["slot"]},
            "legs": {"type": "array", "items": {"type": "string"}, "description": "walk: [near_leg, far_leg] slots, pivots at the hips"},
            "arms": {"type": "array", "items": {"type": "string"}, "description": "walk: [near_arm, far_arm] slots, pivots at the shoulders"},
            "swing": {"type": "number", "description": "walk: leg swing degrees (default 22)"},
            "arm_swing": {"type": "number", "description": "walk: arm swing degrees (default 18)"},
            "frames": {"type": "integer", "description": "walk: frames per cycle (default 8)"}
        }), &["sprite", "preset"]),
        tool("anim_delete", "Delete an animation.", json!({"sprite": sprite, "anim": {"type": "string"}}), &["sprite", "anim"]),
        tool("anim_copy", "Copy an animation; mirror=true flips it horizontally (walk_right -> walk_left), reverse=true reverses frame order.", json!({
            "sprite": sprite, "from": {"type": "string"}, "to": {"type": "string"}, "mirror": {"type": "boolean"}, "reverse": {"type": "boolean"}
        }), &["sprite", "from", "to"]),
        tool("render", "Render for visual inspection. part -> that part; anim+frame -> one frame (onion = show previous frame faded; markers = origin & pose points; scene = composite onto a background image with the sprite origin at x,y); anim only -> all frames in a strip; nothing -> every animation as rows (or all parts if no animations).", json!({
            "sprite": sprite, "part": {"type": "string"}, "anim": {"type": "string"}, "frame": {"type": "integer"},
            "scale": {"type": "integer"}, "onion": {"type": "boolean"}, "markers": {"type": "boolean"},
            "palette": {"type": "string", "description": "palette variant name"},
            "background": {"type": "string", "description": "color, or 'checker' (default)"},
            "scene": {"type": "object", "properties": {"image": {"type": "string"}, "x": {"type": "integer"}, "y": {"type": "integer"}}, "required": ["image", "x", "y"]}
        }), &["sprite"]),
        tool("export", "Export spritesheet PNG (one row per animation) + Aseprite-style JSON (frame rects, tags, durations, origin/foot pivot, portrait crop) + Godot 4 SpriteFrames .tres + a 4x GIF per animation. Texture res:// path is derived from the nearest project.godot unless godot_res_dir is given.", json!({
            "sprite": sprite, "dir": {"type": "string"}, "godot_res_dir": {"type": "string", "description": "e.g. res://characters/hero"},
            "gif": {"type": "boolean", "default": true}
        }), &["sprite", "dir"]),
        tool("export_gif", "Write an animated GIF of one animation — standalone, or inside a room background (scene: sprite origin at x,y; step moves it per frame, e.g. [2,0] to walk across; loops repeats the cycle).", json!({
            "sprite": sprite, "anim": {"type": "string"}, "path": {"type": "string"},
            "scale": {"type": "integer", "description": "default 4, or 3 with scene"},
            "background": color, "palette": {"type": "string", "description": "palette variant name"},
            "scene": {"type": "object", "properties": {
                "image": {"type": "string"}, "x": {"type": "integer"}, "y": {"type": "integer"},
                "step": {"type": "array", "items": {"type": "integer"}}, "loops": {"type": "integer"}
            }, "required": ["image", "x", "y"]}
        }), &["sprite", "anim", "path"]),
        tool("pixelate", "Turn any image (PNG/JPEG/WebP, e.g. a generated background) into pixel art: box-downscale to width (x height, default keeps aspect), then reduce to N colors or to a sprite's palette.", json!({
            "image": {"type": "string"}, "output": {"type": "string"}, "width": {"type": "integer"}, "height": {"type": "integer"},
            "colors": {"type": "integer"}, "sprite_palette": {"type": "string", "description": "sprite whose palette to use"},
            "dither": {"type": "string", "enum": ["none", "bayer", "floyd"], "description": "bayer = ordered retro VGA look, floyd = error diffusion; fights banding in skies/water"},
            "dither_strength": {"type": "number", "description": "0-2 (default 1, floyd 0.75)"},
            "downscale": {"type": "string", "enum": ["box", "lineart"], "description": "box (default, painted art) or lineart (outlined cartoon art)"},
            "ink": {"type": "number", "description": "lineart: share of a block that must be ink (default 0.22)"},
            "ink_level": {"type": "number", "description": "lineart: lightness 0-1 below which a pixel is ink (default 0.3)"}
        }), &["image", "output", "width"]),
        tool("undo", "Undo the last change to a sprite.", json!({"sprite": sprite}), &["sprite"]),
        tool("redo", "Redo.", json!({"sprite": sprite}), &["sprite"]),
    ]
}
