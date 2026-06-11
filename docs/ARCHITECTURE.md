# ReCraft architecture

## Goals

The client should eventually connect to vanilla-compatible Minecraft Java 1.8.9 servers, load the world, render chunks/blocks with vanilla resources, and simulate local player movement closely enough to avoid server correction.

The code is split so later versions can be added without letting protocol-specific packet layouts leak into rendering.

## Crates

```text
crates/
  recraft_core      internal game state: world, chunks, entities, player physics
  recraft_protocol  packet framing and version-specific protocol code
  recraft_render    wgpu renderer, chunk mesh generation, block atlas
  recraft_app       desktop app, input loop, network thread, game tick
```

## Data flow

```text
Server TCP packets
  -> recraft_protocol::v1_8_9 packet decoder
  -> recraft_app network events
  -> recraft_core World / EntityState
  -> recraft_render chunk mesh
  -> GPU draw calls
```

Renderer code does not know protocol versions. Protocol code does not know GPU resources.

## Multi-version protocol model

`ProtocolVersion` currently contains only `V1_8_9` / protocol 47. New versions should add version-specific modules under `recraft_protocol/src/vX_Y_Z` and translate packet payloads into stable app/core events.

Do not scatter version checks through rendering or world storage. Prefer this shape:

```text
version packet bytes -> version decoder -> stable event -> core world mutation
```

## World model

The first world model keeps classic pre-1.13 block IDs:

```rust
BlockState { id: u16, meta: u8 }
```

This matches 1.8.9 and keeps the first milestone small. A later 1.13+ path must introduce registry-backed canonical block states, with 1.8.9 IDs converted at the protocol boundary.

## Player physics

Chunk sections store 1.8-style block IDs/meta plus decoded block light and sky light values. Rendering samples this light data at visible faces.
Single-block and multi-block updates mutate already-loaded chunks and mark the affected chunk plus horizontal neighbors dirty. Server chunk unloads remove the chunk from `World` and use the same dirty path so boundary faces are rebuilt or removed.

`recraft_core::physics` implements a tick-based AABB path based on the vanilla `AxisAlignedBB.addCoord` / `calculate*Offset` collision flow, including the `0.6` player step-height branch from `Entity.moveEntity`. Player position and velocity are stored as `f64`, matching vanilla `posX/posY/posZ` and `motionX/motionY/motionZ`. The current tick order follows the vanilla 1.8.9 movement shape more closely: apply jump/input movement, move with collisions, then apply gravity and drag for the next tick. Input signs follow vanilla `MovementInput` / `moveFlying`: forward is positive, left strafe is positive, right strafe is negative, and yaw increases when turning right. This mirrors the shape of the vanilla 1.8.9 `Entity.moveEntity` / `EntityLivingBase.moveEntityWithHeading` path, but exact collision parity is not proven yet.

Rendering uses interpolation between previous and current 20Hz player positions so visual camera motion is smooth while the authoritative player state remains the real tick position. This mirrors the vanilla render path in `EntityRenderer.orientCamera(partialTicks)`; it is not used for physics or packet state.

Serverbound walking packets are selected in `recraft_app::network` using the vanilla `EntityPlayerSP.onUpdateWalkingPlayer` thresholds: send position when delta squared is greater than `9.0E-4D` or after 20 ticks, send look when yaw/pitch changed, otherwise send the onGround-only player packet. Sprint/sneak state changes are sent first as `C0B EntityAction` packets, matching the same MCP method ordering. The app waits for `JoinGame` before sending movement/action packets so the server has transitioned into play state and the local entity id is known.

Before this is considered complete, we need a trace suite comparing against MCP/black-box vanilla behavior for:

- ground friction and slipperiness
- sprint/sneak acceleration
- jump impulse and gravity order
- block-specific collision boxes for slabs/stairs/fences
- web, ladder, water, lava
- edge sneaking
- collision epsilon behavior
- server packet cadence

## Current limitations

- Online-mode encryption/authentication is not implemented.
- Inventory, block breaking/placing, entities other than local player, chat, and GUI are not complete.
- Chunk meshing is chunk-incremental, but still not section-incremental and has no frustum culling.
- Lighting is currently face-direction lighting, not vanilla block/sky light propagation.
- Sky is a clear color, not a full skybox/sun/moon/stars implementation.
