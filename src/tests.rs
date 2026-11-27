//! Behavior tests: drawing ops, transforms, rigs, and whole tool workflows through the
//! same entry point the MCP server uses.

use std::path::PathBuf;

use image::{Rgba, RgbaImage};
use serde_json::{Value, json};

use crate::color::{Palette, hue_ramp, lightness, palette_from_specs};
use crate::draw::{self, Op, pixel_perfect};
use crate::model::{Frame, Key, Part, Pose, Sprite};
use crate::render;
use crate::tools::Workspace;

const K: Rgba<u8> = Rgba([20, 10, 10, 255]);
const R: Rgba<u8> = Rgba([200, 40, 40, 255]);

fn pal() -> Palette {
    let specs: Vec<crate::color::PalSpec> = serde_json::from_value(json!([
        {"key": "o", "color": "#140a0a"},
        {"key": "a", "color": "#5a1830", "ramp": "red"},
        {"key": "b", "color": "#a02838", "ramp": "red"},
        {"key": "c", "color": "#c82828"},
        {"key": "d", "color": "#ff8060", "ramp": "red"},
    ]))
    .unwrap();
    palette_from_specs(&specs).unwrap()
}

fn op(v: Value) -> Op {
    serde_json::from_value(v).unwrap()
}

fn opaque(img: &RgbaImage) -> usize {
    img.pixels().filter(|p| p[3] > 0).count()
}

/// No 2x2 block holding exactly three pixels of color c (an L-shaped doubled corner).
fn no_doubles(img: &RgbaImage, c: Rgba<u8>) -> bool {
    for y in 0..img.height() - 1 {
        for x in 0..img.width() - 1 {
            let n = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)].iter().filter(|(a, b)| *img.get_pixel(*a, *b) == c).count();
            if n == 3 {
                return false;
            }
        }
    }
    true
}

// ---------- drawing ----------

#[test]
fn pixel_perfect_removes_elbows() {
    let path = [(0, 0), (1, 0), (1, 1), (2, 1), (2, 2)];
    assert_eq!(pixel_perfect(&path), vec![(0, 0), (1, 1), (2, 2)]);
    // straight runs are untouched
    let straight = [(0, 0), (1, 0), (2, 0), (3, 0)];
    assert_eq!(pixel_perfect(&straight), straight.to_vec());
}

#[test]
fn clean_line_has_no_doubled_corners() {
    let p = pal();
    let mut img = RgbaImage::new(24, 24);
    draw::apply(&mut img, &op(json!({"op": "line", "points": [[1, 1], [20, 9], [5, 22]], "color": "o", "clean": true})), &p).unwrap();
    assert!(no_doubles(&img, p.by_key('o').unwrap()));
}

#[test]
fn closed_curve_fills_and_strokes_cleanly() {
    let p = pal();
    let mut img = RgbaImage::new(32, 32);
    let o = op(json!({"op": "curve", "points": [[16, 2], [29, 16], [16, 29], [3, 16]], "closed": true, "fill": true, "color": "c", "stroke": "o"}));
    draw::apply(&mut img, &o, &p).unwrap();
    let (fill, line) = (p.by_key('c').unwrap(), p.by_key('o').unwrap());
    assert_eq!(*img.get_pixel(16, 16), fill, "inside is filled");
    assert_eq!(*img.get_pixel(0, 0), Rgba([0, 0, 0, 0]), "outside stays empty");
    assert!(img.pixels().any(|px| *px == line));
    assert!(no_doubles(&img, line), "pixel-perfect edge");
    // the edge is closed: flood from the center can't escape to the corner
    let mut probe = img.clone();
    draw::apply(&mut probe, &op(json!({"op": "fill", "x": 16, "y": 16, "color": "#00ff00"})), &p).unwrap();
    assert_eq!(*probe.get_pixel(0, 0), Rgba([0, 0, 0, 0]));
}

#[test]
fn bezier_point_counts_are_checked() {
    let p = pal();
    let mut img = RgbaImage::new(8, 8);
    let bad = op(json!({"op": "curve", "kind": "bezier", "points": [[0, 0], [1, 1], [2, 2], [3, 3], [4, 4]], "color": "o"}));
    assert!(draw::apply(&mut img, &bad, &p).is_err());
    let ok = op(json!({"op": "curve", "kind": "bezier", "points": [[0, 7], [3, 0], [7, 7]], "color": "o"}));
    draw::apply(&mut img, &ok, &p).unwrap();
    assert!(opaque(&img) > 5);
}

