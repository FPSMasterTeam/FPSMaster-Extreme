# ReCraft

Rust rewrite experiment for a Minecraft Java client. The first hard target is Java Edition 1.8.9 / protocol 47.

## Current status

Implemented scaffold:

- `recraft_core`: internal world/chunk/block/entity/player-physics state.
- `recraft_protocol`: multi-version protocol shell with 1.8.9 VarInt, framing, compression, login/play packet basics, and chunk-data decoder.
- `recraft_render`: `winit`/`wgpu` renderer for a 3D block world, chunk face meshing, depth buffer, basic face lighting, and block atlas sampling.
- `recraft_app`: desktop client loop with demo world mode and an offline-mode 1.8.9 connection skeleton.

This is not complete yet. Physics constants and collision ordering are structured for vanilla parity but are not fully verified against 1.8.9 MCP traces yet.

## Run

Demo world with local vanilla textures:

```bash
python3 scripts/setup_minecraft_1_8_9_assets.py
cargo run -p recraft_app -- --assets local_assets/minecraft-1.8.9-client.jar
```

Demo world without downloaded assets:

```bash
cargo run -p recraft_app
```

Offline-mode 1.8.9 server skeleton:

```bash
cargo run -p recraft_app -- --connect 127.0.0.1:25565 --username ReCraft --assets local_assets/minecraft-1.8.9-client.jar
```

The app currently supports keyboard movement:

- WASD: move
- Space: jump
- Left Shift: sneak
- Left Ctrl: sprint
- Mouse: turn/look
- Arrow keys: fallback turn/look
- Esc: release mouse cursor
- Click window: capture mouse cursor

## Verify

```bash
cargo test -p recraft_core -p recraft_protocol -p recraft_render
cargo check
```

## Assets

At startup the renderer tries to load vanilla block textures from `--assets <zip-or-jar>` / `RECRAFT_ASSET_ZIP` first, then from common user-owned 1.8.9 jar locations:

- Windows: `%APPDATA%/.minecraft/versions/1.8.9/1.8.9.jar`
- macOS: `~/Library/Application Support/minecraft/versions/1.8.9/1.8.9.jar`
- Linux: `~/.minecraft/versions/1.8.9/1.8.9.jar`

If no jar/resource pack is found, it uses a debug fallback atlas. Mojang assets are downloaded only into `local_assets/`, which is ignored by Git, and should not be committed to this repository.
