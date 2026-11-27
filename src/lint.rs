use anyhow::Result;
use image::RgbaImage;

use crate::color::Px;
use crate::model::Sprite;
use crate::render;

fn opaque(img: &RgbaImage, x: i32, y: i32) -> Option<Px> {
    if x < 0 || y < 0 || x as u32 >= img.width() || y as u32 >= img.height() {
        return None;
    }
    let p = *img.get_pixel(x as u32, y as u32);
    (p[3] > 0).then_some(p)
}

fn list(v: &[(i32, i32)]) -> String {
    let mut s = v.iter().take(12).map(|(x, y)| format!("({x},{y})")).collect::<Vec<_>>().join(" ");
    if v.len() > 12 {
        s += &format!(" … +{}", v.len() - 12);
    }
    s
}

/// Colors that act as line work: the given outline color, or by default the darkest palette
/// color plus the tinted outline shades made by `palette_ramp outline` (named "<ramp> outline").
/// Ordinary dark shading shades are not line work, so cel-shadow corners aren't reported.
fn line_colors(s: &Sprite, outline: Option<Px>) -> Vec<Px> {
    if let Some(o) = outline {
        return vec![o];
    }
    let mut v: Vec<Px> = s.palette.darkest().into_iter().collect();
    for e in &s.palette.0 {
        if e.ramp.is_some() && e.name.as_deref().is_some_and(|n| n.ends_with(" outline")) && !v.contains(&e.color) {
            v.push(e.color);
        }
    }
    v
}

