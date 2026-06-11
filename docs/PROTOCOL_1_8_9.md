# Minecraft Java 1.8.9 protocol scaffold

Protocol 47 is the first target.

## Implemented

- VarInt encode/decode with Minecraft-compatible negative values.
- Packet frame encode/decode.
- Compression frame support after Set Compression.
- Serverbound handshake.
- Serverbound login start.
- Clientbound login success / set compression / encryption request detection.
- Clientbound play packets used by first world-load path:
  - KeepAlive
  - JoinGame
  - PlayerPositionLook
  - ChunkData
  - MultiBlockChange (`S22`)
  - BlockChange (`S23`)
  - ChunkBulk / MapChunkBulk
  - Disconnect
- `ChunkData` ground-up packets with primary bitmask `0` are handled as vanilla chunk unloads, matching MCP `NetHandlerPlayClient.handleChunkData`.
- Block changes decode 1.8.9 legacy block-state IDs as `block_id << 4 | metadata`, matching `Block.BLOCK_STATE_IDS`.
- Serverbound movement packets:
  - Player (`C03`, onGround only)
  - PlayerPosition (`C04`)
  - PlayerLook (`C05`)
  - PlayerPositionLook (`C06`)
  - EntityAction (`C0B`) for sprint/sneak state changes
- Movement packet selection follows the MCP 1.8.9 `EntityPlayerSP.onUpdateWalkingPlayer` decision:
  position delta squared `> 9.0E-4D` or `positionUpdateTicks >= 20`, plus separate yaw/pitch change detection.
- Sprint/sneak action packets are emitted before the walking packet when the effective local state changes, matching the ordering in MCP `EntityPlayerSP.onUpdateWalkingPlayer`.
- 1.8 chunk section block array decoder for protocol 47 chunk data.
- Block light and sky light nibble-array decoding for chunk data.

## Current connection mode

Only offline-mode login is implemented. Online-mode `Encryption Request` is detected and returned as an error.

## Local test target

For the current client skeleton, use a local 1.8.9-compatible server with:

```properties
online-mode=false
server-port=25565
```

Then run:

```bash
cargo run -p recraft_app -- --connect 127.0.0.1:25565 --username ReCraft
```

## Missing before completion

- Online-mode Microsoft/Yggdrasil session flow and AES-CFB8 encryption.
- Full Play-state packet coverage.
- Entity spawn/move/metadata packets.
- Inventory/window packets.
- Chat and command packets.
- Packet-level integration tests against a real 1.8.9 server.
