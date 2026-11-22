use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, RgbaImage};
use serde_json::json;

use crate::color::{Px, apply_remap};
use crate::model::Sprite;
use crate::render;

fn find_godot_root(dir: &Path) -> Option<PathBuf> {
    let mut cur = std::fs::canonicalize(dir).ok()?;
    loop {
        if cur.join("project.godot").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Writes <name>.png (sheet: one row per animation), <name>.json (Aseprite-style)
/// and <name>.tres (Godot 4 SpriteFrames). Returns a human-readable summary.
pub fn export(s: &Sprite, dir: &Path, res_dir: Option<&str>) -> Result<String> {
    if s.animations.is_empty() {
        bail!("sprite has no animations to export");
    }
    std::fs::create_dir_all(dir)?;
    let (w, h) = (s.width, s.height);
    let cols = s.animations.values().map(|a| a.frames.len()).max().unwrap_or(1).max(1) as u32;
    let rows = s.animations.len() as u32;
    let mut sheet = RgbaImage::new(cols * w, rows * h);

    let mut frames_json = vec![];
    let mut tags_json = vec![];
    let mut tres_subs = String::new();
    let mut tres_anims = vec![];
    let mut sub_count = 0;
    let mut idx = 0usize;

    for (row, (name, anim)) in s.animations.iter().enumerate() {
        let start = idx;
        let mut frame_entries = vec![];
        for (col, f) in anim.frames.iter().enumerate() {
            let img = render::compose(s, f)?;
            let (x, y) = (col as u32 * w, row as u32 * h);
            image::imageops::replace(&mut sheet, &img, x as i64, y as i64);
            frames_json.push(json!({
                "filename": format!("{name}_{col}"),
                "frame": {"x": x, "y": y, "w": w, "h": h},
                "duration": f.duration_ms,
            }));
            let id = format!("AtlasTexture_{sub_count}");
            sub_count += 1;
            tres_subs.push_str(&format!(
                "[sub_resource type=\"AtlasTexture\" id=\"{id}\"]\natlas = ExtResource(\"1_sheet\")\nregion = Rect2({x}, {y}, {w}, {h})\n\n"
            ));
            frame_entries.push(format!(
                "{{\n\"duration\": {:.3},\n\"texture\": SubResource(\"{id}\")\n}}",
                f.duration_ms as f32 / 100.0
            ));
            idx += 1;
        }
        if !anim.frames.is_empty() {
            tags_json.push(json!({"name": name, "from": start, "to": idx - 1, "direction": "forward"}));
        }
        tres_anims.push(format!(
            "{{\n\"frames\": [{}],\n\"loop\": {},\n\"name\": &\"{name}\",\n\"speed\": 10.0\n}}",
            frame_entries.join(", "),
            anim.looped
        ));
    }

    let png_name = format!("{}.png", s.name);
    sheet.save(dir.join(&png_name))?;
    let mut extra = vec![];
    for vname in s.palette_variants.keys() {
        let mut v = sheet.clone();
        apply_remap(&mut v, &s.variant_remap(Some(vname))?);
        let n = format!("{}_{vname}.png", s.name);
        v.save(dir.join(&n))?;
        extra.push(format!("{n} (palette variant, same layout)"));
    }
    if let Some([px, py, pw, ph]) = s.portrait {
        let an = s.animations.get("idle").or_else(|| s.animations.values().next()).unwrap();
        if let Some(f) = an.frames.first() {
            let full = render::compose(s, f)?;
            let crop = image::imageops::crop_imm(&full, px, py, pw, ph).to_image();
            crop.save(dir.join(format!("{}_portrait.png", s.name)))?;
            render::upscale(&crop, 4).save(dir.join(format!("{}_portrait_4x.png", s.name)))?;
            extra.push(format!("{0}_portrait.png + {0}_portrait_4x.png (from first frame of idle)", s.name));
        }
    }

    let meta = json!({
        "frames": frames_json,
        "meta": {
            "app": "freesprite",
            "image": png_name,
            "size": {"w": sheet.width(), "h": sheet.height()},
            "frameTags": tags_json,
            "origin": {"x": s.origin[0], "y": s.origin[1]},
            "portrait": s.portrait.map(|[x, y, w, h]| json!({"x": x, "y": y, "w": w, "h": h})),
        }
    });
    std::fs::write(dir.join(format!("{}.json", s.name)), serde_json::to_string_pretty(&meta)?)?;

    let mut note = String::new();
    let res_path = match res_dir {
        Some(r) => format!("{}/{png_name}", r.trim_end_matches('/')),
        None => match find_godot_root(dir) {
            Some(root) => {
                let abs = std::fs::canonicalize(dir)?;
                let rel = abs.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                if rel.is_empty() { format!("res://{png_name}") } else { format!("res://{rel}/{png_name}") }
            }
            None => {
                note = "\nwarning: no project.godot found above the export dir; texture path in .tres is a guess — pass godot_res_dir.".into();
                format!("res://{png_name}")
            }
        },
    };
    let tres = format!(
        "[gd_resource type=\"SpriteFrames\" load_steps={} format=3]\n\n[ext_resource type=\"Texture2D\" path=\"{res_path}\" id=\"1_sheet\"]\n\n{tres_subs}[resource]\nanimations = [{}]\n",
        sub_count + 2,
        tres_anims.join(", ")
    );
    std::fs::write(dir.join(format!("{}.tres", s.name)), tres)?;

    Ok(format!(
        "exported to {}:\n  {png_name} ({}x{}, {} animation rows, frame {w}x{h})\n  {}.json (Aseprite-style)\n  {}.tres (Godot SpriteFrames, texture {res_path}){}\n\
         Godot: AnimatedSprite2D with sprite_frames = {}.tres, centered = false, offset = Vector2({}, {}) so the node position is the sprite origin; use nearest texture filtering.{note}",
        dir.display(),
        sheet.width(),
        sheet.height(),
        rows,
        s.name,
        s.name,
        extra.iter().map(|e| format!("\n  {e}")).collect::<String>(),
        s.name,
        -s.origin[0],
        -s.origin[1],
    ))
}

pub struct GifScene<'a> {
    pub image: &'a RgbaImage,
    pub x: i32,
    pub y: i32,
    /// movement per frame (e.g. walking across the room)
    pub step: [i32; 2],
    pub loops: u32,
}

pub fn gif(s: &Sprite, anim: &str, path: &Path, scale: u32, bg: Option<Px>, palette: Option<&str>, scene: Option<GifScene>) -> Result<()> {
    let remap = s.variant_remap(palette)?;
    let a = s.anim(anim)?;
    if a.frames.is_empty() {
        bail!("animation '{anim}' has no frames");
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut enc = GifEncoder::new_with_speed(file, 10);
    enc.set_repeat(if a.looped { Repeat::Infinite } else { Repeat::Finite(0) })?;
    if let Some(sc) = &scene {
        let mut n = 0;
        for _ in 0..sc.loops.max(1) {
            for f in &a.frames {
                let mut img = render::compose(s, f)?;
                apply_remap(&mut img, &remap);
                let mut room = sc.image.clone();
                let (x, y) = (sc.x + sc.step[0] * n, sc.y + sc.step[1] * n);
                render::paste(&mut room, &img, x - s.origin[0], y - s.origin[1]);
                enc.encode_frame(image::Frame::from_parts(render::upscale(&room, scale), 0, 0, Delay::from_numer_denom_ms(f.duration_ms, 1)))?;
                n += 1;
            }
        }
        return Ok(());
    }
    for f in &a.frames {
        let mut img = render::compose(s, f)?;
        apply_remap(&mut img, &remap);
        let img = match bg {
            Some(_) => render::present(&img, scale, bg),
            None => render::upscale(&img, scale),
        };
        enc.encode_frame(image::Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(f.duration_ms, 1)))?;
    }
    Ok(())
}
