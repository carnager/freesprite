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

pub fn lint(s: &Sprite, outline: Option<Px>) -> Result<String> {
    let mut out: Vec<String> = vec![];
    let n8 = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

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
        if let Some(o) = outline {
            // in a 2x2 block, 3 outline pixels forming an L -> the elbow pixel is a double
            for y in 0..h - 1 {
                for x in 0..w - 1 {
                    let cells = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
                    let is: Vec<bool> = cells.iter().map(|(cx, cy)| opaque(img, *cx, *cy) == Some(o)).collect();
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

    let m = 64;
    let oy = s.origin[1];
    for (an_name, an) in &s.animations {
        let mut feet = vec![];
        let mut centers = vec![];
        for (i, f) in an.frames.iter().enumerate() {
            let img = render::compose_margin(s, f, m)?;
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
            if f.poses.is_empty() {
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
        let bad: Vec<String> = feet.iter().filter(|(_, y)| *y != oy).map(|(i, y)| format!("frame {i}: {:+}", y - oy)).collect();
        if !bad.is_empty() {
            out.push(format!(
                "anim '{an_name}': lowest pixel doesn't touch the origin line y={oy} (- floats, + sinks): {}",
                bad.join(", ")
            ));
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
