# freesprite specification

## Concepts

- **Sprite**: a fixed canvas (`width` × `height`) with an **origin** (the foot point used to place it in the game), a **palette**, **parts** and **animations**.
- **Palette**: entries `{key, color, name?, ramp?}`. `key` is one character used in text rows (`.` `_` and space are reserved). `ramp` groups shades for sel-out outlines. Changing a key's color recolors every pixel that uses it. **Palette variants** hold the same keys with different colors (lighting: day, dusk, tavern…).
- **Part**: a small RGBA pixel image with a **pivot** (part pixel coordinates; `.5` = pixel center). Rotation and placement happen around the pivot.
- **Animation**: `loop` flag plus frames. A **frame** has `duration_ms` and a list of **poses**, drawn back to front. A pose places a part's pivot at canvas `x,y` with optional `rot` (degrees, clockwise), `flip_h`, `flip_v`.
- Rotations that are not multiples of 90° use a RotSprite-style pipeline (scale2x ×3, rotate, nearest sample), so colors stay inside the palette.

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
<dir>/sprite.json     name, width, height, origin, palette, palette_variants, portrait, parts (pivots), animations
<dir>/parts/<part>.png
```

Every mutating tool call autosaves. `undo`/`redo` keep up to 200 steps per session.

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
| `ops` | object[] | yes | see draw ops below |
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
| `frame` | object | yes |  |
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

One control sheet for self-review: 1:1 size, black silhouette (readability check), zoom with pixel grid + coordinate labels, and (for animations) all frames in a strip. grid_text=true also returns the composed frame as text rows.

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

Tweak one part's pose across frames (all frames if 'frames' omitted): absolute x/y/rot/flips, relative dx/dy, swap the part (replace_with, e.g. mouth_closed -> mouth_open), reorder (z = draw index), or remove. Adds the pose if missing.

| Param | Type | Required | Notes |
|---|---|---|---|
| `anim` | string | yes |  |
| `dx` | integer |  |  |
| `dy` | integer |  |  |
| `flip_h` | boolean |  |  |
| `flip_v` | boolean |  |  |
| `frames` | integer[] |  |  |
| `part` | string | yes |  |
| `remove` | boolean |  |  |
| `replace_with` | string |  |  |
| `rot` | number |  |  |
| `sprite` | string | yes | sprite name (or its directory path) |
| `x` | integer |  |  |
| `y` | integer |  |  |
| `z` | integer |  |  |

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

Passed as `ops` to `draw`, applied in order. Coordinates are part-local.

- `{op:'rows', x?, y?, rows:[str], legend?:{char:color}, erase_dots?}`: paint/patch a pixel block; chars = palette keys, legend chars override, '.'/' ' = unchanged (erase if erase_dots), '_' = erase
- `{op:'pixels', points:[[x,y]...], color}`
- `{op:'line', points:[[x,y],[x,y],...], color}`: polyline
- `{op:'rect', x,y,w,h, color, fill?, pattern?, color2?}`
- `{op:'ellipse', x,y,w,h, color, fill?, pattern?, color2?}`: bounding box
- `{op:'polygon', points:[[x,y]...], color, fill? (default true), pattern?, color2?}`
- `{op:'fill', x,y, color, pattern?, color2?}`: flood fill
- `pattern = 'checker' | 'bayer:N' (N 0-16 density) | 'hlines' | 'vlines': dither; 'on' pixels get color, others color2 (or untouched)`
- `{op:'replace', from, to}`: recolor
- `{op:'outline', color?, diagonal?, selout?}`: outline around opaque pixels; selout=true uses the darkest shade of each neighbor's ramp (color = fallback)
- `{op:'shift', dx, dy, wrap?}`
- `{op:'flip', h?, v?}`
- `{op:'clear', rect?:[x,y,w,h]}`
- `{op:'copy', from:[x,y,w,h], to:[x,y]}`
