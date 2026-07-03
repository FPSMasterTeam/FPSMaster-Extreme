# Verification log

## 2026-06-11

Environment:

- macOS host
- Rust `cargo 1.95.0`, `rustc 1.95.0`
- GPU backend observed by wgpu: Metal / Apple M4
- Local server: Paper 1.8.8 build 445, protocol 47, `online-mode=false`

Commands run successfully:

```bash
cargo test -p fpsmaster_core -p fpsmaster_protocol -p fpsmaster_render
cargo check
```

Local Paper setup:

```bash
python3 scripts/setup_paper_1_8_test_server.py
local_server/paper-1.8-protocol47/run.sh
```

Server startup reached:

```text
Starting minecraft server version 1.8.8
Done (...)! For help, type "help" or "?"
```

Client demo runtime check:

```bash
RUST_LOG=warn cargo run -p fpsmaster_app
```

No wgpu validation error was observed during the short run.

Client offline-mode server connection check:

```bash
RUST_LOG=info cargo run -p fpsmaster_app -- --connect 127.0.0.1:25565 --username FPSMasterBot3
```

Observed client logs:

```text
logged in as FPSMasterBot3 (...)
applied chunk bulk: 10 chunks
...
applied chunk bulk: 9 chunks
```

Observed server logs:

```text
FPSMasterBot3[/127.0.0.1:...] logged in with entity id ... at ([world]..., ..., ...)
```

Known issue found and fixed during this verification:

- Sending serverbound `PlayerPositionLook` (`0x06`) to this PaperSpigot build caused `Bad packet id 6` disconnects.
- Current movement sender uses `PlayerPosition` (`0x04`) as a conservative baseline. Look packet support still needs deeper protocol/server compatibility investigation.

Not yet verified:

- Visual correctness of loaded world beyond runtime logs.
- Vanilla-exact player physics parity.
- Block light / sky light rendering.
- Online-mode authentication/encryption.

## 2026-06-11 asset/mouse/movement fix

Commands run successfully:

```bash
python3 scripts/setup_minecraft_1_8_9_assets.py
RUST_LOG=info cargo run -p fpsmaster_app -- --assets local_assets/minecraft-1.8.9-client.jar
cargo test -p fpsmaster_core -p fpsmaster_protocol -p fpsmaster_render
cargo check
```

Observed asset log:

```text
loaded 7 block atlas tiles from local_assets/minecraft-1.8.9-client.jar
loaded Minecraft 1.8.9 block atlas from local_assets/minecraft-1.8.9-client.jar
```

Movement direction is now covered by `movement_forward_matches_minecraft_yaw_convention`. Mouse motion is wired through `DeviceEvent::MouseMotion`; visual feel still needs manual tuning.

## 2026-06-11 chunk lighting path

Commands run successfully:

```bash
cargo test -p fpsmaster_core -p fpsmaster_protocol -p fpsmaster_render
cargo check
```

Coverage added:

- `chunk_light_round_trip`
- `world_light_handles_negative_chunks`
- `decodes_light_nibbles_in_xzy_order`
- `mesh_uses_world_light`

This verifies the data path from decoded 1.8 chunk light nibbles into internal chunk storage and mesh color generation. It does not yet prove vanilla-exact smooth lighting or ambient occlusion.

## 2026-06-11 player physics tick order

Commands run successfully:

```bash
cargo test -p fpsmaster_core -p fpsmaster_protocol -p fpsmaster_render
cargo check
```

Coverage added/updated:

- `jump_moves_before_gravity_drag_for_tick` verifies the jump tick first moves by `0.42` and then stores the post-gravity/post-drag velocity for the next tick.
- Existing landing and movement-direction tests still pass.

This moves the implementation closer to the 1.8.9 movement order, but it is not yet a full vanilla parity proof.

## 2026-06-11 left/right controls and render interpolation

Commands run successfully:

```bash
cargo test -p fpsmaster_core -p fpsmaster_protocol -p fpsmaster_render
cargo check
```

Changes verified by tests:

- `movement_forward_matches_minecraft_yaw_convention` now covers vanilla strafe signs: left is positive X at yaw 0, right is negative X.
- `player_does_not_auto_climb_full_block` verifies a full block is not auto-climbed without jump input.
- `player_can_jump_onto_full_block` verifies holding jump while moving forward can get onto a one-block step.

Runtime behavior changed:

- Mouse X and arrow left/right yaw signs were inverted to match Minecraft-style yaw.
- Render camera position is interpolated between 20Hz physics ticks. Packet sending remains tied to the tick loop.

