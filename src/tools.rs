use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use image::{Rgba, RgbaImage};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::color::{CLEAR, PalSpec, Palette, Px, apply_remap, median_cut, nearest, palette_from_specs, parse_color, quantize_img};
use crate::draw::{self, Op};
use crate::export;
use crate::model::{Animation, Frame, Part, Pose, Sprite, check_name};
use crate::render;

pub const INSTRUCTIONS: &str = "freesprite is a parts-based pixel-art sprite editor for adventure-game characters.\n\
Concepts: a sprite has a fixed canvas (width x height), an origin (the feet point used for placement in the game), a palette, \
parts (small reusable pixel images such as head/torso/arm_l, each with a pivot point), and animations. \
An animation is a list of frames; a frame is a list of poses, each placing a part's pivot at x,y on the canvas with optional rotation (degrees clockwise) and flips. \
Pose order = draw order (first is furthest back).\n\
Workflow: sprite_new -> palette_set with semantic keys (o=outline, a/A/1 skin shadow/base/highlight...) or palette_from_image to match a background -> \
part_new + draw (block in silhouettes with polygon/ellipse, then refine with the 'rows' op written in palette keys) or part_import/part_extract from an image -> \
anim_set -> review (silhouette + zoom grid + frame strip) and lint, fix, repeat -> export for Godot.\n\
Tips: draw each part once and animate by moving/rotating parts instead of redrawing frames; create variant parts (mouth_open, arm_l_bent) for shape changes; \
anim_copy with mirror=true turns walk_right into walk_left; use part_get to read exact pixels before editing; every change autosaves; undo/redo exist.";

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
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest);
        }
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
    let k = render::auto_scale(p.img.width(), p.img.height(), 1, 1).min(16).max(4);
    let mut img = render::present(&p.img, k, None);
    render::marker(&mut img, p.pivot[0], p.pivot[1], k, Rgba([255, 0, 200, 255]));
    Ok(Out::Image(img))
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
    pivot: Option<[f32; 2]>,
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
    part: String,
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
}
#[derive(Deserialize)]
struct SpriteRef {
    sprite: String,
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
                let sprite = Sprite::load(&dir)?;
                let n = sprite.name.clone();
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
        match f(&mut doc.sprite) {
            Ok(o) => {
                doc.sprite.save(&doc.dir)?;
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
        t += "animations:\n";
        for (n, a) in &s.animations {
            let d: Vec<String> = a.frames.iter().map(|f| f.duration_ms.to_string()).collect();
            t += &format!("  {n}: {} frames, loop={}, durations ms [{}]\n", a.frames.len(), a.looped, d.join(","));
        }
        t
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
                    if ox != 0 || oy != 0 {
                        s.origin[0] += ox;
                        s.origin[1] += oy;
                        for an in s.animations.values_mut() {
                            for f in &mut an.frames {
                                for p in &mut f.poses {
                                    p.x += ox;
                                    p.y += oy;
                                }
                            }
                        }
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
                        if let Some(n) = s.palette.by_key(e.key) {
                            if n != e.color {
                                remap.push((e.color, n));
                            }
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
                        for an in s.animations.values_mut() {
                            for f in &mut an.frames {
                                for p in &mut f.poses {
                                    if p.part == a.part {
                                        p.part = new.clone();
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
                    let used: Vec<String> = s
                        .animations
                        .iter()
                        .filter(|(_, an)| an.frames.iter().any(|f| f.poses.iter().any(|p| p.part == a.part)))
                        .map(|(n, _)| n.clone())
                        .collect();
                    if !used.is_empty() {
                        bail!("part '{}' is used by animations: {}", a.part, used.join(", "));
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
                        for p in img.pixels_mut() {
                            let d: i32 = (0..3).map(|i| (p[i] as i32 - kc[i] as i32).abs()).sum();
                            if d < 40 {
                                *p = CLEAR;
                            }
                        }
                    }
                    if let Some([w, h]) = a.size {
                        if w == 0 || h == 0 || w > 1024 || h > 1024 {
                            bail!("size must be 1..=1024");
                        }
                        img = render::box_resize(&img, w, h);
                    }
                    if img.width() > 1024 || img.height() > 1024 {
                        bail!("imported image is {}x{}; pass size to scale it down", img.width(), img.height());
                    }
                    threshold(&mut img, a.alpha_threshold.unwrap_or(128));
                    if a.quantize {
                        quantize_to(&mut img, &s.palette.colors())?;
                    }
                    let pivot = a.pivot.unwrap_or([img.width() as f32 / 2.0, img.height() as f32 / 2.0]);
                    let (w, h) = img.dimensions();
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
                    for (i, op) in a.ops.iter().enumerate() {
                        draw::apply(&mut p.img, op, &pal).map_err(|e| anyhow!("op {i}: {e}"))?;
                    }
                    let mut out = vec![Out::Text(format!("applied {} ops to '{}'", a.ops.len(), a.part))];
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
                    None => s.palette.darkest(),
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
                self.mutate(&a.sprite, |s| {
                    s.part(&a.part)?;
                    if let Some(r) = &a.replace_with {
                        s.part(r)?;
                    }
                    let an = s.anim_mut(&a.anim)?;
                    let idx: Vec<usize> = if a.frames.is_empty() { (0..an.frames.len()).collect() } else { a.frames.clone() };
                    let mut changed = 0;
                    for i in idx {
                        let len = an.frames.len();
                        let f = an.frames.get_mut(i).ok_or_else(|| anyhow!("frame {i} out of range ({len} frames)"))?;
                        let pos = f.poses.iter().position(|p| p.part == a.part);
                        if a.remove {
                            if let Some(pi) = pos {
                                f.poses.remove(pi);
                                changed += 1;
                            }
                            continue;
                        }
                        let pi = match pos {
                            Some(pi) => pi,
                            None => {
                                f.poses.push(Pose { part: a.part.clone(), x: 0, y: 0, rot: 0.0, flip_h: false, flip_v: false });
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
                    Ok(vec![Out::Text(format!("updated '{}' in {changed} frame(s) of '{}'", a.part, a.anim))])
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
                        let w = s.width as i32;
                        for f in &mut an.frames {
                            for p in &mut f.poses {
                                p.x = w - p.x;
                                p.flip_h = !p.flip_h;
                                p.rot = -p.rot;
                            }
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
                            for pose in &f.poses {
                                render::marker(&mut out, pose.x as f32, pose.y as f32, k, Rgba([0, 200, 255, 255]));
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
                let mut img = render::box_resize(&src, a.width, h);
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
        "x": {"type": "integer", "description": "canvas x where the part's pivot goes"},
        "y": {"type": "integer"},
        "rot": {"type": "number", "description": "degrees clockwise around the pivot"},
        "flip_h": {"type": "boolean"}, "flip_v": {"type": "boolean"}
    }, "required": ["part"]});
    let frame = json!({"type": "object", "properties": {
        "duration_ms": {"type": "integer", "default": 100},
        "poses": {"type": "array", "items": pose, "description": "back to front"}
    }, "required": ["poses"]});
    let op = json!({"type": "object", "description":
        "Drawing op, discriminated by 'op':\n\
         {op:'rows', x?, y?, rows:[str], legend?:{char:color}, erase_dots?} — paint/patch a pixel block; chars = palette keys, legend chars override, '.'/' ' = unchanged (erase if erase_dots), '_' = erase\n\
         {op:'pixels', points:[[x,y]...], color}\n\
         {op:'line', points:[[x,y],[x,y],...], color} — polyline\n\
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
        "properties": {"op": {"type": "string", "enum": ["rows","pixels","line","rect","ellipse","polygon","fill","replace","outline","shift","flip","clear","copy"]}},
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
            "region": rect, "append": {"type": "boolean", "description": "add to existing palette instead of replacing"}
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
        tool("part_import", "Create/replace a part from an image file (e.g. a generated character), optionally cropped, scaled down (box filter), background-keyed and snapped to the palette.", json!({
            "sprite": sprite, "part": {"type": "string"}, "image": {"type": "string"}, "region": rect,
            "size": {"type": "array", "items": {"type": "integer"}, "description": "[w, h] target size"},
            "quantize": {"type": "boolean", "description": "snap colors to the sprite palette"},
            "alpha_threshold": {"type": "integer", "description": "alpha below this becomes transparent (default 128)"},
            "key_color": {"type": "string", "description": "make this background color transparent"},
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
        tool("review", "One control sheet for self-review: 1:1 size, black silhouette (readability check), zoom with pixel grid + coordinate labels, and (for animations) all frames in a strip. grid_text=true also returns the composed frame as text rows.", json!({
            "sprite": sprite, "part": {"type": "string"}, "anim": {"type": "string"}, "frame": {"type": "integer"},
            "zoom": {"type": "integer"}, "grid_text": {"type": "boolean"}, "palette": {"type": "string", "description": "palette variant name"}
        }), &["sprite"]),
        tool("lint", "Check for common pixel-art problems: orphan pixels, doubled outline corners (jaggies), off-palette colors, parts clipped by the canvas, feet not touching the origin line / drifting between frames.", json!({
            "sprite": sprite, "outline": {"type": "string", "description": "outline color/key (default: darkest palette color)"}
        }), &["sprite"]),
        tool("frame_delete", "Delete a frame.", json!({"sprite": sprite, "anim": {"type": "string"}, "index": {"type": "integer"}}), &["sprite", "anim", "index"]),
        tool("pose_set", "Tweak one part's pose across frames (all frames if 'frames' omitted): absolute x/y/rot/flips, relative dx/dy, swap the part (replace_with, e.g. mouth_closed -> mouth_open), reorder (z = draw index), or remove. Adds the pose if missing.", json!({
            "sprite": sprite, "anim": {"type": "string"}, "frames": {"type": "array", "items": {"type": "integer"}},
            "part": {"type": "string"}, "x": {"type": "integer"}, "y": {"type": "integer"}, "dx": {"type": "integer"}, "dy": {"type": "integer"},
            "rot": {"type": "number"}, "flip_h": {"type": "boolean"}, "flip_v": {"type": "boolean"},
            "replace_with": {"type": "string"}, "z": {"type": "integer"}, "remove": {"type": "boolean"}
        }), &["sprite", "anim", "part"]),
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
            "dither_strength": {"type": "number", "description": "0-2 (default 1, floyd 0.75)"}
        }), &["image", "output", "width"]),
        tool("undo", "Undo the last change to a sprite.", json!({"sprite": sprite}), &["sprite"]),
        tool("redo", "Redo.", json!({"sprite": sprite}), &["sprite"]),
    ]
}
