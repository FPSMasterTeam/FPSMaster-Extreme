# recraft Extension SDK

recraft has a two-layer extension system:

| Layer | For | Distribution | Power |
|---|---|---|---|
| **JS** (`tier = "js"`) | behaviour / automation / HUD / config | one `.js` file, cross-platform, hot-reloadable | events, commands, read-views, HUD drawing, a closed set of preset render toggles |
| **Native** (`tier = "native"`) | depth / performance / arbitrary native code | a `cdylib` per OS × arch | everything JS can do **plus** unrestricted native code (own threads/state, heavy CPU work, and — roadmap — raw render hooks) |

Both layers are bindings over one internal **event bus + command queue** (`recraft_ext`), so they share the exact same event/command semantics; the JS and native APIs are the same surface with different transports.

> There is **no sandbox**. A JS mod's errors are isolated (a thrown handler is caught and logged, a broken mod is disabled), but a native mod runs in-process with full access and can crash the host. Capabilities are a *declare + confirm* trust signal, not an enforcement boundary.

---

## 1. Quickstart (JS)

Mods live under `mods/<id>/`, each with a `mod.toml` and an entry file. Create `mods/hello/mod.toml`:

```toml
id = "hello"
version = "1.0.0"
tier = "js"
api = "^0.1"
entry = "main.js"
capabilities = ["hud", "read_player"]
name = "Hello"
description = "Minimal example."
```

and `mods/hello/main.js`:

```js
/// <reference path="../recraft.d.ts" />

recraft.onLoad(() => recraft.log("hello loaded"));

recraft.drawHud((ctx) => {
  const p = recraft.player();
  hud.text(2, 2, `y=${p.y.toFixed(1)}`, { color: 0xffff55ff });
});
```

Launch recraft from the directory that contains `mods/`. Press **F10** to hot-reload all mods after editing. Copy this SDK's `js/recraft.d.ts` next to your mods for editor autocomplete/type-checking.

Four worked examples are in `js/examples/`: `coords_hud`, `chat_alert`, `block_tint`, `preset_demo`.

---

## 2. `mod.toml`

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | Unique mod id. |
| `version` | yes | Mod version. |
| `tier` | yes | `"js"` or `"native"`. |
| `api` | yes | Semver requirement against the host API (e.g. `"^0.1"`). The host refuses incompatible mods. |
| `entry` | yes | `main.js` for JS; the dylib filename (`librecraft_*.dylib` / `.so` / `.dll`) for native. |
| `depends` | no | List of mod ids this mod loads after (topologically ordered; cycles are an error). |
| `capabilities` | no | Declared capabilities (see below). |
| `name`, `description` | no | Display metadata. |

### Capabilities

`hud`, `read_world`, `read_player`, `read_entities`, `inject_packet`, `chat`, `sound`, `particle`, `render`, `input`.

They are logged at load and surfaced for trust decisions; `inject_packet` is flagged sensitive. They are advisory in this version (no sandbox).

---

## 3. JS API

Everything below is injected as the globals `recraft`, `hud`, and a `console` shim. See `js/recraft.d.ts` for exact types.

### Events

```js
recraft.onTick(() => {});            // 20 Hz
recraft.onFrame(() => {});           // per frame
recraft.onLoad(() => {});            // once, after load
recraft.onKey((e) => e.pressed);     // e:{key,pressed}; return true to CONSUME the key
recraft.onChat((e) => {});           // e:{text,position,json}
recraft.onBlockChange((e) => {});    // e:{x,y,z,id,meta}
recraft.onChunkLoad((e) => {});      // e:{x,z}
recraft.onChunkUnload((e) => {});    // e:{x,z}
recraft.onEntitySpawn((e) => {});    // e:{id,kind,typeId,x,y,z}
recraft.onEntityRemove((e) => {});   // e:{id}
recraft.onPlayerHealth((e) => {});   // e:{health,food}
recraft.onPacket("BlockChange", (p) => {});  // raw clientbound packet; return false to DROP
recraft.drawHud((ctx) => {});        // ctx:{width,height,scale,screenOpen}; draw with `hud.*`
```

`onPacket` types are the stable `PacketType` names (not vanilla wire ids). High-frequency packets are opt-in (you only pay for what you subscribe to). Coverage note: chunk packets and net-thread control packets (KeepAlive/ConfirmTransaction) are **not** delivered to `onPacket` — use `onChunkLoad`/`onChunkUnload` for chunks.

### Commands