## 2026-06-11 MCP movement/packet audit

MCP reference availability:

- Local reference repo: `references/MCP-919`
- Current commit checked by Git object access: `1717f75`
- Used MCP files:
  - `EntityRenderer.java`
  - `Entity.java`
  - `EntityLivingBase.java`
  - `EntityPlayerSP.java`
  - `MovementInputFromOptions.java`
  - `C03PacketPlayer.java`

Commands run successfully:

```bash
cargo test -p fpsmaster_core -p fpsmaster_protocol -p fpsmaster_render
cargo test -p fpsmaster_app
cargo check
```

Coverage added/updated:

- Player position and velocity storage now uses `f64` to match vanilla `posX/posY/posZ` and `motionX/motionY/motionZ`.
- Render camera interpolation remains render-only: previous/current tick positions are interpolated for the camera, while authoritative player state and movement snapshots keep the real tick position.
- Serverbound walking packets now follow the MCP `EntityPlayerSP.onUpdateWalkingPlayer` decision across `C03`/`C04`/`C05`/`C06`, including the `9.0E-4D` movement threshold and 20 tick forced position update.
- Physics tick moved closer to MCP by applying input `0.98F`, sneak input scaling before `moveFlying`, sprint speed/air multiplier, sprint jump impulse, and vanilla gravity/vertical drag constants.

Not yet verified:

- Full vanilla collision parity; current collision is still a simplified AABB clipper and does not yet implement every `Entity.moveEntity` branch/epsilon.

## 2026-06-11 local server packet smoke test and collision pass

Local server:

```bash
local_server/paper-1.8-protocol47/run.sh
```

Client smoke command:

```bash
RUST_LOG=info cargo run -p fpsmaster_app -- \
  --connect 127.0.0.1:25565 \
  --username FPSMasterVerify \
  --assets local_assets/minecraft-1.8.9-client.jar
```

Observed:

```text
logged in as FPSMasterVerify (...)
applied chunk bulk: 10 chunks
...
applied chunk bulk: 9 chunks
```

Server observed successful login and only logged `Connection reset` after the client process was manually terminated. The previous immediate `Bad packet id 6` failure was not reproduced during this idle movement-packet smoke test. This does not prove every C06 path yet because no scripted look+position movement was injected.

Commands run successfully:

```bash
cargo test -p fpsmaster_core
```

Coverage added/updated:

- Collision now uses a closer MCP-shaped `AxisAlignedBB.addCoord` and `calculateX/Y/ZOffset` path.
- Player `step_height` defaults to vanilla `0.6`.
- The step-height branch from `Entity.moveEntity` is implemented while the existing full-block no-auto-climb test still passes.
- `head_collision_stops_upward_motion_before_gravity` verifies upward head collision zeroes vertical motion before gravity is applied.

## 2026-06-11 scripted C06 movement smoke test

Client smoke command:

```bash
RUST_LOG=info,fpsmaster_app::network=debug cargo run -p fpsmaster_app -- \
  --connect 127.0.0.1:25565 \
  --username FPSMasterC06 \
  --assets local_assets/minecraft-1.8.9-client.jar \
  --scripted-smoke 20
```

The `--scripted-smoke` mode is a verification-only app entry point. It drives forward sprint movement, yaw changes, short jump pulses, and pitch changes, then exits automatically.

Observed client logs:

```text
logged in as FPSMasterC06 (...)
applied chunk bulk: 10 chunks
...
sending C06 PlayerPositionLook
...
scripted smoke complete
```

Observed server logs:

```text
FPSMasterC06[/127.0.0.1:...] logged in with entity id ... at ([world]..., ..., ...)
FPSMasterC06 lost connection: Disconnected
FPSMasterC06 left the game.
```

No `Bad packet id 6` or protocol exception was observed during the scripted C06 path.

## 2026-06-11 extracted default assets

Asset setup command:

```bash
python3 scripts/setup_minecraft_1_8_9_assets.py
```

Observed:

```text
Asset jar ready: .../local_assets/minecraft-1.8.9-client.jar
Extracted 3085 asset files to: .../local_assets/minecraft-1.8.9
Run with: cargo run -p fpsmaster_app
```

The extracted directory preserves the vanilla/resource-pack root layout:

```text
local_assets/minecraft-1.8.9/assets/minecraft/textures/blocks/...
```

Demo smoke command without `--assets`:

