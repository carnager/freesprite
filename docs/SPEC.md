# freesprite specification

## Concepts

- **Sprite**: a fixed canvas (`width` × `height`) with an **origin** (the line under the feet, used to place it in the game: the lowest opaque row should be `origin.y - 1`), a **palette**, **parts**, an optional **rig** and **animations**.
- **Palette**: entries `{key, color, name?, ramp?}`. `key` is one character used in text rows (`.` `_` and space are reserved). `ramp` groups the shades of one material, ordered by perceived lightness. Shading (`shade`, `ramp` ops) steps along a ramp, and sel-out outlines use its darkest shade. `palette_ramp` builds hue-shifted ramps (cool shadows, warm highlights) with an optional deep outline shade. Changing a key's color recolors every pixel that uses it. **Palette variants** hold the same keys with different colors (lighting: day, dusk, tavern…).
- **Part**: a small RGBA pixel image with a **pivot** (part pixel coordinates; `.5` = pixel center). Rotation and placement happen around the pivot.
- **Pose**: places a part's pivot at `x,y` with optional `rot` (degrees, clockwise), `flip_h`, `flip_v` and `hide`. `slot` names the placement (default: the part name); it stays when the part is swapped. With `parent` (a slot), `x,y` are the offset from the parent's pivot in the parent's rotated and flipped space, rotation adds (reversed under a mirrored parent) and flips combine. Hidden poses still carry their children.
- **Rig**: the rest pose, a list of poses back to front with parent links (mouth → head → torso). Set with `rig_set`.
- **Animation**: `loop` flag plus frames. A **frame** has `duration_ms`, optional `flip_h` (mirror the whole frame around the origin x) and either
  - **explicit poses** (`poses`, drawn back to front), or
  - **keys** (rig frames, when `poses` is empty and a rig exists): the rest pose with per-slot changes `{slot, dx, dy, rot, part, hide, flip_h, flip_v, z}`. Offsets and rotation are relative to the rest pose; slots without a key stay at rest. Editing the rig updates every rig frame.
- Rotations that are not multiples of 90° use a RotSprite-style pipeline (scale2x ×3, rotate, nearest sample), so colors stay inside the palette. Prefer drawn variant parts for small, detailed shapes.

## Colors

Wherever a color is accepted: a palette key (`"o"`), `#rgb`, `#rrggbb`, `#rrggbbaa`, `@N` (palette index) or `transparent`.

## `rows` pixel format

Used by the `rows` draw op and returned by `part_get` / `review grid_text`:

```
..hhhh..   each char = palette key (or a one-off legend char)
.hAAAAh.   "." or " " = leave unchanged ("." erases with erase_dots: true)
hAwoAwoA   "_" = erase
```

## Project format

A sprite is a directory:

```
<dir>/sprite.json     name, width, height, origin, palette, palette_variants, portrait, parts (pivots), rig, animations
<dir>/parts/<part>.png
```

Every mutating tool call autosaves; files are written to a temp name and renamed, so a crash never leaves a half-written project. A call that fails changes nothing. `undo`/`redo` keep up to 200 steps per session. Projects from earlier versions (explicit poses only, no rig) load and render unchanged.

## Export format

`export` writes into the target directory:

- `<name>.png`: spritesheet, one row per animation (alphabetical), one column per frame, cells of canvas size
- `<name>_<variant>.png`: the same sheet per palette variant
- `<name>.json`: Aseprite-style `frames` (`filename`, `frame` rect, `duration`) and `meta` (`frameTags`, `origin`, `portrait`)
- `<name>.tres`: Godot 4 `SpriteFrames` (speed 10 fps, `duration` = ms / 100). The `res://` path comes from the nearest `project.godot`, or from `godot_res_dir`
- `<name>_portrait.png` and `<name>_portrait_4x.png` when a portrait crop is set
- `<name>_<anim>.gif`: previews at 4×

In Godot use an `AnimatedSprite2D` with `centered = false` and `offset = -origin`, and nearest texture filtering.

## Transport

MCP over stdio (newline-delimited JSON-RPC 2.0). Methods: `initialize`, `ping`, `tools/list`, `tools/call`. Tool results contain `text` and `image` (PNG) content.

