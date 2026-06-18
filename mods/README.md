# recraft mods

Drop mods here. Each mod is a subdirectory with a `mod.toml` and an entry file.
recraft loads everything in this folder at startup (in dependency order); press
**F10** in-game to hot-reload.

```
mods/
  recraft.d.ts          ← JS type definitions (reference it for editor autocomplete)
  coords_hud/           ← example: player position / facing / vitals HUD
    mod.toml
    main.js
  chat_alert/           ← example: ping + banner on a chat keyword
  block_tint/           ← example: setBlockTint preset
```

See **`docs/EXTENSION_SDK.md`** for the full guide (JS + native API, `mod.toml`,
capabilities, building native `cdylib` mods).

## JS mod skeleton

```js
/// <reference path="../recraft.d.ts" />
recraft.onLoad(() => recraft.log("hi"));
recraft.drawHud(() => hud.text(2, 2, "y=" + recraft.player().y.toFixed(1)));
```

## Native mod

A native mod is a `cdylib` built against `recraft_ext_api` (see
`crates/recraft_native_example`). Build it, copy the resulting
`librecraft_*.{dylib,so,dll}` into a subdirectory here, and point `entry` in its
`mod.toml` at the filename. Native mods are per-OS × arch.
