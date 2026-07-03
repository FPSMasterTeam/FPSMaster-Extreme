# fpsmaster mods

Drop mods here. Each mod is a subdirectory with a `mod.toml` and an entry file.
fpsmaster loads everything in this folder at startup (in dependency order); press
**F10** in-game to hot-reload.

```
mods/
  mc.d.ts               ← JS type definitions (reference it for editor autocomplete)
  coords_hud/           ← example: player position / facing / vitals HUD
    mod.toml
    main.js
  chat_alert/           ← example: ping + banner on a chat keyword
  block_tint/           ← example: setBlockTint preset
  preset_demo/          ← example: toggle every render preset with keys
```

See the **[`sdk/`](../sdk/)** folder for the full SDK — start with
[`sdk/README.md`](../sdk/README.md) and [`sdk/REFERENCE.md`](../sdk/REFERENCE.md)
(JS + native API, `mod.toml`, capabilities, building native `cdylib` mods).

## JS mod skeleton

```js
/// <reference path="../mc.d.ts" />
mc.on("load", () => mc.log("hi"));
mc.drawHud(() => hud.text(2, 2, "y=" + mc.player.y.toFixed(1)));
```

## Native mod

A native mod is a `cdylib` built against `fpsmaster_ext_api` (see
`crates/fpsmaster_native_example`). Build it, copy the resulting
`libfpsmaster_*.{dylib,so,dll}` into a subdirectory here, and point `entry` in its
`mod.toml` at the filename. Native mods are per-OS × arch.
