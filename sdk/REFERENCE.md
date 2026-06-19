# recraft Extension SDK

recraft has a two-layer extension system:

| Layer | For | Distribution | Power |
|---|---|---|---|
| **JS** (`tier = "js"`) | behaviour / automation / HUD / config | one `.js` file, cross-platform, hot-reloadable | events, commands, read-views, HUD drawing, key bindings, a scheduler, per-mod config, a closed set of preset render toggles |
| **Native** (`tier = "native"`) | depth / performance / arbitrary native code | a `cdylib` per OS × arch | everything JS can do **plus** unrestricted native code (own threads/state, heavy CPU work, raw render hooks, direct file IO) |

Both layers are bindings over one internal **event bus + command queue** (`recraft_ext`), so they share the exact same event/command semantics; the JS and native APIs are the same surface with different transports.

The JS API is modelled on vanilla Minecraft: `mc.player`, `mc.world`, `mc.connection`.

> There is **no sandbox**. A JS mod's errors are isolated (a thrown handler is caught and logged, a broken mod is disabled), but a native mod runs in-process with full access and can crash the host. Capabilities are a *declare + confirm* trust signal, not an enforcement boundary.

**API version: 0.2.0**

---

## 1. Quickstart (JS)

Mods live under `mods/<id>/`, each with a `mod.toml` and an entry file. Create `mods/hello/mod.toml`:

```toml
id = "hello"
version = "1.0.0"
tier = "js"
api = "^0.3"
entry = "main.js"
capabilities = ["hud", "read_player"]
name = "Hello"
description = "Minimal example."
```

and `mods/hello/main.js`:

```js
/// <reference path="../mc.d.ts" />

mc.on("load", () => mc.log("hello loaded"));

mc.drawHud((ctx) => {
  const p = mc.player;
  hud.text(2, 2, `y=${p.y.toFixed(1)}`, { color: 0xffff55ff });
});
```

Launch recraft from the directory that contains `mods/`. Press **F10** to hot-reload all mods after editing. Copy this SDK's `js/mc.d.ts` next to your mods for editor autocomplete/type-checking.

Four worked examples are in `js/examples/`: `coords_hud`, `chat_alert`, `block_tint`, `preset_demo`.

---

## 2. `mod.toml`

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | Unique mod id. |
| `version` | yes | Mod version. |
| `tier` | yes | `"js"` or `"native"`. |
| `api` | yes | Semver requirement against the host API (e.g. `"^0.2"`). The host refuses incompatible mods. |
| `entry` | yes | `main.js` for JS; the dylib filename (`librecraft_*.dylib` / `.so` / `.dll`) for native. |
| `depends` | no | List of mod ids this mod loads after (topologically ordered; cycles are an error). |
| `capabilities` | no | Declared capabilities (see below). |
| `name`, `description` | no | Display metadata. |

### Capabilities

`hud`, `read_world`, `read_player`, `read_entities`, `inject_packet`, `chat`, `sound`, `particle`, `render`, `input`.

They are logged at load and surfaced for trust decisions; `inject_packet` is flagged sensitive. They are advisory in this version (no sandbox).

---

## 3. JS API

Everything below is injected as the globals `mc`, `hud`, and a `console` shim. See `js/mc.d.ts` for exact types.

### Events

```js
mc.on("tick", () => {});            // 20 Hz
mc.on("frame", () => {});           // per frame
mc.on("load", () => {});            // once, after load
mc.on("key", (e) => e.pressed);     // e:{key,pressed}; return true to CONSUME the key
mc.on("chat", (e) => {});           // e:{text,position,json}
mc.on("blockChange", (e) => {});    // e:{x,y,z,id,meta}
mc.on("chunkLoad", (e) => {});      // e:{x,z}
mc.on("chunkUnload", (e) => {});    // e:{x,z}
mc.on("entitySpawn", (e) => {});    // e:{id,kind,typeId,x,y,z}
mc.on("entityRemove", (e) => {});   // e:{id}
mc.on("playerHealth", (e) => {});   // e:{health,food}

mc.onPacket("BlockChange", (p) => {});        // raw clientbound packet; return false to DROP
mc.onServerbound("SbChatMessage", (p) => {}); // outbound packet, BEFORE send; return false to DROP
mc.drawHud((ctx) => {});            // ctx:{width,height,scale,screenOpen}; draw with `hud.*`
```

