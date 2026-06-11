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
  - ChunkBulk / MapChunkBulk
  - Disconnect
- Serverbound movement packets:
  - Player (`C03`, onGround only)
  - PlayerPosition (`C04`)
  - PlayerLook (`C05`)
  - PlayerPositionLook
- Movement packet selection follows the MCP 1.8.9 `EntityPlayerSP.onUpdateWalkingPlayer` decision:
  position delta squared `> 9.0E-4D` or `positionUpdateTicks >= 20`, plus separate yaw/pitch change detection.
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
- Block update and multi-block change application.
- Inventory/window packets.
- Chat and command packets.
- Packet-level integration tests against a real 1.8.9 server.