#[test]
fn huge_shapes_are_clipped_not_hung() {
    let p = pal();
    let mut img = RgbaImage::new(16, 16);
    let t = std::time::Instant::now();
    draw::apply(&mut img, &op(json!({"op": "rect", "x": -1000000, "y": -1000000, "w": 2000000, "h": 2000000, "color": "o", "fill": true})), &p).unwrap();
    draw::apply(&mut img, &op(json!({"op": "ellipse", "x": -100000, "y": -100000, "w": 200000, "h": 200000, "color": "c", "fill": true})), &p).unwrap();
    draw::apply(&mut img, &op(json!({"op": "clear", "rect": [-500000, -500000, 1000000, 1000000]})), &p).unwrap();
    assert!(t.elapsed().as_secs() < 2, "loops must be bounded by the image");
    assert_eq!(opaque(&img), 0);
    let far = op(json!({"op": "line", "points": [[0, 0], [10_000_000, 3]], "color": "o"}));
    assert!(draw::apply(&mut img, &far, &p).is_err());
}

#[test]
fn mirror_copies_left_to_right() {
    let p = pal();
    let mut img = RgbaImage::new(10, 4);
    img.put_pixel(1, 1, R);
    img.put_pixel(9, 3, K); // right side gets overwritten by the mirror
    draw::apply(&mut img, &op(json!({"op": "mirror"})), &p).unwrap();
    assert_eq!(*img.get_pixel(8, 1), R);
    assert_eq!(*img.get_pixel(9, 3), Rgba([0, 0, 0, 0]));
    // odd width: the center column maps onto itself
    let mut odd = RgbaImage::new(5, 1);
    odd.put_pixel(0, 0, R);
    odd.put_pixel(2, 0, K);
    draw::apply(&mut odd, &op(json!({"op": "mirror"})), &p).unwrap();
    assert_eq!(*odd.get_pixel(4, 0), R);
    assert_eq!(*odd.get_pixel(2, 0), K);
}

#[test]
fn shade_darkens_the_far_side_only() {
    let p = pal();
    let (a, b, d) = (p.by_key('a').unwrap(), p.by_key('b').unwrap(), p.by_key('d').unwrap());
    let mut img = RgbaImage::new(12, 12);
    draw::apply(&mut img, &op(json!({"op": "rect", "x": 2, "y": 2, "w": 8, "h": 8, "color": "b", "fill": true})), &p).unwrap();
    draw::apply(&mut img, &op(json!({"op": "shade", "light": [-1, -1], "depth": 1, "highlight": 1})), &p).unwrap();
    assert_eq!(*img.get_pixel(9, 9), a, "bottom-right corner is in shadow");
    assert_eq!(*img.get_pixel(9, 5), a, "right edge is in shadow");
    assert_eq!(*img.get_pixel(2, 2), d, "top-left is lit");
    assert_eq!(*img.get_pixel(5, 5), b, "interior keeps the base tone");
    // a color without a ramp is left alone
    let mut flat = RgbaImage::new(6, 6);
    draw::apply(&mut flat, &op(json!({"op": "rect", "x": 1, "y": 1, "w": 4, "h": 4, "color": "c", "fill": true})), &p).unwrap();
    let before = flat.clone();
    draw::apply(&mut flat, &op(json!({"op": "shade"})), &p).unwrap();
    assert_eq!(flat, before);
}

#[test]
fn ramp_steps_and_contour() {
    let p = pal();
    let (a, b, d, c) = (p.by_key('a').unwrap(), p.by_key('b').unwrap(), p.by_key('d').unwrap(), p.by_key('c').unwrap());
    let mut img = RgbaImage::from_pixel(4, 1, b);
    draw::apply(&mut img, &op(json!({"op": "ramp", "steps": 1, "points": [[0, 0]]})), &p).unwrap();
    draw::apply(&mut img, &op(json!({"op": "ramp", "steps": -5, "rect": [1, 0, 1, 1]})), &p).unwrap();
    assert_eq!(*img.get_pixel(0, 0), d);
    assert_eq!(*img.get_pixel(1, 0), a, "clamped at the darkest shade");
    assert_eq!(*img.get_pixel(2, 0), b);
    // contour: the b pixel next to a different color becomes the line color
    let mut img = RgbaImage::from_pixel(3, 1, b);
    img.put_pixel(2, 0, c);
    draw::apply(&mut img, &op(json!({"op": "contour", "of": "b", "selout": true})), &p).unwrap();
    assert_eq!(*img.get_pixel(1, 0), a);
    assert_eq!(*img.get_pixel(0, 0), b);
}

