# FPSMaster Extreme

**FPSMaster Extreme** is part of the FPSMaster ecosystem — a Rust rewrite experiment for a Minecraft Java client. The first hard target is Java Edition 1.8.9 / protocol 47.

> The crates, extension API, and mod SDK are all namespaced `fpsmaster_*`.

## Current status

Implemented scaffold:

- `fpsmaster_core`: internal world/chunk/block/entity/player-physics state.
- `fpsmaster_protocol`: multi-version protocol shell with 1.8.9 VarInt, framing, compression, login/play packet basics, and chunk-data decoder.
- `fpsmaster_render`: `winit`/`wgpu` renderer for a 3D block world, chunk face meshing, depth buffer, basic face lighting, and block atlas sampling.
- `fpsmaster_app`: desktop client loop with demo world mode and an offline-mode 1.8.9 connection skeleton.
- `fpsmaster_ext` / `fpsmaster_ext_api`: the extension (mod) system — a JavaScript layer for behaviour/HUD/automation and a native (`cdylib`) layer for deep rendering and content. See [Extensions](#extensions).

This is not complete yet. Physics constants and collision ordering are structured for vanilla parity but are not fully verified against 1.8.9 MCP traces yet.

## Run

Demo world with local vanilla textures:

```bash
python3 scripts/setup_minecraft_1_8_9_assets.py
cargo run -p fpsmaster_app
```

Demo world without downloaded assets:

```bash
cargo run -p fpsmaster_app
```

Offline-mode 1.8.9 server skeleton:

```bash
cargo run -p fpsmaster_app -- --connect 127.0.0.1:25565 --username FPSMaster
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
cargo test -p fpsmaster_core -p fpsmaster_protocol -p fpsmaster_render
cargo check
```

## Extensions

fpsmaster has a two-layer mod system. Mods live in `mods/`, each with a `mod.toml`
manifest, and are managed in-app from the **Mods…** button on the title/pause
screen (list / toggle / reload / open folder).

- **JavaScript layer** — for behaviour, HUD, automation, and preset render
  tweaks. Mods use the `mc.*` API (`mc.player` / `mc.world` / `mc.connection`,
  event hooks, HUD drawing, keybindings, config). Hot-reloadable and per-mod
  error-isolated.
- **Native layer** — `cdylib` plugins built against the `fpsmaster_ext_api` crate
  (stable abi_stable ABI) for deep rendering hooks and content registration.

The SDK (typings, native template + example, API reference) is in [`sdk/`](sdk/);
runnable example mods are in [`mods/`](mods/). Full guide:
[`docs/EXTENSION_SDK.md`](docs/EXTENSION_SDK.md).

## Assets

For local development, extract the vanilla 1.8.9 assets once:

```bash
python3 scripts/setup_minecraft_1_8_9_assets.py
```

This creates `local_assets/minecraft-1.8.9/assets/minecraft/...`, preserving the original resource-pack directory structure. After that, the default app commands load textures without extra flags:

```bash
cargo run -p fpsmaster_app
cargo run -p fpsmaster_app -- --connect 127.0.0.1:25565 --username FPSMaster
```

At startup the renderer tries to load vanilla block textures from `FPSMASTER_ASSET_PATH` / `--assets <resource-pack-root-or-zip>`, then the default extracted directory, then common user-owned 1.8.9 jar locations:

- Windows: `%APPDATA%/.minecraft/versions/1.8.9/1.8.9.jar`
- macOS: `~/Library/Application Support/minecraft/versions/1.8.9/1.8.9.jar`
- Linux: `~/.minecraft/versions/1.8.9/1.8.9.jar`

If no extracted assets, jar, or resource pack is found, it uses a debug fallback atlas. Mojang assets are downloaded only into `local_assets/`, which is ignored by Git, and should not be committed to this repository.

## 许可
MIT License — Copyright (c) 2026 FPSMaster Team，见 [LICENSE](LICENSE)
