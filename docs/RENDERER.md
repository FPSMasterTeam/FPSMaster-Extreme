# Renderer design

## Backend

The renderer uses `wgpu` and `winit` for the current Rust implementation. This targets modern systems first. The code is intentionally Minecraft-specific rather than a general game engine.

## Pipeline

Current pipeline:

```text
World chunks + stored block/sky light
  -> visible-face meshing
  -> Vertex { position, color, uv }
  -> block texture atlas
  -> sky gradient pass
  -> wgpu chunk render pass with depth buffer
```

## Chunk meshing

`build_chunk_mesh` walks one loaded chunk and emits one quad per visible block face. Internal faces between opaque cube blocks are culled, including across chunk boundaries when the neighbor chunk is loaded.

The renderer stores one GPU vertex/index buffer pair per `ChunkPos`. When a chunk packet changes world data, the app marks that chunk and its four horizontal neighbors dirty, then only those GPU meshes are rebuilt:

```text
ChunkPos { chunk_x, chunk_z }
  -> vertex/index buffers
  -> dirty rebuild when local or horizontal-neighbor chunk changes
```

`build_world_mesh` still exists for simple whole-world paths and tests, but runtime server chunk loading uses the incremental chunk upload path.

## Textures

The first atlas contains common 1.8.9 terrain/building block textures: stone variants, dirt/grass/podzol, sand/red sand, ores, planks/logs/leaves, sandstone/red sandstone, wool, mineral blocks, bricks, stone bricks, pumpkins/melons, nether/end blocks, quartz, clay/stained clay, snow/ice, and related blocks.

The renderer loads them from a resource-pack-style root containing `assets/minecraft/...`, or from a zip/jar with the same internal layout. The default development path is `local_assets/minecraft-1.8.9`, produced by `scripts/setup_minecraft_1_8_9_assets.py`. That script extracts the original `assets/...` tree without changing its structure, so future resource-pack support can use the same layout. `--assets <resource-pack-root-or-zip>` / `RECRAFT_ASSET_PATH` are only overrides; normal local runs should not need them after setup.

Each tile falls back independently, so one missing texture no longer forces the whole atlas to debug colors. This is still an explicit block-id/meta mapping, not yet a full vanilla blockstate/model loader. Mojang assets are downloaded only into `local_assets/`, which is ignored by Git, and should not be committed.

## Lighting

Current lighting uses decoded 1.8 chunk block-light and sky-light nibble arrays. Mesh generation samples the light level outside each visible face and combines it with directional face shading:

- top: brightest
- bottom: darkest
- sides: fixed brightness by axis
- block/sky light: max(block_light, sky_light), mapped through a simple 0..15 curve

This is still a scaffold. Vanilla-compatible lighting still requires the exact vanilla light table, ambient occlusion/smooth lighting, and local relighting for block updates.

## Sky

The current sky pass renders a full-screen blue gradient before chunk rendering. A fuller 1.8.9 sky implementation should add:

- sky gradient
- sun/moon quads
- stars
- rain/fog interaction
- time-of-day color