pub fn lint(s: &Sprite, outline: Option<Px>) -> Result<String> {
    let mut out: Vec<String> = vec![];
    let n8 = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];
    let lines = line_colors(s, outline);

    for (name, part) in &s.parts {
        let img = &part.img;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let off = if s.palette.is_empty() { 0 } else { img.pixels().filter(|p| p[3] > 0 && s.palette.entry_of(**p).is_none()).count() };
        if off > 0 {
            out.push(format!("part '{name}': {off} off-palette pixels (part_get shows them as '?')"));
        }
        let semi = img.pixels().filter(|p| p[3] > 0 && p[3] < 255).count();
        if semi > 0 {
            out.push(format!("part '{name}': {semi} semi-transparent pixels (pixel art should be fully opaque or empty)"));
        }
        let (mut orphans, mut specks, mut doubles) = (vec![], vec![], vec![]);
        for y in 0..h {
            for x in 0..w {
                let Some(c) = opaque(img, x, y) else { continue };
                let neigh: Vec<Option<Px>> = n8.iter().map(|(dx, dy)| opaque(img, x + dx, y + dy)).collect();
                if neigh.iter().all(|n| n.is_none()) {
                    orphans.push((x, y));
                } else if neigh.iter().all(|n| n.is_some() && *n != Some(c)) {
                    let first = neigh[0];
                    if neigh.iter().all(|n| *n == first) {
                        specks.push((x, y));
                    }
                }
            }
        }
        let floor = (0..h).rev().find(|y| (0..w).any(|x| opaque(img, x, *y).is_some()));
        if !lines.is_empty() {
            // in a 2x2 block, 3 line pixels forming an L -> the elbow pixel is a double
            for y in 0..h - 1 {
                for x in 0..w - 1 {
                    let cells = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
                    let is: Vec<bool> = cells.iter().map(|(cx, cy)| opaque(img, *cx, *cy).is_some_and(|p| lines.contains(&p))).collect();
                    // 90° corners where a foot meets the floor line are intentional
                    if Some(y + 1) == floor {
                        continue;
                    }
                    if is.iter().filter(|b| **b).count() == 3 {
                        let missing = is.iter().position(|b| !*b).unwrap();
                        let elbow = cells[3 - missing];
                        // only count it if the missing cell is not outline-colored interior (thick lines are fine elsewhere)
                        if !doubles.contains(&elbow) {
                            doubles.push(elbow);
                        }
                    }
                }
            }
        }
        if !orphans.is_empty() {
            out.push(format!("part '{name}': {} orphan pixels (no neighbours): {}", orphans.len(), list(&orphans)));
        }
        if !specks.is_empty() {
            out.push(format!("part '{name}': {} single-pixel specks inside a uniform area (fine for eyes/highlights, else noise): {}", specks.len(), list(&specks)));
        }
        if !doubles.is_empty() {
            out.push(format!("part '{name}': {} doubled outline corners (L-shaped jaggies; remove the elbow pixel): {}", doubles.len(), list(&doubles)));
        }
    }

    // palette: shared colors make text rows ambiguous; one-color ramps can't shade or sel-out
    for (i, a) in s.palette.0.iter().enumerate() {
        if let Some(b) = s.palette.0[i + 1..].iter().find(|b| b.color == a.color) {
            out.push(format!("palette: keys '{}' and '{}' have the same color (pixels read back as '{}')", a.key, b.key, a.key));
        }
    }
    let mut ramps: std::collections::BTreeMap<&str, usize> = Default::default();
    for e in &s.palette.0 {
        if let Some(r) = &e.ramp {
            *ramps.entry(r).or_default() += 1;
        }
    }
    for (r, n) in ramps.iter().filter(|(_, n)| **n < 2) {
        out.push(format!("palette: ramp '{r}' has {n} color (shade/ramp/selout need at least 2; palette_ramp builds one)"));
    }

    if !s.rig.is_empty()
        && let Err(e) = s.validate_poses(&s.rig, "rig") {
            out.push(format!("{e:#}"));
        }
    if !s.animations.is_empty() {
        let mut used: std::collections::HashSet<&str> = s.rig.iter().map(|p| p.part.as_str()).collect();
        for an in s.animations.values() {
            for f in &an.frames {
                used.extend(f.poses.iter().map(|p| p.part.as_str()));
                used.extend(f.keys.iter().filter_map(|k| k.part.as_deref()));
            }
        }
        let unused: Vec<&str> = s.parts.keys().map(String::as_str).filter(|p| !used.contains(p)).collect();
        if !unused.is_empty() {
            out.push(format!("parts not used by the rig or any animation: {} (fine if they are spares)", unused.join(", ")));
        }
    }

    let m = 64;
    let oy = s.origin[1];
    for (an_name, an) in &s.animations {
        let mut feet = vec![];
        let mut centers = vec![];
        for (i, f) in an.frames.iter().enumerate() {
            let img = match render::compose_margin(s, f, m) {
                Ok(img) => img,
                Err(e) => {
                    out.push(format!("anim '{an_name}' frame {i}: can't be drawn: {e:#}"));
                    continue;
                }
            };
            let (w, h) = (s.width as i32, s.height as i32);
            let mut clipped = 0;
            let mut bottom: Option<i32> = None;
            for (x, y, p) in img.enumerate_pixels() {
                if p[3] == 0 {
                    continue;
                }
                let (cx, cy) = (x as i32 - m, y as i32 - m);
                if cx < 0 || cy < 0 || cx >= w || cy >= h {
                    clipped += 1;
                } else {
                    bottom = Some(bottom.map_or(cy, |b: i32| b.max(cy)));
                }
            }
            if clipped > 0 {
                out.push(format!("anim '{an_name}' frame {i}: {clipped} pixels outside the canvas (clipped)"));
            }
            if f.poses.is_empty() && !s.is_rig_frame(f) {
                out.push(format!("anim '{an_name}' frame {i}: empty frame"));
            }
            if let Some(b) = bottom {
                feet.push((i, b + 1));
                let xs: Vec<i32> = (b - 2..=b)
                    .flat_map(|y| (0..w).filter(move |_| y >= 0).map(move |x| (x, y)))
                    .filter(|(x, y)| img.get_pixel((x + m) as u32, (y + m) as u32)[3] > 0)
                    .map(|(x, _)| x)
                    .collect();
                if !xs.is_empty() {
                    centers.push(xs.iter().sum::<i32>() as f32 / xs.len() as f32);
                }
            }
        }
        let bad: Vec<(usize, i32)> = feet.iter().filter(|(_, y)| *y != oy).map(|(i, y)| (*i, y - oy)).collect();
        if !bad.is_empty() {
            // feet[i].1 is (lowest opaque row + 1): the line the sprite stands on
            let say = |d: i32| if d > 0 { format!("sink {d}px") } else { format!("float {}px", -d) };
            let d0 = bad[0].1;
            if bad.len() == feet.len() && bad.iter().all(|(_, d)| *d == d0) {
                out.push(format!(
                    "anim '{an_name}': in every frame the feet {} (lowest row is {}, but origin y={oy} is the line under the feet, so the lowest row should be {}). Fix: sprite_edit origin [{}, {}], or move the root placements (rig roots / root poses) by dy {}",
                    say(d0),
                    oy - 1 + d0,
                    oy - 1,
                    s.origin[0],
                    oy + d0,
                    -d0
                ));
            } else {
                out.push(format!(
                    "anim '{an_name}': feet off the origin line y={oy} (lowest row should be {}) in {}; fix with pose_set dy on those frames (ignore for jumps)",
                    oy - 1,
                    bad.iter().map(|(i, d)| format!("frame {i}: {}", say(*d))).collect::<Vec<_>>().join(", ")
                ));
            }
        }
        if centers.len() > 1 && !an_name.contains("walk") {
            let (mn, mx) = centers.iter().fold((f32::MAX, f32::MIN), |(a, b), c| (a.min(*c), b.max(*c)));
            if mx - mn > 2.0 {
                out.push(format!("anim '{an_name}': feet drift {:.1}px horizontally across frames (sliding?)", mx - mn));
            }
        }
    }

    if out.is_empty() {
        Ok("lint: no issues found".into())
    } else {
        Ok(format!("lint: {} finding(s)\n- {}", out.len(), out.join("\n- ")))
    }
}