```js
recraft.sendChat("hello");                       // or a /command
recraft.sendPacket({ type: "swingArm" });        // needs `inject_packet`
recraft.log("x", 1); recraft.warn(...); recraft.error(...);
recraft.spawnParticle(26 /*flame*/, x, y, z, { count: 5, speed: 0.02 });
recraft.playSound("random.orb", x, y, z, { pitch: 1.5 });
```

`sendPacket` accepts a whitelisted play-state packet: `chat`, `playerPosition`, `playerLook`, `heldItemChange`, `swingArm`, `playerDigging`. Handshake/login packets are unreachable.

### Read-views

```js
const p = recraft.player();      // {x,y,z,yaw,pitch,vx,vy,vz,onGround,health,food,sneaking,sprinting}
const b = recraft.blockAt(x,y,z); // {id,meta}
const list = recraft.entities();  // [{id,kind,typeId,x,y,z,yaw,pitch,onGround,name,health}, ...]  (local player excluded)
recraft.worldTime();              // ticks 0..24000
recraft.dimension();              // 0 overworld, -1 nether (best-effort)
```

### HUD drawing

Inside a `drawHud` callback. Coordinates are GUI pixels (the host scales them). Colors accept a packed `0xRRGGBBAA` int, `[r,g,b(,a)]`, or `"#rrggbb"`.

```js
hud.rect(x, y, w, h, color);
hud.text(x, y, "label", { color, scale, shadow });
hud.itemIcon(x, y, itemId, { size });
hud.blockItem(x, y, blockId, meta, { size });
```

### Preset render modifications

