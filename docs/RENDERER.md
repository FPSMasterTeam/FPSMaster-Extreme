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

`build_world_mesh` walks loaded chunks and emits one quad per visible block face. Internal faces between opaque cube blocks are culled.

Current mesh is global. The next step is per-chunk-section GPU buffers:

```text
ChunkSectionMeshKey { chunk_x, section_y, chunk_z }
  -> vertex/index buffers
  -> dirty rebuild when neighbor or local section changes
```

## Textures

The first atlas contains a small set of 1.8.9 block textures:

- stone
- grass top
- grass side
- dirt
- sand
- oak log
- oak leaves

The renderer loads them from a resource-pack-style root containing `assets/minecraft/...`, or from a zip/jar with the same internal layout. The default development path is `local_assets/minecraft-1.8.9`, produced by `scripts/setup_minecraft_1_8_9_assets.py`. That script extracts the original `assets/...` tree without changing its structure, so future resource-pack support can use the same layout. `--assets <resource-pack-root-or-zip>` / `RECRAFT_ASSET_PATH` are only overrides; normal local runs should not need them after setup.

Each tile falls back independently, so one missing texture no longer forces the whole atlas to debug colors. Mojang assets are downloaded only into `local_assets/`, which is ignored by Git, and should not be committed.

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