## Tools

### `sprite_new`

Create a sprite project (a directory with sprite.json + parts/*.png; autosaved on every change).

| Param | Type | Required | Notes |
|---|---|---|---|
| `dir` | string | yes | directory for the project (absolute path recommended) |
| `height` | integer | yes |  |
| `name` | string | yes |  |
| `origin` | integer[] |  | anchor point, default [width/2, height] (bottom center) |
| `palette` | string \| object[] |  | entries: {key, color, name?, ramp?} or 'k=#hex' or '#hex' (auto key). Keys '.', '_' and space are reserved. |
| `width` | integer | yes |  |

### `sprite_open`

Open an existing sprite project directory.

| Param | Type | Required | Notes |
|---|---|---|---|
| `dir` | string | yes |  |

### `sprite_info`

Describe a sprite: canvas, origin, palette, parts, animations.

| Param | Type | Required | Notes |
|---|---|---|---|
| `sprite` | string | yes | sprite name (or its directory path) |

### `sprite_edit`

Change canvas size / origin / portrait crop. offset shifts all poses and the origin.

| Param | Type | Required | Notes |
|---|---|---|---|
| `height` | integer |  |  |
| `offset` | [integer; 2] |  |  |
| `origin` | [integer; 2] |  |  |
| `portrait` | [integer; 4] |  | [x, y, w, h] |
| `sprite` | string | yes | sprite name (or its directory path) |
| `width` | integer |  |  |

### `palette_set`

Replace the palette. Give every color a semantic one-char key (e.g. o=outline, a/A/1 = skin shadow/base/highlight) and optionally a name and a ramp group (used by selout outlines). Keys whose color changes recolor existing pixels (palette swap, e.g. night lighting).

| Param | Type | Required | Notes |
|---|---|---|---|
| `colors` | string \| object[] | yes | entries: {key, color, name?, ramp?} or 'k=#hex' or '#hex' (auto key). Keys '.', '_' and space are reserved. |
| `sprite` | string | yes | sprite name (or its directory path) |

### `palette_variant`

Create/update a named palette variant (lighting: day, dusk, tavern, moonlit...). Starts from the base palette (or the existing variant) and overrides the given keys. Use palette='<name>' in render/review/export_gif; export writes one sheet per variant.

| Param | Type | Required | Notes |
|---|---|---|---|
| `colors` | string \| object[] |  | entries: {key, color, name?, ramp?} or 'k=#hex' or '#hex' (auto key). Keys '.', '_' and space are reserved. |
| `name` | string | yes |  |
| `remove` | boolean |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `palette_from_image`

Extract a palette from an image (e.g. a game background) with median cut, so the character matches the scene.

| Param | Type | Required | Notes |
|---|---|---|---|
| `append` | boolean |  | add to existing palette instead of replacing |
| `colors` | integer | yes | max 62 |
| `image` | string | yes |  |
| `region` | [integer; 4] |  | [x, y, w, h] |
| `sprite` | string | yes | sprite name (or its directory path) |

### `part_new`

Create an empty part.

| Param | Type | Required | Notes |
|---|---|---|---|
| `height` | integer | yes |  |
| `part` | string | yes |  |
| `pivot` | number[] |  | rotation/placement point in part pixels, default center; e.g. shoulder joint for an arm |
| `sprite` | string | yes | sprite name (or its directory path) |
| `width` | integer | yes |  |

### `part_edit`

Rename a part, set its pivot, or resize its canvas (offset moves existing pixels and pivot).

| Param | Type | Required | Notes |
|---|---|---|---|
| `part` | string | yes |  |
| `pivot` | [number; 2] |  |  |
| `rename` | string |  |  |
| `resize` | object |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `part_delete`

Delete an unused part.

| Param | Type | Required | Notes |
|---|---|---|---|
| `part` | string | yes |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `part_duplicate`

Copy a part (optionally mirrored) under a new name — e.g. to make a variant like arm_bent.

| Param | Type | Required | Notes |
|---|---|---|---|
| `flip_h` | boolean |  |  |
| `flip_v` | boolean |  |  |
| `new_name` | string | yes |  |
| `part` | string | yes |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `part_import`

Create/replace a part from an image file (e.g. a generated character), optionally cropped, scaled down (box filter), background-keyed and snapped to the palette.

| Param | Type | Required | Notes |
|---|---|---|---|
| `alpha_threshold` | integer |  | alpha below this becomes transparent (default 128) |
| `image` | string | yes |  |
| `key_color` | string |  | make this background color transparent |
| `part` | string | yes |  |
| `pivot` | [number; 2] |  |  |
| `quantize` | boolean |  | snap colors to the sprite palette |
| `region` | [integer; 4] |  | [x, y, w, h] |
| `size` | integer[] |  | [w, h] target size |
| `sprite` | string | yes | sprite name (or its directory path) |

### `part_extract`

Copy a rectangle of one part into a new part (cut=true erases it from the source). Use to split an imported full character into limbs.

| Param | Type | Required | Notes |
|---|---|---|---|
| `cut` | boolean |  |  |
| `from` | string | yes |  |
| `part` | string | yes |  |
| `pivot` | [number; 2] |  |  |
| `region` | [integer; 4] | yes | [x, y, w, h] |
| `sprite` | string | yes | sprite name (or its directory path) |

### `part_get`

Read a part's exact pixels as text rows (palette keys, '.' = transparent) plus a preview. Use region to read just e.g. the face, then patch it back with a rows op at the same x,y.

| Param | Type | Required | Notes |
|---|---|---|---|
| `part` | string | yes |  |
| `region` | [integer; 4] |  | [x, y, w, h] |
| `sprite` | string | yes | sprite name (or its directory path) |

### `draw`

Apply a batch of drawing ops to a part (coordinates are part-local). Returns a preview (magenta = pivot).

| Param | Type | Required | Notes |
|---|---|---|---|
| `ops` | object[] | yes |  |
| `part` | string | yes |  |
| `preview` | boolean |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `anim_set`

Create or replace an animation with all its frames.

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string | yes |  |
| `frames` | object[] | yes |  |
| `loop` | boolean |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `frame_set`

Replace frame at index, or insert (insert=true or index == frame count).

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string | yes |  |
| `frame` | object | yes | Explicit frame: poses (back to front). Rig frame: no poses, only keys = changes to the rig's rest pose. |
| `index` | integer | yes |  |
| `insert` | boolean |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `frame_clone`

Duplicate a frame (count times), inserted at 'to' (default right after it). Then tweak with pose_set.

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string | yes |  |
| `count` | integer |  |  |
| `index` | integer | yes |  |
| `sprite` | string | yes | sprite name (or its directory path) |
| `to` | integer |  |  |

### `review`

One control sheet for self-review: 1:1 size, black silhouette (does the pose read?), value study (grayscale by perceived lightness: do face, hands and clothes separate?), zoom with pixel grid + coordinate labels, and (for animations) all frames in a strip. grid_text=true also returns the composed frame as text rows.

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string |  |  |
| `frame` | integer |  |  |
| `grid_text` | boolean |  |  |
| `palette` | string |  | palette variant name |
| `part` | string |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |
| `zoom` | integer |  |  |

### `lint`

Check for common pixel-art problems: orphan pixels, doubled outline corners (jaggies), off-palette colors, parts clipped by the canvas, feet not touching the origin line / drifting between frames.

| Param | Type | Required | Notes |
|---|---|---|---|
| `outline` | string |  | outline color/key (default: darkest palette color) |
| `sprite` | string | yes | sprite name (or its directory path) |

### `frame_delete`

Delete a frame.

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string | yes |  |
| `index` | integer | yes |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `pose_set`

Tweak one part's pose across frames (all frames if 'frames' omitted): absolute x/y/rot/flips, relative dx/dy, swap the part (replace_with, e.g. mouth_closed -> mouth_open), reorder (z = draw index), or remove. Explicit frames: pass part (adds the pose if missing). Rig frames: pass slot; this edits the frame's key, x/y/rot are the slot's local values (offset from rest is stored), remove hides it.

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string | yes |  |
| `dx` | integer |  |  |
| `dy` | integer |  |  |
| `flip_h` | boolean |  |  |
| `flip_v` | boolean |  |  |
| `frames` | integer[] |  |  |
| `part` | string |  |  |
| `remove` | boolean |  |  |
| `replace_with` | string |  |  |
| `rot` | number |  |  |
| `slot` | string |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |
| `x` | integer |  |  |
| `y` | integer |  |  |
| `z` | integer |  |  |

### `rig_set`

Set the character's rest pose: every slot (back to front) with its part, position and parent. Children hang from their parent's pivot, so moving or turning the torso carries head, arms, mouth and hat along. Frames without poses are then drawn from the rig plus their keys, so animations only list what changes. Returns the rest pose with pivots marked.

| Param | Type | Required | Notes |
|---|---|---|---|
| `poses` | object[] | yes | back to front; roots use canvas coordinates, children offsets from their parent's pivot |
| `sprite` | string | yes | sprite name (or its directory path) |

### `palette_ramp`

Add or replace a hue-shifted shading ramp built from one base color: shadows drift cool (violet), highlights warm (yellow) with less chroma. This is the standard pixel-art ramp instead of flat darker/lighter copies. keys = one char per shade, dark to light. Existing keys are recolored in place.

| Param | Type | Required | Notes |
|---|---|---|---|
| `base` | string | yes | #rrggbb, the mid tone |
| `base_index` | integer |  | which shade is the base (default: middle) |
| `hue_shift` | number |  | degrees of hue drift per step (default 12) |
| `keys` | string | yes | e.g. 'abcd' (dark -> light) |
| `name` | string | yes | ramp group, e.g. skin, jacket |
| `outline` | string |  | extra key for a deep tinted outline shade in this ramp; outline selout then draws colored DOTT-style lines that hold against any background |
| `sprite` | string | yes | sprite name (or its directory path) |
| `step` | number |  | lightness between shades, OKLab L (default 0.1) |

### `anim_talk`

Lip-sync: build a talk animation from a line of dialogue on a rig. Letters map to mouth shapes (A, E, O, M, F, L, C, rest) found as parts named <slot>_a, <slot>_e, <slot>_o, <slot>_m, <slot>_f, <slot>_l, <slot>_c, <slot>_rest (missing ones fall back to similar shapes), or given via visemes. Punctuation adds pauses; head bobs on open vowels.

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string |  | default 'talk' |
| `head` | string |  | slot to bob 1px on A/O |
| `loop` | boolean |  |  |
| `ms` | integer |  | time per letter (default 75) |
| `slot` | string |  | mouth slot in the rig (default 'mouth') |
| `sprite` | string | yes | sprite name (or its directory path) |
| `text` | string | yes |  |
| `visemes` | object |  | {"A": "mouth_open", "M": "mouth_closed", ...} |

### `anim_generate`

Generate a starting animation on a rig, then refine it. preset 'idle': 1px breathing of the body slots, with lag slots (hat, hair) arriving a frame late and an optional blink (swap to a closed-eyes part, or show a hidden lids slot). preset 'walk' (side view): legs swing in opposite directions, arms counter-swing, and the body drops so the feet stay on the floor; it reports the walk speed that avoids sliding.

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string |  |  |
| `arm_swing` | number |  | walk: arm swing degrees (default 18) |
| `arms` | string[] |  | walk: [near_arm, far_arm] slots, pivots at the shoulders |
| `blink` | object |  |  |
| `body` | string[] |  | idle: slots that rise (default torso/body) |
| `frames` | integer |  | walk: frames per cycle (default 8) |
| `lag` | string[] |  | idle: slots that follow one frame late |
| `legs` | string[] |  | walk: [near_leg, far_leg] slots, pivots at the hips |
| `ms` | integer |  | frame duration (idle 180, walk 100) |
| `preset` | `idle` \| `walk` | yes |  |
| `sprite` | string | yes | sprite name (or its directory path) |
| `swing` | number |  | walk: leg swing degrees (default 22) |

### `anim_delete`

Delete an animation.

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string | yes |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `anim_copy`

Copy an animation; mirror=true flips it horizontally (walk_right -> walk_left), reverse=true reverses frame order.

| Param | Type | Required | Notes |
|---|---|---|---|
| `from` | string | yes |  |
| `mirror` | boolean |  |  |
| `reverse` | boolean |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |
| `to` | string | yes |  |

### `render`

Render for visual inspection. part -> that part; anim+frame -> one frame (onion = show previous frame faded; markers = origin & pose points; scene = composite onto a background image with the sprite origin at x,y); anim only -> all frames in a strip; nothing -> every animation as rows (or all parts if no animations).

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string |  |  |
| `background` | string |  | color, or 'checker' (default) |
| `frame` | integer |  |  |
| `markers` | boolean |  |  |
| `onion` | boolean |  |  |
| `palette` | string |  | palette variant name |
| `part` | string |  |  |
| `scale` | integer |  |  |
| `scene` | object |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `export`

Export spritesheet PNG (one row per animation) + Aseprite-style JSON (frame rects, tags, durations, origin/foot pivot, portrait crop) + Godot 4 SpriteFrames .tres + a 4x GIF per animation. Texture res:// path is derived from the nearest project.godot unless godot_res_dir is given.

| Param | Type | Required | Notes |
|---|---|---|---|
| `dir` | string | yes |  |
| `gif` | boolean |  |  |
| `godot_res_dir` | string |  | e.g. res://characters/hero |
| `sprite` | string | yes | sprite name (or its directory path) |

### `export_gif`

Write an animated GIF of one animation — standalone, or inside a room background (scene: sprite origin at x,y; step moves it per frame, e.g. [2,0] to walk across; loops repeats the cycle).

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string | yes |  |
| `background` | string |  | palette key (one char), #rrggbb, #rrggbbaa, @N (palette index) or 'transparent' |
| `palette` | string |  | palette variant name |
| `path` | string | yes |  |
| `scale` | integer |  | default 4, or 3 with scene |
| `scene` | object |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |

### `pixelate`

Turn any image (PNG/JPEG/WebP, e.g. a generated background) into pixel art: box-downscale to width (x height, default keeps aspect), then reduce to N colors or to a sprite's palette.

| Param | Type | Required | Notes |
|---|---|---|---|
| `colors` | integer |  |  |
| `dither` | `none` \| `bayer` \| `floyd` |  | bayer = ordered retro VGA look, floyd = error diffusion; fights banding in skies/water |
| `dither_strength` | number |  | 0-2 (default 1, floyd 0.75) |
| `height` | integer |  |  |
| `image` | string | yes |  |
| `output` | string | yes |  |
| `sprite_palette` | string |  | sprite whose palette to use |
| `width` | integer | yes |  |

### `undo`

Undo the last change to a sprite.

| Param | Type | Required | Notes |
|---|---|---|---|
| `sprite` | string | yes | sprite name (or its directory path) |

### `redo`

Redo.

| Param | Type | Required | Notes |
|---|---|---|---|
| `sprite` | string | yes | sprite name (or its directory path) |

## Draw ops

Passed as `ops` to `draw`, applied in order. Coordinates are part-local. Shapes are clipped to the part, and coordinates beyond ±8192 are rejected. `draw` warns when an op adds off-palette colors.

- `{op:'rows', x?, y?, rows:[str], legend?:{char:color}, erase_dots?}`: paint/patch a pixel block; chars = palette keys, legend chars override, '.'/' ' = unchanged (erase if erase_dots), '_' = erase
- `{op:'pixels', points:[[x,y]...], color}`
- `{op:'line', points:[[x,y],[x,y],...], color, clean?}`: polyline; `clean` = pixel-perfect (drops the elbow pixel of L-shaped corners and hairpins)
- `{op:'curve', points:[[x,y]...], color, kind?, closed?, fill?, stroke?, clean?, pattern?, color2?}`: smooth line. `kind:'spline'` (default) passes through every point (Catmull-Rom). `'bezier'` uses control points: 3 = quadratic, 4 = cubic, 7/10/13… = chained cubics. `closed` + `fill` paints the inside with `color` (patterns work) and the edge with `stroke` (default `color`; `'none'` = no edge). Edges are pixel-perfect unless `clean: false`. Use it to block in organic cartoon shapes (heads, hair, jackets).
- `{op:'mirror', axis?:'x'|'y', at?, from?}`: copy one half onto the other, mirrored. `at` is the mirror line in pixel-edge coordinates (default: the middle; on odd sizes the center column maps to itself). `from` is `'left'`/`'right'` or `'top'`/`'bottom'` (default left/top). Draw half a face or body, mirror, then add the asymmetric details.
- `{op:'shade', light?:[dx,dy], depth?, highlight?, steps?, colors?, rect?, edge?}`: cel shading along palette ramps from one light direction (default `[-1,-1]`, top-left). Pixels within `depth` (default 1) of their region's far side step `steps` (default 1) shades darker; with `highlight` > 0, pixels that close to the lit side step lighter. A region ends where the ramp changes (`edge:'ramp'`, default), the color changes (`'color'`) or at transparency (`'alpha'`). Colors without a ramp are untouched. Run it before `outline`.
- `{op:'ramp', steps, rect?, points?, colors?}`: step pixels along their ramp (negative = darker), for hand-placed shading touches.
- `{op:'contour', of, color?, selout?, against?, diagonal?}`: inner line work. Pixels of color `of` that touch a different opaque color (or only `against`) become `color`, or with `selout` the darkest shade of `of`'s ramp. Example: the edge of a vest against the shirt.
- `{op:'rect', x,y,w,h, color, fill?, pattern?, color2?}`
- `{op:'ellipse', x,y,w,h, color, fill?, pattern?, color2?}`: bounding box
- `{op:'polygon', points:[[x,y]...], color, fill? (default true), pattern?, color2?}`
- `{op:'fill', x,y, color, pattern?, color2?}`: flood fill
- `pattern = 'checker' | 'bayer:N' (N 0-16 density) | 'hlines' | 'vlines': dither; 'on' pixels get color, others color2 (or untouched)`
- `{op:'replace', from, to}`: recolor
- `{op:'outline', color?, diagonal?, selout?}`: outline around opaque pixels; selout=true uses the darkest shade of each neighbor's ramp (color = fallback). With `palette_ramp … outline:'k'` ramps this gives tinted DOTT-style line work.
- `{op:'shift', dx, dy, wrap?}`
- `{op:'flip', h?, v?}`
- `{op:'clear', rect?:[x,y,w,h]}`
- `{op:'copy', from:[x,y,w,h], to:[x,y]}`

## Example: a rigged adventure character

```jsonc
// materials: hue-shifted ramps with a deep tinted outline shade each
{"tool": "palette_ramp", "sprite": "hero", "name": "skin", "base": "#f2cfae", "keys": "abcd", "outline": "k"}
// parts: block in with curves, mirror, shade, outline last
{"tool": "draw", "sprite": "hero", "part": "head", "ops": [
  {"op": "curve", "closed": true, "fill": true, "color": "c", "stroke": "none", "points": [[13,2],[22,9],[20,24],[13,28],[6,24],[4,9]]},
  {"op": "mirror"}, {"op": "shade", "highlight": 1}, {"op": "outline", "selout": true}]}
// rest pose: children hang from their parent's pivot
{"tool": "rig_set", "sprite": "hero", "poses": [
  {"slot": "leg_far", "part": "leg", "x": 23, "y": 58},
  {"slot": "leg_near", "part": "leg", "x": 18, "y": 58},
  {"part": "torso", "x": 20, "y": 61},
  {"part": "head", "parent": "torso", "x": 0, "y": -22},
  {"slot": "mouth", "part": "mouth_rest", "parent": "head", "x": 0, "y": -3},
  {"part": "lids", "parent": "head", "x": -8, "y": -16, "hide": true}]}
// animations only list what changes
{"tool": "anim_generate", "sprite": "hero", "preset": "idle", "blink": {"slot": "lids"}}
{"tool": "anim_talk", "sprite": "hero", "text": "Two point five centimetres. Precisely.", "head": "head"}
{"tool": "pose_set", "sprite": "hero", "anim": "idle", "frames": [3], "slot": "head", "rot": 4}
```