A **closed, host-implemented** set (the JS layer's "controlled rendering" — adding one means changing host code; arbitrary rendering is native-only).

```js
recraft.setBlockTint(1, [120, 200, 255]);   // statically tint a block id (read by the mesher)
recraft.fullbright(true);
recraft.chunkBorders(true);
recraft.entityBox("mobs", "#ffffff", true);   // thick hitbox wireframe
recraft.nametagScale(1.5);
recraft.particleDensity(0.5);
```

All presets are wired to the renderer: `setBlockTint` (chunk mesher),
`fullbright` (forces the world lightmap to full — caves/night fully visible,
no washout), `nametagScale` (nametag world size), `particleDensity` (spawn-count
multiplier), `chunkBorders` (the player chunk's grid), and `entityBox` (a thick
white hitbox wireframe around entities). The targeted-block outline is built into
recraft (always drawn, like vanilla), not a preset.

### Content (experimental)

`recraft.registerBlock` (and `HostApi::register_block` for native mods) registers
a new full-cube block beyond the vanilla `0..=197` id range. The block gets full
render/collision/light properties from a runtime registry overlay (vanilla ids
keep their fast path, so there is no cost to existing blocks):

```js
recraft.onLoad(() => recraft.registerBlock(300, {
  texture: "stone",          // reuse a vanilla atlas texture for now
  opaque: true, alpha: 1.0,
  luminance: 7,              // 0..15 light emitted
  tint: [0.4, 0.8, 1.0],     // optional [r,g,b] 0..1
}));
```

**Caveat:** this only manifests in a *recraft-authoritative* world. recraft is a
client to a vanilla/Paper 1.8 server, which never sends mod block ids, so there
is currently no world where a registered mod block can be placed. Per-mod
*texture* registration (its own atlas entry, vs. reusing a vanilla name) is also
not implemented yet. The id-space + property registry — the core of this
milestone — is in place; placement/rendering of a new block awaits a
recraft-authoritative world.

### Error isolation & hot reload

Each mod runs in its own QuickJS context. A handler that throws is caught and logged (other handlers keep running); a mod that fails to load is skipped. **F10** drops all directory mods and reloads them from disk.

---

## 4. Native mods (`abi_stable`)

Native mods compile against the stable **`recraft_ext_api`** crate and export a root module the host loads with `abi_stable`, which checks the type layout + version at load and rejects a mismatch.

A native mod talks to the host through the **same JSON protocol** as JS — the host hands it three `extern "C"` function pointers (`cmd` / `query` / `hud`) bundled in `HostApi`. So a native hook builds/parses the same JSON shapes documented above.

### Cargo setup

```toml
[package]
name = "my_native_mod"
version = "1.0.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
recraft_ext_api = "0.1"
# abi_stable's macros expand to `::abi_stable` paths, so depend on it directly
# at the SAME version recraft_ext_api uses:
abi_stable = "0.11"
```

### Minimal mod

```rust
use recraft_ext_api::prelude::*;

#[derive(Default)]
struct MyMod { ticks: u64 }

impl NativePlugin for MyMod {
    fn id(&self) -> RString { RString::from("my.native.mod") }
    fn on_load(&mut self, host: HostApi) {
        host.cmd(RString::from(r#"{"t":"log","l":2,"m":"loaded"}"#));
    }
    fn on_clientbound_packet(&mut self, _h: HostApi, _p: RStr<'_>) -> bool { false /*Pass; true = Drop*/ }
    fn on_event(&mut self, _h: HostApi, _e: RStr<'_>) {}
    fn on_tick(&mut self, host: HostApi) {
        self.ticks += 1;
        if self.ticks == 40 { host.cmd(RString::from(r#"{"t":"chat","s":"native online"}"#)); }
    }
    fn on_frame(&mut self, _h: HostApi) {}
    fn on_input(&mut self, _h: HostApi, _i: RStr<'_>) -> bool { false /*true = consume*/ }
    fn draw_hud(&mut self, host: HostApi, _ctx: RStr<'_>) {
        let _player = host.query(RString::from(r#"{"k":"player"}"#)); // JSON in, JSON out
        host.hud(RString::from(r#"{"o":"text","x":2,"y":48,"s":1,"c":4294967295,"text":"native","sh":1}"#));
    }
}

#[export_root_module]
fn get_root_module() -> ExtApiRef { ExtApi { new_plugin }.leak_into_prefix() }

#[sabi_extern_fn]
fn new_plugin() -> PluginObj { NativePlugin_TO::from_value(MyMod::default(), TD_Opaque) }
```

A complete, building example is in `native/example/` (and `native/template/` is a minimal starter).

### JSON protocol (the `cmd` / `query` / `hud` payloads)

- `cmd`: `{"t":"chat","s":"…"}` · `{"t":"log","l":0|1|2|3,"m":"…"}` · `{"t":"packet","p":{…OutPacket…}}` · `{"t":"particle","kind":…,"x":…,…}` · `{"t":"sound","event":"…","x":…,…}` · `{"t":"render","r":"blockTint","id":…,"meta":-1,"color":<u32>}`
- `query` → returns JSON: `{"k":"player"}` · `{"k":"block","x":…,"y":…,"z":…}` · `{"k":"entities"}` · `{"k":"time"}` · `{"k":"dim"}`
- `hud`: `{"o":"rect","x":…,"y":…,"w":…,"h":…,"c":<u32>}` · `{"o":"text","x":…,"y":…,"s":<scale>,"c":<u32>,"text":"…","sh":0|1}` · `{"o":"item","x":…,"y":…,"sz":…,"id":…}` · `{"o":"block","x":…,"y":…,"sz":…,"id":…,"meta":…}`

The hook argument (packet/event/input/hud-ctx) is JSON with the same shapes as the JS event payloads.

### Native render hook (geometry)

Native mods — and only native mods — can submit custom world-space geometry,
drawn in the world pass right after entities (so it depth-tests against the world
and is tone-mapped/bloomed like everything else). Vertices are `ExtVertex`
(position + RGBA + a UV into the entity atlas; `color` modulates the sampled
texel, so a solid-color mesh points its UVs at an opaque atlas texel):

```rust
let v = |x, y, z, r, g, b| ExtVertex { x, y, z, r, g, b, a: 1.0, u: 0.0, v: 0.0 };
host.submit_geometry(
    &[v(0.0, 80.0, 0.0, 1.0, 0.0, 0.0),
      v(1.0, 80.0, 0.0, 0.0, 1.0, 0.0),
      v(0.5, 81.0, 0.0, 0.0, 0.0, 1.0)],
    &[0, 1, 2],
);
```

Each call **replaces** the previous submission (empty slices clear it); resubmit
when your geometry changes (e.g. from `on_frame`). This is the escape hatch the
JS layer deliberately lacks.

### Building & packaging

```sh
cargo build --release          # → target/release/libmy_native_mod.dylib (/.so /.dll)
```

Copy the dylib into `mods/<id>/` and set `entry` in `mod.toml` to its filename. Native mods are **not** cross-platform — ship one binary per OS × arch.

### Obfuscation

Rust has no true obfuscator; the workspace `release` profile uses `strip = "symbols"`. That is safe here because native mods never reference host symbols — they depend only on `recraft_ext_api`'s **layout** (verified at load). A stripped/obfuscated host still loads native mods, and a mod's `cdylib` keeps its root-module export in the *dynamic* symbol table (where the loader resolves it) even when stripped. That dynamic export is the de-facto symbol whitelist. (Verified: a `strip`ped release `cdylib` loads.)

---

## 5. Versioning

- `recraft_ext_api` follows semver; native loads are double-checked by `abi_stable`'s runtime layout hash.
- The JS API has a single version (currently `0.1.0`); the host refuses a mod whose `api` requirement it doesn't satisfy.