#[test]
fn hue_ramp_is_ordered_and_keeps_the_base() {
    let base = Rgba([196, 46, 58, 255]);
    let r = hue_ramp(base, 5, 2, 12.0, 0.1);
    assert_eq!(r.len(), 5);
    assert_eq!(r[2], base);
    for w in r.windows(2) {
        assert!(lightness(w[0]) < lightness(w[1]), "dark to light");
    }
    // shadows lean blue, highlights lean toward yellow (more green than the base)
    assert!(r[0][2] as i32 - (r[0][0] as i32) > base[2] as i32 - base[0] as i32);
    assert!(r[4][1] as f32 / r[4][0] as f32 > base[1] as f32 / base[0] as f32);
}

// ---------- transforms and rigs ----------

fn dot_part(w: u32, h: u32, pivot: [f32; 2]) -> Part {
    let mut img = RgbaImage::new(w, h);
    img.put_pixel(0, 0, R);
    Part { pivot, img }
}

fn sprite() -> Sprite {
    serde_json::from_value(json!({"name": "t", "width": 40, "height": 40, "origin": [20, 40]})).unwrap()
}

fn pose(part: &str, x: i32, y: i32) -> Pose {
    Pose { part: part.into(), x, y, ..Default::default() }
}

#[test]
fn children_follow_parent_translation_rotation_and_flip() {
    let mut root = pose("body", 10, 10);
    let mut kid = pose("head", 5, 0);
    kid.parent = Some("body".into());
    let x = render::resolve(&[root.clone(), kid.clone()]).unwrap();
    assert_eq!((x[1].x, x[1].y), (15.0, 10.0));

    root.rot = 90.0;
    let x = render::resolve(&[root.clone(), kid.clone()]).unwrap();
    assert!((x[1].x - 10.0).abs() < 1e-3 && (x[1].y - 15.0).abs() < 1e-3, "rotated 90° clockwise: offset (5,0) -> (0,5)");
    assert_eq!(x[1].rot, 90.0);

    root.rot = 0.0;
    root.flip_h = true;
    kid.rot = 10.0;
    let x = render::resolve(&[root, kid]).unwrap();
    assert_eq!((x[1].x, x[1].y), (5.0, 10.0), "mirrored parent mirrors the offset");
    assert!(x[1].flip_h);
    assert_eq!(x[1].rot, -10.0, "and turns the child the other way");
}

#[test]
fn parent_cycles_and_missing_parents_are_errors() {
    let mut a = pose("a", 0, 0);
    let mut b = pose("b", 0, 0);
    a.parent = Some("b".into());
    b.parent = Some("a".into());
    assert!(render::resolve(&[a.clone(), b]).is_err());
    a.parent = Some("nope".into());
    assert!(render::resolve(&[a]).is_err());
}

