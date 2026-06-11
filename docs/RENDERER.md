# Renderer design

## Backend

The renderer uses `wgpu` and `winit` for the current Rust implementation. This targets modern systems first. The code is intentionally Minecraft-specific rather than a general game engine.

## Pipeline

Current pipeline:

```text
World chunks
  -> visible-face meshing
  -> Vertex { position, color, uv }
  -> block texture atlas
  -> wgpu render pass with depth buffer
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

The renderer tries to load them from the user's local 1.8.9 jar. Missing assets fall back to debug colors. Mojang assets are not committed.

## Lighting

Current lighting is directional face shading:

- top: brightest
- bottom: darkest
- sides: fixed brightness by axis

This is only a scaffold. Vanilla-compatible lighting requires decoding/storing block light and sky light nibble arrays from chunk packets, plus local relighting for block updates.

## Sky

The current sky pass renders a full-screen blue gradient before chunk rendering. A fuller 1.8.9 sky implementation should add:

- sky gradient
- sun/moon quads
- stars
- rain/fog interaction
- time-of-day color
