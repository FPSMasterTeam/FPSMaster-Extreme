# Changelog

All notable changes to the recraft extension API.

## 0.3.0

**Breaking.** Expanded render API; the native `HostApi`/`geometry` ABI gained
fields (recompile native mods). Declare `api = "^0.3"`.

- **Mod textures**: `mc.loadTexture(path)`, `mc.registerTexture(rgba, w, h)`,
  `mc.createTexture(w, h)`, `mc.updateTexture(handle, rgba)`, `mc.freeTexture`.
  Native: `host.register_texture` / `load_texture` / `update_texture` /
  `free_texture`.
- **HUD primitives**: `hud.image(x, y, w, h, handle, { src })`,
  `hud.line(x0, y0, x1, y1, color, width)`,
  `hud.gradient(x, y, w, h, top, bottom)`. Native parity via `hud_image*` /
  `hud_line` / `hud_gradient` / `hud_text_ex`.
- **Textured native geometry**: `host.submit_geometry_textured(verts, idx, tex)`
  samples a registered texture instead of the entity atlas.
- **Full-screen post effect**: `mc.setPostEffect(wgsl)` / `mc.clearPostEffect()`
  (native: `host.set_post_effect` / `clear_post_effect`). `wgsl` defines
  `fn effect(uv: vec2<f32>, color: vec4<f32>) -> vec4<f32>`; `U.time` /
  `U.resolution` and `src_tex`/`src_samp` are in scope. Compile errors are
  logged and the previous effect kept.

## 0.2.0

**Breaking.** The JS global was renamed `recraft` → `mc` and restructured around
vanilla-style objects (`mc.player`, `mc.world`, `mc.connection`). Declare
`api = "^0.2"`. The native ABI bumps to 0.2.0; `recraft.d.ts` is now `mc.d.ts`.

- **Namespace**: `mc.player` / `mc.world` / `mc.connection`; `mc.log`/`warn`/`error`.
- **Events**: unified `mc.on(name, cb)` (`tick`, `frame`, `load`, `key`, `chat`,
  `blockChange`, `chunkLoad`, `chunkUnload`, `entitySpawn`, `entityRemove`,
  `playerHealth`); `mc.onPacket` (clientbound, droppable); **`mc.onServerbound`**
  (outbound pre-send hook, droppable); `mc.drawHud`.
- **Player reads**: `heldItem`, `inventory`, `selectedSlot`, `capabilities`,
  `effects`, `xp`, `container`; richer `mc.world.getBlock` (`isAir`, `luminance`,
  `opaque`, `shape`); `mc.world.entity(id)`; `mc.connection.connected`.
- **Player actions**: `placeBlock`, `dig`, `attack`, `interact`, `openInventory`,
  `closeContainer`, `clickSlot`, `selectSlot`, `swing`, `useItem`, and silent
  head rotation `setRotation(yaw, pitch, {silent})` / `clearRotation` (vanilla
  pre-event style).
- **Forge-style helpers**: `mc.keyBinding`, `mc.scheduler` (`after`/`every`/
  `clear`), `mc.config` (`load`/`get`/`set`/`save`, persisted to
  `<mod>/config.json`).
- **Native**: matching `HostApi` helpers for every new read/action; new
  `on_serverbound_packet` hook (default body, ABI-compatible for older mods).

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