```bash
RUST_LOG=info cargo run -p fpsmaster_app -- --scripted-smoke 2
```

Observed:

```text
loaded 7 block atlas tiles from local_assets/minecraft-1.8.9
loaded Minecraft 1.8.9 block atlas from asset directory local_assets/minecraft-1.8.9
scripted smoke complete
```

Server smoke command without `--assets`:

```bash
RUST_LOG=info cargo run -p fpsmaster_app -- \
  --connect 127.0.0.1:25565 \
  --username FPSMasterAssets \
  --scripted-smoke 8
```

Observed client logs:

```text
loaded 7 block atlas tiles from local_assets/minecraft-1.8.9
loaded Minecraft 1.8.9 block atlas from asset directory local_assets/minecraft-1.8.9
logged in as FPSMasterAssets (...)
applied chunk bulk: 10 chunks
...
scripted smoke complete
```

Observed server logs:

```text
FPSMasterAssets[/127.0.0.1:...] logged in with entity id ... at ([world]..., ..., ...)
FPSMasterAssets lost connection: Disconnected
FPSMasterAssets left the game.
```

## 2026-06-11 expanded block atlas coverage

Demo smoke command without `--assets`:

```bash
RUST_LOG=info cargo run -p fpsmaster_app -- --scripted-smoke 2
```

Observed:

```text
loaded 129 block atlas tiles from local_assets/minecraft-1.8.9
loaded Minecraft 1.8.9 block atlas from asset directory local_assets/minecraft-1.8.9
scripted smoke complete
```

Server smoke command without `--assets`:

```bash
RUST_LOG=info cargo run -p fpsmaster_app -- \
  --connect 127.0.0.1:25565 \
  --username FPSMasterAtlas \
  --scripted-smoke 8
```

Observed client logs:

```text
loaded 129 block atlas tiles from local_assets/minecraft-1.8.9
loaded Minecraft 1.8.9 block atlas from asset directory local_assets/minecraft-1.8.9
logged in as FPSMasterAtlas (...)
applied chunk bulk: 10 chunks
...
scripted smoke complete
```

Observed server logs:

```text
FPSMasterAtlas[/127.0.0.1:...] logged in with entity id ... at ([world]..., ..., ...)
FPSMasterAtlas lost connection: Disconnected
FPSMasterAtlas left the game.
```

Coverage expanded from the initial 7-tile atlas to common 1.8.9 block-id/meta textures. This is still not a full blockstate/model/resource-pack implementation.

## 2026-06-11 incremental chunk mesh upload

Runtime renderer change:

- GPU meshes are now stored per `ChunkPos`.
- Full `upload_world` is still used for the initial demo/full load path.
- Server chunk packets mark the changed chunk and four horizontal neighbor chunks dirty.
- Runtime server chunk loading calls `upload_dirty_chunks` instead of rebuilding one global world mesh after every chunk packet.

Server smoke command without `--assets`:

```bash
RUST_LOG=info cargo run -p fpsmaster_app -- \
  --connect 127.0.0.1:25565 \
  --username FPSMasterMesh \
  --scripted-smoke 8
```

Observed client logs:

```text
loaded 129 block atlas tiles from local_assets/minecraft-1.8.9
loaded Minecraft 1.8.9 block atlas from asset directory local_assets/minecraft-1.8.9
logged in as FPSMasterMesh (...)
applied chunk bulk: 10 chunks
...
scripted smoke complete
```

Observed server logs:

```text
FPSMasterMesh[/127.0.0.1:...] logged in with entity id ... at ([world]..., ..., ...)
FPSMasterMesh lost connection: Disconnected
FPSMasterMesh left the game.
```

No wgpu validation or protocol error was observed during the scripted smoke run. This is a runtime smoke check, not a visual-performance benchmark.

## 2026-06-11 default extracted assets and chunk unload follow-up

Commands run successfully:

```bash
python3 scripts/setup_minecraft_1_8_9_assets.py
cargo fmt --all
cargo check
cargo test -p fpsmaster_core -p fpsmaster_protocol -p fpsmaster_render
cargo test -p fpsmaster_app
RUST_LOG=info cargo run -p fpsmaster_app -- --scripted-smoke 2
```

Observed default asset log without `--assets`:

```text
loaded 129 block atlas tiles from local_assets/minecraft-1.8.9
loaded Minecraft 1.8.9 block atlas from asset directory local_assets/minecraft-1.8.9
scripted smoke complete
```

Local Paper server smoke command without `--assets`:

