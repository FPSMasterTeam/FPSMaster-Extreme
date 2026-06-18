# recraft Extension SDK

Everything you need to write **recraft** mods. recraft has a two-layer extension
system; both layers share one API surface (events, commands, read-views, HUD,
preset render toggles) over different transports.

| Layer | Write a… | Distribute | When |
|---|---|---|---|
| **JS** (`tier = "js"`) | `.js` file | one file, cross-platform, hot-reloadable | behaviour, HUD, automation, config — most mods |
| **Native** (`tier = "native"`) | Rust `cdylib` | one binary per OS × arch | heavy native code, own threads, render hooks |

**API version: 0.1.0**

## What's in here

```
sdk/
  README.md            ← you are here
  REFERENCE.md         ← the full API reference (read this)
  CHANGELOG.md
  js/
    recraft.d.ts       ← TypeScript typings (editor autocomplete/type-checking)
    jsconfig.json      ← makes editors pick up the typings for this folder
    examples/          ← coords_hud, chat_alert, block_tint, preset_demo
  native/
    README.md          ← building a native cdylib mod
    recraft_ext_api/   ← the plugin ABI crate, bundled (no crates.io needed)
    example/           ← a complete, building native mod
    template/          ← a minimal starter native mod
```

## Quickstart (JS — 30 seconds)

1. Find your recraft `mods/` folder (next to where you launch recraft).
2. Create `mods/hello/mod.toml`:

   ```toml
   id = "hello"
   version = "1.0.0"
   tier = "js"
   api = "^0.1"
   entry = "main.js"
   capabilities = ["hud", "read_player"]
   ```

3. Create `mods/hello/main.js`:

   ```js
   /// <reference path="../recraft.d.ts" />
   recraft.onLoad(() => recraft.log("hello!"));
   recraft.drawHud(() => hud.text(2, 2, "y=" + recraft.player().y.toFixed(1)));
   ```

4. Copy `js/recraft.d.ts` from this SDK to `mods/recraft.d.ts` for autocomplete.
5. Launch recraft, enter a world, see your HUD. Press **F10** to hot-reload after edits.

Then read **[REFERENCE.md](REFERENCE.md)** for the full API, and crib from
`js/examples/`.

## Native mods

See **[native/README.md](native/README.md)**. Native mods compile against the
`recraft_ext_api` crate (bundled here in `native/recraft_ext_api/` — no crates.io
needed) and `abi_stable`.

## Capabilities

A mod declares `capabilities` in its `mod.toml` (`hud`, `read_world`,
`read_player`, `read_entities`, `inject_packet`, `chat`, `sound`, `particle`,
`render`, `input`). They're a trust signal (there is no sandbox).

## Compatibility

The host refuses a mod whose `api` requirement it doesn't satisfy. Native loads
are double-checked by `abi_stable`'s runtime layout hash, so a mod built against a
different API version is rejected rather than crashing.