`onPacket` types are the stable `PacketType` names (not vanilla wire ids); `onServerbound` types are the `Sb*` outbound names (e.g. `SbChatMessage`, `SbPlayerDigging`, `SbPlayerBlockPlacement`). High-frequency packets are opt-in (you only pay for what you subscribe to).

Coverage notes: chunk packets and net-thread control packets (KeepAlive/ConfirmTransaction) are **not** delivered to `onPacket` — use `onChunkLoad`/`onChunkUnload` for chunks. `onServerbound` sees the natural per-tick client packets (digging, block placement, use-entity, held-item, swing, chat); **movement is not routed through it** (a dropped move would desync physics), and packets a mod itself injects don't re-fire it.

### `mc.player`

`mc.player` is a **fresh snapshot each access** (cache it in a local, like vanilla `mc.thePlayer`). It carries the base fields, lazy read methods, and the action methods.

```js
const p = mc.player;
// fields: x,y,z, yaw,pitch, vx,vy,vz, onGround, health, food, sneaking, sprinting

// extra reads (each runs its own query):
p.heldItem();      // {id,count,damage} | null
p.inventory();     // 45 slots, null per empty
p.selectedSlot();  // 0..8
p.capabilities();  // {invulnerable,flying,allowFlying,creative,flySpeed,walkSpeed}
p.effects();       // [{id,amplifier,duration}, ...]
p.xp();            // {bar,level}
p.container();     // {windowId,kind,size} | null

// actions (sent to the server):
p.setRotation(yaw, pitch, { silent: true }); // silent = server-only turn, camera unchanged
p.clearRotation();                            // drop a silent override
p.selectSlot(2);
p.swing();
p.useItem();                                  // eat / draw bow / raise sword
p.attack(entityOrId);
p.interact(entityOrId, [hx, hy, hz]?);        // optional InteractAt hit point
p.placeBlock(x, y, z, face, [cx,cy,cz]?);     // cursor defaults to face centre [8,8,8]
p.dig(status, x, y, z, face?);                // 0 start,1 cancel,2 finish,3 drop-stack,4 drop,5 release
p.openInventory();
p.closeContainer();
p.clickSlot(slot, button, mode);              // vanilla ClickWindow codes
```

**Silent rotation** is the vanilla "pre-event" pattern: with `{silent:true}` the camera/view stay put and only the *server-visible* yaw/pitch on the next movement packet change — useful for silent aim. Clear it with `clearRotation()`.

### `mc.world`

```js
mc.world.getBlock(x, y, z);   // {id,meta,isAir,luminance,opaque,shape}
mc.world.time;                // ticks (getter)
mc.world.dimension;           // 0 overworld, -1 nether (getter)
mc.world.loadedChunks;        // count (getter)
mc.world.entities();          // [{id,kind,typeId,x,y,z,yaw,pitch,onGround,name,health}, ...]  (local player excluded)
mc.world.entity(id);          // one entity | null

// preset render modifications (see below)
// content blocks: mc.world.registerBlock(...)
mc.world.spawnParticle(26 /*flame*/, x, y, z, { count: 5, speed: 0.02 });
mc.world.playSound("random.orb", x, y, z, { pitch: 1.5 });
```

### `mc.connection`

```js
mc.connection.connected;             // bool (getter): joined + position-synced
mc.connection.sendChat("hello");     // or a /command
mc.connection.sendPacket({ type: "swingArm" });  // needs `inject_packet`
```

`sendPacket` accepts a whitelisted play-state packet: `chat`, `playerPosition`, `playerLook`, `heldItemChange`, `swingArm`, `playerDigging`. Handshake/login packets are unreachable. (Most interactions have a dedicated `mc.player.*` method — prefer those; `sendPacket` is the raw escape hatch.)

### HUD drawing

Inside a `drawHud` callback. Coordinates are GUI pixels (the host scales them). Colors accept a packed `0xRRGGBBAA` int, `[r,g,b(,a)]` (0–255), or `"#rrggbb"`.

```js
hud.rect(x, y, w, h, color);
hud.text(x, y, "label", { color, scale, shadow });
hud.itemIcon(x, y, itemId, { size });
hud.blockItem(x, y, blockId, meta, { size });
hud.image(x, y, w, h, handle, { src: [sx, sy, sw, sh] });  // mod texture (src optional)
hud.line(x0, y0, x1, y1, color, width);
hud.gradient(x, y, w, h, topColor, bottomColor);
```