#[test]
fn rig_frames_apply_keys() {
    let mut s = sprite();
    s.parts.insert("body".into(), dot_part(2, 2, [0.0, 0.0]));
    s.parts.insert("head".into(), dot_part(2, 2, [0.0, 0.0]));
    s.parts.insert("head_alt".into(), dot_part(2, 2, [0.0, 0.0]));
    let mut head = pose("head", 0, -5);
    head.parent = Some("body".into());
    s.rig = vec![pose("body", 10, 20), head];
    s.validate_poses(&s.rig.clone(), "rig").unwrap();

    let rest = render::compose(&s, &Frame::rig(100, vec![])).unwrap();
    assert_eq!(*rest.get_pixel(10, 20), R);
    assert_eq!(*rest.get_pixel(10, 15), R);

    // moving the body carries the head; swapping keeps the slot
    let f = Frame::rig(100, vec![Key { slot: "body".into(), dy: 2, ..Default::default() }, Key { slot: "head".into(), part: Some("head_alt".into()), ..Default::default() }]);
    let poses = s.frame_poses(&f).unwrap();
    assert_eq!(poses[1].part, "head_alt");
    assert_eq!(poses[1].slot(), "head");
    let img = render::compose(&s, &f).unwrap();
    assert_eq!(*img.get_pixel(10, 22), R);
    assert_eq!(*img.get_pixel(10, 17), R);

    // hidden parents still carry their children
    let f = Frame::rig(100, vec![Key { slot: "body".into(), hide: Some(true), dx: 3, ..Default::default() }]);
    let img = render::compose(&s, &f).unwrap();
    assert_eq!(*img.get_pixel(13, 20), Rgba([0, 0, 0, 0]));
    assert_eq!(*img.get_pixel(13, 15), R);

    // keys for unknown slots are rejected
    let bad = Frame::rig(100, vec![Key { slot: "tail".into(), dx: 1, ..Default::default() }]);
    assert!(s.validate_frame(&bad).is_err());
}

#[test]
fn frame_flip_mirrors_around_the_origin() {
    let mut s = sprite();
    s.parts.insert("dot".into(), dot_part(1, 1, [0.0, 0.0]));
    let f = Frame { duration_ms: 100, poses: vec![pose("dot", 25, 5)], keys: vec![], flip_h: true };
    let img = render::compose(&s, &f).unwrap();
    // origin x = 20: pixel 25 -> 2*20-1-25 = 14
    assert_eq!(*img.get_pixel(14, 5), R);
    assert_eq!(opaque(&img), 1);
}

#[test]
fn legacy_projects_still_load_and_draw() {
    // sprite.json as written by earlier versions: explicit poses, no slots/keys/rig
    let s: Sprite = serde_json::from_value(json!({
        "name": "old", "width": 8, "height": 8, "origin": [4, 8],
        "palette": [{"key": "o", "color": "#000000"}],
        "parts": {"p": {"pivot": [0.0, 0.0]}},
        "animations": {"idle": {"loop": true, "frames": [{"duration_ms": 100, "poses": [{"part": "p", "x": 2, "y": 3}]}]}}
    }))
    .unwrap();
    assert!(s.rig.is_empty());
    let f = &s.animations["idle"].frames[0];
    assert!(!s.is_rig_frame(f));
    assert_eq!(s.frame_poses(f).unwrap()[0].slot(), "p");
    // and serializing doesn't add noise for the new optional fields
    let v = serde_json::to_value(&s).unwrap();
    assert!(v.get("rig").is_none());
    assert!(v["animations"]["idle"]["frames"][0].get("keys").is_none());
}

// ---------- tools end to end ----------

