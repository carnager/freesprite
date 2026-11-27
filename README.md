# freesprite

A pixel-art sprite editor that AI agents operate over [MCP](https://modelcontextprotocol.io). It is built for point-and-click adventure characters (low-res, DOTT-style).

Characters are made of **parts** (head, torso, arm…) that are drawn once and hung together in a **rig** (mouth on the head, head on the torso). **Animations** then move, rotate, swap and flip those parts per frame, listing only what changes, so the agent never redraws whole frames. Agents write pixels as text rows in semantic palette keys, check their work with `review` and `lint`, and export to Godot.

**Spec:** [docs/SPEC.md](docs/SPEC.md) (concepts, file formats, all tools)

## Features

- Keyed palettes with hue-shifted ramps (`palette_ramp`: cool shadows, warm highlights, tinted outline shade), palette swaps and lighting variants
- Batched drawing: text-row patches, pixel-perfect lines and splines/Béziers, polygons, ellipses, flood fill, dithering, mirror symmetry, cel shading from a light direction, contour lines and sel-out outlines
- Rigs: a rest pose with parent links (mouth → head → torso), so animation frames only list what changes; RotSprite-style rotation, part swaps and mirroring
- Generators: lip-sync from a line of dialogue (`anim_talk`), breathing idle with blink and side-view walk cycles (`anim_generate`)
- Review sheet (1:1, silhouette, value study, zoom grid, frame strip) and a pixel-art linter that says how to fix what it finds
- Export: spritesheet, Aseprite-style JSON, Godot 4 `SpriteFrames`, portraits and GIFs (also animated inside a room)
- `pixelate` turns generated backgrounds into palette-limited pixel art

## Install

Arch Linux: a PKGBUILD is in `packaging/arch/`. Or build it yourself:

```sh
cargo build --release
claude mcp add freesprite -- "$PWD/target/release/freesprite"
```

Any MCP client works: the binary speaks MCP over stdio.

## License

MIT, see [LICENSE](LICENSE).