### Mod textures

Register a texture (inside a hook — the command queue isn't live during top-level
eval), then draw it with `hud.image` or sample it from native geometry. Handles
are process-unique.

```js
let tex;
mc.on("load", () => {
  tex = mc.loadTexture("ui/panel.png");      // PNG/JPEG from the mod folder
  // or: mc.registerTexture(rgbaBytes, w, h)  // raw RGBA (w*h*4)
  // or: mc.createTexture(w, h)               // blank, stream into it later
});
mc.updateTexture(tex, rgbaBytes);            // replace pixels (same dimensions)
mc.freeTexture(tex);
```

`updateTexture` is the channel for an off-screen renderer (e.g. a CEF browser in
OSR mode) to push frames; large per-frame updates from JS are heavy, so do HD
streaming from a native mod.

### Full-screen post effect

Run a custom WGSL fragment over the composited world (the HUD is drawn after, so
it stays crisp). The snippet defines `effect`; `U.time`/`U.resolution` and
`src_tex`/`src_samp` are in scope. A compile error is logged and the previous
effect kept.

```js
mc.setPostEffect(`
  fn effect(uv: vec2<f32>, color: vec4<f32>) -> vec4<f32> {
    let g = dot(color.rgb, vec3<f32>(0.299, 0.587, 0.114));
    return vec4<f32>(vec3<f32>(g), color.a);  // grayscale
  }`);
mc.clearPostEffect();
```

### Preset render modifications