struct Env {
    ws: Workspace,
    dir: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("freesprite-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Env { ws: Workspace::default(), dir }
    }
    fn path(&self) -> String {
        self.dir.to_string_lossy().into()
    }
    /// Calls a tool; returns its text. Panics with the message if the tool reports an error.
    fn ok(&mut self, tool: &str, args: Value) -> String {
        let (err, text) = self.call(tool, args);
        assert!(!err, "{tool} failed: {text}");
        text
    }
    fn err(&mut self, tool: &str, args: Value) -> String {
        let (err, text) = self.call(tool, args);
        assert!(err, "{tool} should have failed but said: {text}");
        text
    }
    fn call(&mut self, tool: &str, args: Value) -> (bool, String) {
        let r = self.ws.call(tool, args);
        let text = r["content"].as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n");
        (r["isError"].as_bool().unwrap(), text)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A small rigged character: legs, torso, head with a mouth set and blink lids.
fn build_character(e: &mut Env) {
    let p = e.path();
    e.ok("sprite_new", json!({"name": "hero", "dir": p, "width": 40, "height": 60, "palette": ["o=#1c1018"]}));
    e.ok("palette_ramp", json!({"sprite": "hero", "name": "skin", "base": "#f5bf98", "keys": "abcd"}));
    e.ok("palette_ramp", json!({"sprite": "hero", "name": "coat", "base": "#c42e3a", "keys": "efgh"}));
    e.ok("palette_ramp", json!({"sprite": "hero", "name": "pants", "base": "#34477e", "keys": "rstu"}));
    let part = |e: &mut Env, name: &str, w: u32, h: u32, pivot: [f32; 2], ops: Value| {
        e.ok("part_new", json!({"sprite": "hero", "part": name, "width": w, "height": h, "pivot": pivot}));
        e.ok("draw", json!({"sprite": "hero", "part": name, "ops": ops, "preview": false}));
    };
    part(e, "leg", 6, 16, [3.0, 0.0], json!([{"op": "rect", "x": 1, "y": 0, "w": 4, "h": 15, "color": "t", "fill": true}, {"op": "shade"}, {"op": "outline", "color": "o"}]));
    part(e, "torso", 14, 14, [7.0, 13.0], json!([
        {"op": "curve", "points": [[2, 13], [1, 3], [7, 1], [12, 3], [11, 13]], "closed": true, "fill": true, "color": "g", "stroke": "none"},
        {"op": "mirror"}, {"op": "shade", "highlight": 1}, {"op": "outline", "selout": true}
    ]));
    part(e, "head", 16, 16, [8.0, 15.0], json!([
        {"op": "ellipse", "x": 1, "y": 1, "w": 14, "h": 14, "color": "c", "fill": true},
        {"op": "pixels", "points": [[5, 6], [10, 6]], "color": "o"},
        {"op": "shade"}, {"op": "outline", "color": "o"}
    ]));
    for (m, rows) in [("mouth_rest", ["....", "oooo"]), ("mouth_a", ["oooo", "oaao"]), ("mouth_e", ["oooo", ".oo."]), ("mouth_o", [".oo.", ".oo."]), ("mouth_m", ["....", "oooo"])] {
        part(e, m, 4, 2, [2.0, 1.0], json!([{"op": "rows", "rows": rows}]));
    }
    part(e, "lids", 7, 1, [0.0, 0.0], json!([{"op": "rows", "rows": ["cc.oocc"]}]));
    e.ok("rig_set", json!({"sprite": "hero", "poses": [
        {"slot": "leg_far", "part": "leg", "x": 22, "y": 44},
        {"slot": "leg_near", "part": "leg", "x": 18, "y": 44},
        {"part": "torso", "x": 20, "y": 46},
        {"part": "head", "parent": "torso", "x": 0, "y": -12},
        {"slot": "mouth", "part": "mouth_rest", "parent": "head", "x": 0, "y": -3},
        {"part": "lids", "parent": "head", "x": -3, "y": -9, "hide": true}
    ]}));
}

#[test]
fn rigged_character_workflow() {
    let mut e = Env::new("workflow");
    build_character(&mut e);

    // generators
    let idle = e.ok("anim_generate", json!({"sprite": "hero", "preset": "idle", "blink": {"slot": "lids"}}));
    assert!(idle.contains("9 rig frames"), "{idle}");
    let walk = e.ok("anim_generate", json!({"sprite": "hero", "preset": "walk", "legs": ["leg_near", "leg_far"]}));
    assert!(walk.contains("px/s"), "{walk}");
    let talk = e.ok("anim_talk", json!({"sprite": "hero", "text": "Mama, open up!", "head": "head"}));
    assert!(talk.contains("visemes"), "{talk}");

    // the walk keeps the feet on the floor in every frame
    let lint = e.ok("lint", json!({"sprite": "hero"}));
    assert!(!lint.contains("anim 'walk': feet"), "{lint}");
    assert!(!lint.contains("anim 'idle': feet"), "{lint}");
    assert!(!lint.contains("can't be drawn"), "{lint}");
    assert!(!lint.contains("outside the canvas"), "{lint}");

    // pose_set on rig frames edits keys relative to the rest pose
    e.ok("pose_set", json!({"sprite": "hero", "anim": "idle", "frames": [0], "slot": "head", "rot": 8}));
    let info = e.ok("sprite_info", json!({"sprite": "hero"}));
    assert!(info.contains("rig (rest pose"), "{info}");
    let s = Sprite::load(&e.dir).unwrap();
    let k = s.animations["idle"].frames[0].keys.iter().find(|k| k.slot == "head").unwrap();
    assert_eq!(k.rot, 8.0);
    // setting it back to the rest value removes the key again
    e.ok("pose_set", json!({"sprite": "hero", "anim": "idle", "frames": [0], "slot": "head", "rot": 0}));
    let s = Sprite::load(&e.dir).unwrap();
    assert!(!s.animations["idle"].frames[0].keys.iter().any(|k| k.slot == "head"));

    // mirrored copy stays anchored at the origin
    e.ok("anim_copy", json!({"sprite": "hero", "from": "walk", "to": "walk_left", "mirror": true}));
    let s = Sprite::load(&e.dir).unwrap();
    let a = render::compose(&s, &s.animations["walk"].frames[0]).unwrap();
    let b = render::compose(&s, &s.animations["walk_left"].frames[0]).unwrap();
    assert_eq!(b, render::mirror_h(&a, s.origin[0]));

    // export + review run on rig animations
    let out = e.dir.join("export");
    e.ok("export", json!({"sprite": "hero", "dir": out.to_string_lossy(), "gif": false}));
    assert!(out.join("hero.png").exists() && out.join("hero.tres").exists());
    e.ok("review", json!({"sprite": "hero", "anim": "talk"}));

    // the rig can't lose a slot that animations still key
    let msg = e.err("rig_set", json!({"sprite": "hero", "poses": [{"part": "torso", "x": 20, "y": 46}]}));
    assert!(msg.contains("animation"), "{msg}");
    // and parts in use can't be deleted
    let msg = e.err("part_delete", json!({"sprite": "hero", "part": "mouth_a"}));
    assert!(msg.contains("talk"), "{msg}");
}

#[test]
fn renaming_a_part_keeps_rig_and_keys_working() {
    let mut e = Env::new("rename");
    build_character(&mut e);
    e.ok("anim_talk", json!({"sprite": "hero", "text": "ah"}));
    e.ok("part_edit", json!({"sprite": "hero", "part": "mouth_a", "rename": "mouth_open"}));
    e.ok("part_edit", json!({"sprite": "hero", "part": "head", "rename": "head_round"}));
    let s = Sprite::load(&e.dir).unwrap();
    for an in s.animations.values() {
        for f in &an.frames {
            s.validate_frame(f).unwrap();
        }
    }
    assert!(s.rig.iter().any(|p| p.part == "head_round" && p.slot() == "head"));
}

#[test]
fn errors_roll_back_and_paths_reuse_open_docs() {
    let mut e = Env::new("rollback");
    build_character(&mut e);
    let before = std::fs::read_to_string(e.dir.join("sprite.json")).unwrap();
    e.err("anim_set", json!({"sprite": "hero", "anim": "bad", "frames": [{"keys": [{"slot": "nope", "dx": 1}]}]}));
    assert_eq!(std::fs::read_to_string(e.dir.join("sprite.json")).unwrap(), before, "failed calls don't touch the project");
    // addressing the sprite by path finds the open document (undo history intact)
    e.ok("draw", json!({"sprite": "hero", "part": "leg", "ops": [{"op": "pixels", "points": [[0, 0]], "color": "o"}], "preview": false}));
    let p = e.path();
    e.ok("undo", json!({"sprite": p}));
    assert_eq!(std::fs::read_to_string(e.dir.join("sprite.json")).unwrap(), before);
    // no stray temp files from atomic saves
    let stray = std::fs::read_dir(&e.dir).unwrap().filter_map(|d| d.ok()).any(|d| d.file_name().to_string_lossy().ends_with(".tmp"));
    assert!(!stray);
}

#[test]
fn lint_explains_how_to_fix_sinking_feet() {
    let mut e = Env::new("feet");
    let p = e.path();
    e.ok("sprite_new", json!({"name": "f", "dir": p, "width": 8, "height": 10, "origin": [4, 9], "palette": ["o=#000000"]}));
    e.ok("part_new", json!({"sprite": "f", "part": "b", "width": 2, "height": 10, "pivot": [0, 0]}));
    e.ok("draw", json!({"sprite": "f", "part": "b", "ops": [{"op": "rect", "x": 0, "y": 0, "w": 2, "h": 10, "color": "o", "fill": true}], "preview": false}));
    e.ok("anim_set", json!({"sprite": "f", "anim": "idle", "frames": [{"poses": [{"part": "b", "x": 3, "y": 0}]}]}));
    let lint = e.ok("lint", json!({"sprite": "f"}));
    assert!(lint.contains("sink 1px") && lint.contains("origin [4, 10]"), "{lint}");
    // and following the advice actually fixes it
    e.ok("sprite_edit", json!({"sprite": "f", "origin": [4, 10]}));
    let lint = e.ok("lint", json!({"sprite": "f"}));
    assert!(!lint.contains("feet"), "{lint}");
}

#[test]
fn offset_moves_roots_only() {
    let mut e = Env::new("offset");
    let p = e.path();
    e.ok("sprite_new", json!({"name": "o", "dir": p, "width": 30, "height": 30, "palette": ["o=#000000"]}));
    e.ok("part_new", json!({"sprite": "o", "part": "dot", "width": 1, "height": 1, "pivot": [0, 0]}));
    e.ok("draw", json!({"sprite": "o", "part": "dot", "ops": [{"op": "pixels", "points": [[0, 0]], "color": "o"}], "preview": false}));
    e.ok("rig_set", json!({"sprite": "o", "poses": [{"part": "dot", "x": 5, "y": 5}, {"slot": "kid", "part": "dot", "parent": "dot", "x": 2, "y": 0}]}));
    e.ok("anim_set", json!({"sprite": "o", "anim": "rig", "frames": [{"keys": []}]}));
    e.ok("anim_set", json!({"sprite": "o", "anim": "explicit", "frames": [{"poses": [{"part": "dot", "x": 5, "y": 5}, {"slot": "kid", "part": "dot", "parent": "dot", "x": 2, "y": 0}]}]}));
    e.ok("sprite_edit", json!({"sprite": "o", "offset": [3, 0]}));
    let s = Sprite::load(&e.dir).unwrap();
    for an in ["rig", "explicit"] {
        let img = render::compose(&s, &s.animations[an].frames[0]).unwrap();
        let xs: Vec<u32> = (0..30).filter(|x| img.get_pixel(*x, 5)[3] > 0).collect();
        assert_eq!(xs, vec![8, 10], "{an}: everything moves by 3, children stay attached");
    }
}

#[test]
fn legacy_frames_may_place_a_part_twice() {
    let mut e = Env::new("twice");
    let p = e.path();
    e.ok("sprite_new", json!({"name": "d", "dir": p, "width": 10, "height": 10, "palette": ["o=#000000"]}));
    e.ok("part_new", json!({"sprite": "d", "part": "eye", "width": 1, "height": 1}));
    e.ok("anim_set", json!({"sprite": "d", "anim": "blink", "frames": [{"poses": [{"part": "eye", "x": 2, "y": 2}, {"part": "eye", "x": 6, "y": 2}]}]}));
    e.ok("anim_set", json!({"sprite": "d", "anim": "other", "frames": [{"poses": [{"part": "eye", "x": 1, "y": 1}]}]}));
    e.ok("pose_set", json!({"sprite": "d", "anim": "other", "part": "eye", "dx": 1}));
}

#[test]
fn bad_input_never_leaves_half_applied_changes() {
    let mut e = Env::new("panic");
    build_character(&mut e);
    let before = Sprite::load(&e.dir).unwrap().parts["leg"].img.clone();
    // the clear would run, then the non-finite curve fails: nothing may stick
    e.err("draw", json!({"sprite": "hero", "part": "leg", "ops": [{"op": "clear"}, {"op": "curve", "points": [[1e39, 0], [0, 0], [5, 5]], "closed": true, "fill": true, "color": "o"}]}));
    let got = e.ok("part_get", json!({"sprite": "hero", "part": "leg"}));
    assert!(got.contains('t'), "part was cleared in memory: {got}");
    assert_eq!(Sprite::load(&e.dir).unwrap().parts["leg"].img, before);
    e.err("pose_set", json!({"sprite": "hero", "anim": "x", "slot": "head", "x": 2147483647}));
    // removing an explicit frame's last pose would turn it into a rig frame
    e.ok("anim_set", json!({"sprite": "hero", "anim": "still", "frames": [{"poses": [{"part": "torso", "x": 20, "y": 46}]}]}));
    e.err("pose_set", json!({"sprite": "hero", "anim": "still", "part": "torso", "remove": true}));
}

#[test]
fn cel_shadow_corners_are_not_jaggies() {
    let mut e = Env::new("celshadow");
    let p = e.path();
    e.ok("sprite_new", json!({"name": "c", "dir": p, "width": 12, "height": 12, "palette": ["o=#000000"]}));
    e.ok("palette_ramp", json!({"sprite": "c", "name": "pants", "base": "#34477e", "keys": "rst"}));
    e.ok("part_new", json!({"sprite": "c", "part": "p", "width": 10, "height": 10}));
    e.ok("draw", json!({"sprite": "c", "part": "p", "ops": [{"op": "rect", "x": 1, "y": 1, "w": 7, "h": 7, "color": "s", "fill": true}, {"op": "shade"}, {"op": "outline", "color": "o"}], "preview": false}));
    let lint = e.ok("lint", json!({"sprite": "c"}));
    assert!(!lint.contains("doubled"), "{lint}");
}

#[test]
fn copy_handles_negative_sources() {
    let p = pal();
    let mut img = RgbaImage::new(16, 3);
    for x in 0..16 {
        img.put_pixel(x, 0, R);
    }
    draw::apply(&mut img, &op(json!({"op": "copy", "from": [-4, 0, 20, 1], "to": [-4, 2]})), &p).unwrap();
    assert!((0..16).all(|x| *img.get_pixel(x, 2) == R));
}

#[test]
fn lint_sees_jaggies_in_tinted_outlines() {
    let mut e = Env::new("tinted");
    let p = e.path();
    e.ok("sprite_new", json!({"name": "t", "dir": p, "width": 8, "height": 8, "palette": ["o=#000000"]}));
    let ramp = e.ok("palette_ramp", json!({"sprite": "t", "name": "skin", "base": "#f2cfae", "keys": "abc", "outline": "k"}));
    assert!(ramp.contains("k="), "{ramp}");
    e.ok("part_new", json!({"sprite": "t", "part": "p", "width": 4, "height": 4}));
    // an L of the skin ramp's outline shade (not the palette's darkest color)
    // (kept off the bottom row, where L corners are feet meeting the floor and are allowed)
    e.ok("draw", json!({"sprite": "t", "part": "p", "ops": [{"op": "rows", "rows": ["kk..", ".k..", "....", "cccc"]}], "preview": false}));
    let lint = e.ok("lint", json!({"sprite": "t"}));
    assert!(lint.contains("doubled outline corners"), "{lint}");
    // and outline selout on that ramp uses the tinted shade
    e.ok("part_new", json!({"sprite": "t", "part": "q", "width": 4, "height": 4}));
    e.ok("draw", json!({"sprite": "t", "part": "q", "ops": [{"op": "pixels", "points": [[1, 1]], "color": "b"}, {"op": "outline", "selout": true}], "preview": false}));
    let s = Sprite::load(&e.dir).unwrap();
    assert_eq!(*s.parts["q"].img.get_pixel(0, 1), s.palette.by_key('k').unwrap());
}

#[test]
fn talk_needs_mouth_shapes() {
    let mut e = Env::new("talk");
    let p = e.path();
    e.ok("sprite_new", json!({"name": "m", "dir": p, "width": 8, "height": 8, "palette": ["o=#000000"]}));
    e.ok("part_new", json!({"sprite": "m", "part": "mouth", "width": 2, "height": 1}));
    e.ok("rig_set", json!({"sprite": "m", "poses": [{"part": "mouth", "x": 4, "y": 4}]}));
    let msg = e.err("anim_talk", json!({"sprite": "m", "text": "hello"}));
    assert!(msg.contains("mouth_a"), "{msg}");
}

#[test]
fn spec_documents_every_tool_and_op() {
    let spec = include_str!("../docs/SPEC.md");
    for t in crate::tools::definitions() {
        let name = t["name"].as_str().unwrap();
        assert!(spec.contains(&format!("### `{name}`")), "docs/SPEC.md lacks tool '{name}'");
    }
    let ops = crate::tools::definitions().into_iter().find(|t| t["name"] == "draw").unwrap()["inputSchema"]["properties"]["ops"]["items"]["properties"]["op"]["enum"].clone();
    for op in ops.as_array().unwrap() {
        let op = op.as_str().unwrap();
        assert!(spec.contains(&format!("op:'{op}'")), "docs/SPEC.md lacks draw op '{op}'");
    }
}
