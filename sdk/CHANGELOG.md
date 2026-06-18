# Changelog

All notable changes to the recraft extension API.

## 0.1.0

Initial release. Two-layer extension system (JS + native), sharing one API.

- **Events**: `onTick`, `onFrame`, `onLoad`, `onKey` (consumable), `onChat`,
  `onBlockChange`, `onChunkLoad`, `onChunkUnload`, `onEntitySpawn`,
  `onEntityRemove`, `onPlayerHealth`, `onPacket(type, cb)` (droppable), `drawHud`.
- **Commands**: `sendChat`, `sendPacket`, `log`/`warn`/`error`, `spawnParticle`,
  `playSound`.
- **Read-views**: `player`, `blockAt`, `entities`, `worldTime`, `dimension`.
- **HUD**: `hud.rect` / `text` / `itemIcon` / `blockItem`.
- **Preset render**: `setBlockTint`, `fullbright`, `chunkBorders`, `entityBox`,
  `nametagScale`, `particleDensity`. (The targeted-block outline is built in.)
- **Content (experimental)**: `registerBlock` (recraft-authoritative worlds only).
- **Native**: `recraft_ext_api` crate — `NativePlugin` trait, `HostApi`
  (`cmd`/`query`/`hud`/`geometry`), plus a render-geometry hook (`submit_geometry`).
- Per-mod error isolation; **F10** hot reload; capability declarations;
  dependency load order.
