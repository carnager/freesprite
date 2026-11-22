# freesprite

A pixel-art sprite editor that AI agents operate over [MCP](https://modelcontextprotocol.io). It is built for point-and-click adventure characters (low-res, DOTT-style).

Characters are made of **parts** (head, torso, arm…) that are drawn once. **Animations** then move, rotate and flip those parts per frame, so the agent never redraws whole frames. Agents write pixels as text rows in semantic palette keys, check their work with `review` and `lint`, and export to Godot.

**Spec:** [docs/SPEC.md](docs/SPEC.md) (concepts, file formats, all tools)

## Features

- Keyed palettes with color ramps, palette swaps and lighting variants
- Batched drawing: text-row patches, polygons, ellipses, flood fill, dithering, sel-out outlines
- Parts-based animation with clean RotSprite-style rotation, mirroring and per-frame pose tweaks
- Review sheet (1:1, silhouette, zoom grid, frame strip) and a pixel-art linter
- Export: spritesheet, Aseprite-style JSON, Godot 4 `SpriteFrames`, portraits and GIFs (also animated inside a room)
- `pixelate` turns generated backgrounds into palette-limited pixel art

## Build

```sh
cargo build --release
claude mcp add freesprite -- "$PWD/target/release/freesprite"
```

Any MCP client works: the binary speaks MCP over stdio.