```bash
local_server/paper-1.8-protocol47/run.sh
RUST_LOG=info cargo run -p fpsmaster_app -- \
  --connect 127.0.0.1:25565 \
  --username FPSMasterAssets2 \
  --scripted-smoke 8
```

Observed client logs:

```text
loaded 129 block atlas tiles from local_assets/minecraft-1.8.9
loaded Minecraft 1.8.9 block atlas from asset directory local_assets/minecraft-1.8.9
logged in as FPSMasterAssets2 (...)
applied chunk bulk: 10 chunks
...
scripted smoke complete
```

Observed server logs:

```text
FPSMasterAssets2[/127.0.0.1:...] logged in with entity id ... at ([world]144.5, 67.0, 71.5)
FPSMasterAssets2 lost connection: Disconnected
FPSMasterAssets2 left the game.
```

MCP source checked for chunk unload behavior:

- `NetHandlerPlayClient.handleChunkData(S21PacketChunkData)` calls `doPreChunk(x, z, false)` and returns when `func_149274_i()` is true and `getExtractedSize() == 0`.
- `S21PacketChunkData.getExtractedSize()` returns `extractedData.dataSize`, the primary section bitmask.

Implementation now treats `ChunkData` with `ground_up == true` and primary bitmask `0` as an unload, removes the chunk from the internal world, and marks the chunk plus horizontal neighbors dirty so the renderer removes/rebuilds affected chunk meshes. No extra tests were added for this path; this was kept as a small MCP-backed runtime/protocol fix.


## 2026-06-11 block update packet pass

MCP source checked:

- `S23PacketBlockChange` reads `BlockPos` and `Block.BLOCK_STATE_IDS` VarInt.
- `S22PacketMultiBlockChange` reads chunk X/Z, VarInt count, then crammed local positions and `Block.BLOCK_STATE_IDS` VarInts.
- `NetHandlerPlayClient.handleBlockChange` / `handleMultiBlockChange` call `invalidateRegionAndSetBlock` for each update.
- `Block.BLOCK_STATE_IDS` is populated as `block_id << 4 | metadata` in MCP 1.8.9 registration.

Commands run successfully:

```bash
cargo test -p fpsmaster_protocol
cargo check
```

Implementation notes:

- Clientbound play `0x22` / `0x23` now decode into stable block-change events.
- The app applies those changes to already-loaded chunks and marks affected chunk meshes dirty.
- Updates for chunks not present in the client world are ignored instead of creating phantom chunks from isolated block updates.

This verifies packet decoding and build integrity. It does not yet verify a live server action that sends `S22/S23`; that should be covered by a later manual smoke scenario such as placing/breaking blocks from another client.


## 2026-06-11 sprint/sneak entity action packets

MCP source checked:

- `C0BPacketEntityAction` writes VarInt entity id, enum ordinal action, and VarInt aux data.
- `EntityPlayerSP.onUpdateWalkingPlayer` sends sprint state changes before sneak state changes, and sends both before the normal `C03/C04/C05/C06` walking packet.

Commands run successfully:

```bash
cargo test -p fpsmaster_protocol
cargo test -p fpsmaster_app
cargo test -p fpsmaster_core -p fpsmaster_protocol -p fpsmaster_render
cargo test -p fpsmaster_app
cargo check
```

Local server smoke initially exposed a real ordering bug: scripted input could send movement/action packets before the app had applied `JoinGame`. Paper disconnected that first run with `Bad packet id 11` after `C0B` was sent too early. The app now waits for `JoinGame` before sending movement/action packets.

Successful local Paper smoke after the fix:

```bash
local_server/paper-1.8-protocol47/run.sh
RUST_LOG=info,fpsmaster_app::network=debug cargo run -p fpsmaster_app -- \
  --connect 127.0.0.1:25565 \
  --username FPSMasterC0B2 \
  --scripted-smoke 6
```

Observed client logs:

```text
loaded 129 block atlas tiles from local_assets/minecraft-1.8.9
logged in as FPSMasterC0B2 (...)
sending C0B EntityAction START_SPRINTING
sending C06 PlayerPositionLook
...
sending C0B EntityAction STOP_SPRINTING
scripted smoke complete
```

Observed server logs:

```text
FPSMasterC0B2[/127.0.0.1:...] logged in with entity id ... at ([world]145.5, 67.0, 72.5)
FPSMasterC0B2 lost connection: Disconnected
FPSMasterC0B2 left the game.
```

No `Bad packet id 11` or other server protocol exception occurred in the successful run.