A **closed, host-implemented** set (the JS layer's "controlled rendering" — adding one means changing host code; arbitrary rendering is native-only).

```js
mc.world.setBlockTint(1, [120, 200, 255]);   // statically tint a block id (read by the mesher)
mc.world.fullbright(true);
mc.world.chunkBorders(true);
mc.world.entityBox("mobs", "#ffffff", true);  // thick hitbox wireframe
mc.world.nametagScale(1.5);
mc.world.particleDensity(0.5);
```

All presets are wired to the renderer: `setBlockTint` (chunk mesher), `fullbright` (forces the world lightmap to full — caves/night fully visible, no washout), `nametagScale` (nametag world size), `particleDensity` (spawn-count multiplier), `chunkBorders` (the player chunk's grid), and `entityBox` (a thick white hitbox wireframe around entities). The targeted-block outline is built into recraft (always drawn, like vanilla), not a preset.

### Content blocks (experimental)

`mc.world.registerBlock` (and `HostApi::register_block` for native mods) registers a new full-cube block beyond the vanilla `0..=197` id range, with full render/collision/light properties from a runtime registry overlay (vanilla ids keep their fast path):

```js
mc.on("load", () => mc.world.registerBlock(300, {
  texture: "stone",          // reuse a vanilla atlas texture for now
  opaque: true, alpha: 1.0,
  luminance: 7,              // 0..15 light emitted
  tint: [0.4, 0.8, 1.0],     // optional [r,g,b] 0..1
}));
```

**Caveat:** this only manifests in a *recraft-authoritative* world. recraft is a client to a vanilla/Paper 1.8 server, which never sends mod block ids, so there is currently no world where a registered mod block can be placed. Per-mod *texture* registration is also not implemented yet.

### Key bindings

A Forge-style key binding built on the raw key stream:

```js
const fly = mc.keyBinding("Toggle fly", "KeyG")
  .onPress(() => mc.log("G pressed"))
  .onRelease(() => mc.log("G released"));

mc.on("tick", () => { if (fly.isPressed()) { /* ... */ } });
```

Key names are the stable codes from key events (`"KeyG"`, `"F7"`, …). A binding does not consume the key globally; use `mc.on("key", …)` returning `true` if you need to suppress default handling.

### Scheduler

QuickJS has no `setTimeout`; use the tick-based scheduler:

```js
mc.scheduler.after(40, () => mc.log("2s later"));       // once, returns an id
const id = mc.scheduler.every(20, () => mc.log("tick")); // repeat every 1s
mc.scheduler.clear(id);
```

### Config

Per-mod JSON config, persisted to `<mod>/config.json`:

```js
const cfg = mc.config.load({ enabled: true, color: "#ffaa00" }); // defaults under loaded data
if (cfg.enabled) { /* ... */ }
mc.config.set("enabled", false);   // or mutate mc.config.data directly
mc.config.save();                  // writes <mod>/config.json
mc.config.get("color", "#fff");    // read with a fallback
```

### Error isolation & hot reload

Each mod runs in its own QuickJS context. A handler that throws is caught and logged (other handlers keep running); a mod that fails to load is skipped. **F10** drops all directory mods and reloads them from disk.

---

## 4. Native mods (`abi_stable`)

Native mods compile against the stable **`recraft_ext_api`** crate and export a root module the host loads with `abi_stable`, which checks the type layout + version at load and rejects a mismatch.

A native mod talks to the host through the **same JSON protocol** as JS — the host hands it function pointers (`cmd` / `query` / `hud` / `geometry`) bundled in `HostApi`, plus ergonomic Rust helpers that build the JSON for you.

### Cargo setup

```toml
[package]
name = "my_native_mod"
version = "1.0.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
# Bundled with this SDK — no crates.io needed. (Or a git dep:
#   recraft_ext_api = { git = "https://github.com/gaoyu06/MiniCraft" }  )
recraft_ext_api = { path = "../recraft_ext_api" }
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
    fn on_load(&mut self, host: HostApi) { host.log("loaded"); }
    fn on_clientbound_packet(&mut self, _h: HostApi, _p: RStr<'_>) -> bool { false /*Pass; true = Drop*/ }
    fn on_event(&mut self, _h: HostApi, _e: RStr<'_>) {}
    fn on_tick(&mut self, host: HostApi) {
        self.ticks += 1;
        if self.ticks == 40 { host.send_chat("native online"); }
    }
    fn on_frame(&mut self, _h: HostApi) {}
    fn on_input(&mut self, _h: HostApi, _i: RStr<'_>) -> bool { false /*true = consume*/ }
    fn draw_hud(&mut self, host: HostApi, _ctx: RStr<'_>) {
        let _player = host.player_json();             // JSON in, JSON out
        host.hud_text(2, 48, "native", 0xffff_ffff);
    }
    // Optional (0.2, default = don't drop): pre-send hook for outbound packets.
    fn on_serverbound_packet(&mut self, _h: HostApi, _p: RStr<'_>) -> bool { false }
}

#[export_root_module]
fn get_root_module() -> ExtApiRef { ExtApi { new_plugin }.leak_into_prefix() }

#[sabi_extern_fn]
fn new_plugin() -> PluginObj { NativePlugin_TO::from_value(MyMod::default(), TD_Opaque) }
```

A complete, building example is in `native/example/` (and `native/template/` is a minimal starter). `on_serverbound_packet` has a default body, so older mods stay source- and ABI-compatible.

### `HostApi` helpers

Reads return JSON `RString` (parse with your JSON crate) or a primitive:

```rust
host.player_json(); host.block_json(x,y,z); host.entities_json(); host.entity_json(id);
host.held_item_json(); host.inventory_json(); host.capabilities_json();
host.effects_json(); host.xp_json(); host.container_json();
host.selected_slot(); host.world_time(); host.dimension(); host.connected();
```

Actions:

```rust
host.send_chat("hi");  host.log/warn/error("…");
host.place_block(x,y,z, face, [8,8,8]);  host.dig(status, x,y,z, face);
host.attack_entity(id);  host.interact_entity(id);  host.interact_entity_at(id, [x,y,z]);
host.container_click(slot, button, mode);  host.container_close();  host.open_inventory();
host.select_slot(2);  host.swing();  host.use_item();
host.set_rotation(yaw, pitch, /*silent*/ true);  host.clear_rotation();
host.set_block_tint(id, 0xRRGGBBAA);  host.register_block(id, "stone", true, 1.0, 7);
host.hud_text(x,y,"…",color);  host.hud_rect(x,y,w,h,color);
host.submit_geometry(&verts, &indices);
```

Config: native mods have full native access, so just read/write your own file with `std::fs` (no config command needed).

### JSON protocol (the `cmd` / `query` / `hud` payloads)

For when you build the JSON yourself instead of using the helpers:

- **`cmd`** (`{"t": …}`): `chat{s}` · `log{l:0|1|2|3, m}` · `packet{p:{…OutPacket…}}` · `particle{kind,x,y,z,…}` · `sound{event,x,y,z,…}` · `render{r:"blockTint"|"fullbright"|"chunkBorders"|"entityBox"|"nametagScale"|"particleDensity", …}` · `block{id,texture,opaque,alpha,lum,tint}` · `place{x,y,z,face,cx,cy,cz}` · `dig{status,x,y,z,face}` · `attack{id}` · `interact{id,ax?,ay?,az?}` · `click{slot,button,mode}` · `close` · `openInv` · `selectSlot{slot}` · `swing` · `useItem` · `rotate{yaw,pitch,silent}` · `clearRotate` · `saveConfig{dir,json}`
- **`query`** (`{"k": …}` → JSON): `player` · `block{x,y,z}` · `entities` · `entity{id}` · `time` · `dim` · `chunks` · `connected` · `held` · `selectedSlot` · `inventory` · `capabilities` · `effects` · `xp` · `container`
- **`hud`** (`{"o": …}`): `rect{x,y,w,h,c}` · `text{x,y,s,c,text,sh}` · `item{x,y,sz,id}` · `block{x,y,sz,id,meta}` · `image{x,y,w,h,tex[,sx,sy,sw,sh]}` · `line{x,y,x2,y2,c,w}` · `gradient{x,y,w,h,c,c2}`

The hook argument (packet/event/input/hud-ctx) is JSON with the same shapes as the JS event payloads. An outbound packet at `on_serverbound_packet` is JSON like `{"type":"SbChatMessage","message":"…"}`.

### Native render hook (geometry)

Native mods — and only native mods — can submit custom world-space geometry, drawn in the world pass right after entities (so it depth-tests against the world and is tone-mapped/bloomed like everything else). Vertices are `ExtVertex` (position + RGBA + a UV into the entity atlas; `color` modulates the sampled texel, so a solid-color mesh points its UVs at an opaque atlas texel):

```rust
let v = |x, y, z, r, g, b| ExtVertex { x, y, z, r, g, b, a: 1.0, u: 0.0, v: 0.0 };
host.submit_geometry(
    &[v(0.0, 80.0, 0.0, 1.0, 0.0, 0.0),
      v(1.0, 80.0, 0.0, 0.0, 1.0, 0.0),
      v(0.5, 81.0, 0.0, 0.0, 0.0, 1.0)],
    &[0, 1, 2],
);
```

Each call **replaces** the previous submission (empty slices clear it); resubmit when your geometry changes (e.g. from `on_frame`). This is the escape hatch the JS layer deliberately lacks.

To texture the geometry with your own asset instead of the entity atlas, register a texture (`host.register_texture(rgba, w, h)` / `host.load_texture(path)`) and submit with normalized UVs via `host.submit_geometry_textured(&verts, &indices, tex)`. Its GPU upload is decoupled from geometry resubmission, so streaming geometry every frame doesn't re-upload the texture. Native mods can also drive the full-screen post effect (`host.set_post_effect(wgsl)` / `host.clear_post_effect()`) and every texture/HUD helper the JS layer has.

### Building & packaging

```sh
cargo build --release          # → target/release/libmy_native_mod.dylib (/.so /.dll)
```

Copy the dylib into `mods/<id>/` and set `entry` in `mod.toml` to its filename. Native mods are **not** cross-platform — ship one binary per OS × arch.

### Obfuscation

Rust has no true obfuscator; the workspace `release` profile uses `strip = "symbols"`. That is safe here because native mods never reference host symbols — they depend only on `recraft_ext_api`'s **layout** (verified at load). A stripped/obfuscated host still loads native mods, and a mod's `cdylib` keeps its root-module export in the *dynamic* symbol table (where the loader resolves it) even when stripped.

---

## 5. Versioning

- `recraft_ext_api` follows semver; native loads are double-checked by `abi_stable`'s runtime layout hash.
- The JS and native APIs share a single version (currently `0.3.0`); the host refuses a mod whose `api` requirement it doesn't satisfy. **0.3 is a breaking bump from 0.2** — it adds the expanded render API (mod textures, `hud.image`/`line`/`gradient`, textured native geometry, full-screen post effects) and the native `HostApi`/`geometry` ABI gained fields, so recompile native mods and declare `api = "^0.3"`. (0.2 was itself a breaking bump from 0.1: the JS global was renamed `recraft` → `mc` and restructured around `mc.player`/`mc.world`/`mc.connection`.)
