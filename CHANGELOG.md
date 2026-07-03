# Changelog

All notable changes to fpsmaster are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project adheres to
[Semantic Versioning](https://semver.org/). The crates (`fpsmaster_app`,
`fpsmaster_core`, `fpsmaster_protocol`, `fpsmaster_render`, `fpsmaster_ext`,
`fpsmaster_ext_api`) are versioned in lockstep.

## [0.3.1] - 2026-06-19

### Fixed

- **GL backend startup crash** — the post, water-SSR and volumetric passes read
  the world depth buffer with a non-comparison `textureLoad`, which naga's GLSL
  backend can't translate (it maps a depth texture to `sampler2DShadow`, which
  has no plain load/sample overload), panicking pipeline creation on the GL
  backend. These read-only depth inputs are now bound as unfilterable-float
  textures (`texture_2d<f32>`) so they emit a plain `sampler2D` / `texelFetch` on
  every backend. The shadow map (comparison-sampled) is unchanged.

## [0.3.0] - 2026-06-19

The extension-system milestone. fpsmaster grows a two-layer mod platform — a
sandbox-friendly JavaScript layer for behaviour/HUD/automation and a native
(`cdylib`) layer for deep rendering and content — plus the SDK and example mods
to build against it. Built from the plan in `docs/PLAN_EXTENSION_SYSTEM.md`.
Also tightens the network send/receive timing to be vanilla bit-for-bit so
ext-driven automation stays anti-cheat-legit.

The extension API is versioned `0.3.0` — a breaking bump from `0.2` (the native
`HostApi`/`geometry` ABI grew fields for the render API), so mods must declare
`api = "^0.3"` and native mods must be recompiled.

### Added

#### Extension system — core
- **Two-layer mod platform** (`fpsmaster_ext`) — a host event bus + command queue
  threaded through four seams (clientbound packets, input, HUD assembly, tick),
  with all mod code on the main thread and per-mod error isolation. The 6500-line
  `game.rs` packet handler was split into discrete per-packet functions to host
  the seams.
- **`mods/` loader** — manifest (`mod.toml`) parsing, capability gating, load
  ordering, and hot-reload.

#### Extension system — JavaScript layer
- **`mc.*` JS API** (rquickjs) — `mc.player` / `mc.world` / `mc.connection`
  read-views, event subscriptions, chat/command injection, HUD drawing,
  keybinding + scheduler + config, an `on_serverbound` pre-send hook, and
  `mc.now()` real-time access. (Replaces the earlier `fpsmaster.*` prototype API.)
- **Preset render modifications** — `setBlockTint` (global tint registry +
  re-mesh), fullbright (forced full lightmap), a built-in vanilla block outline,
  and thick white entity hitboxes — all toggleable from JS.

#### Extension system — native layer
- **Stable native ABI** (`fpsmaster_ext_api`, abi_stable) — `NativePlugin` trait
  with a serverbound hook, interaction + rich read-view HostApi helpers, and
  runtime layout/version checking that refuses to load a mismatched mod.
- **Native render hook** — submit custom world geometry (`ExtVertex` /
  `submit_geometry`) against real renderer resources, batched (no per-block
  callbacks).
- **Content blocks** — a runtime registry overlay for block ids beyond the
  vanilla 1.8.9 range (self-owned worlds).

#### Extension system — expanded render API
- **Mod textures** — `mc.loadTexture` (PNG/JPEG from the mod folder),
  `createTexture` + `updateTexture` (stream pixels, e.g. an off-screen renderer
  pushing frames), and `registerTexture` (raw RGBA). Host-owned registry,
  process-unique handles.
- **HUD primitives** — `hud.image` (whole or sprite sub-rect of a mod texture),
  `hud.line`, and `hud.gradient`, with native parity (`hud_image` / `hud_line` /
  `hud_gradient` / `hud_text_ex`).
- **Textured native geometry** — `submit_geometry_textured` lets the native
  render hook sample a mod-registered texture instead of the entity atlas; its
  GPU upload is decoupled from geometry resubmission.
- **Full-screen post effect** — `mc.setPostEffect(wgsl)` runs a mod WGSL fragment
  (`fn effect(uv, color)`) over the composited world, with `U.time`/`U.resolution`
  uniforms and scene sampling. Shader compile errors are isolated (logged, the
  previous effect kept) rather than fatal.

#### SDK & examples
- **Extension SDK** (`sdk/`) — JS TypeScript typings (`mc.d.ts`), a native build
  guide + template + worked example, an API reference, and a bundled
  `fpsmaster_ext_api` snapshot so the SDK builds without crates.io.
- **Example mods** in `mods/` — `coords_hud`, `chat_alert`, `block_tint`,
  `preset_demo` (toggles every render preset by key), `scaffold_demo` (a
  vanilla-legit auto-bridge demonstrating the `mc.*` API), and `render_demo`
  (the textures / HUD primitives / post-effect surface).
- **Mod-management screen** — a `Mods…` button on the title/pause screen to
  list / toggle / reload mods and open the mods folder; disabled state persists
  to `fpsmaster_options.txt`.

### Changed
- **Anti-cheat-legit silent rotation & automation** — the extension tick now
  runs before physics so silent yaw matches the movement physics integrates
  with; movement is strafe-locked under a silent yaw; the silent look is
  GCD-quantized to pass Grim's `AimModulo360`; and placement uses a real-time
  cooldown to defeat fastplace detection. Together these let ext-driven
  silent-aim + auto-place pass Grim/AAC.

### Fixed
- **Large-coordinate "distance effect"** — the world is now rendered
  camera-relative (a per-frame render origin at the camera's block, subtracted
  from every world position before it hits an f32 matrix). Previously the whole
  pipeline transformed absolute world coordinates, so far from spawn
  `view_proj * world_pos` cancelled catastrophically: block/foliage jitter and
  z-fighting, shadows that shimmered (worst while moving), and motion-blur
  jitter. Block geometry, water, shadows, fog, volumetric light and motion blur
  are all stable now. The shadow texel-snap was moved to clip space (removing the
  world-origin lever arm), and motion-blur reprojection is composed in f64 on the
  CPU so the large translations cancel there instead of in the shader.
- **Vanilla packet timing** — incoming packets are processed per-frame (as
  vanilla does), and outgoing acks/commands are handled on the main thread in
  vanilla per-tick order, instead of fpsmaster's previous ad-hoc scheduling.
- **macOS GPU-memory leak** — skip rendering entirely while the window is
  occluded (the swapchain was throttled but still accumulating).
- Scaffold-demo placement now derives from a real ray cast and only places
  against faces it can actually see.

## [0.2.0] - 2026-06-17

A large client-parity release: most of the remaining 1.8.9 visual/audio/HUD gap
is closed, the mob and block coverage is filled in against the decompiled
oracle, and the physics paths are tightened to vanilla bit-for-bit. Worked from
the 28-task plan in `docs/PLAN_0_2_0.md`.

### Added

#### Rendering
- **Particle system** — instanced billboard pool covering the full 1.8
  `EnumParticleTypes` set with vanilla per-type physics; fed by `SpawnParticle`,
  block break/landing/footsteps, crits, and block-break debris.
- **Special block-entities** — signs (with `UpdateSign` text), the enchanting-
  table floating book, bookshelves, and the nether/end portal planes.
- **Fluid rendering** — level-dependent water/lava surfaces: 8-level meta height,
  corner-interpolated sloped tops, flow-direction UV.
- **Animated terrain & fire** — water/lava/fire/portal frame animation; fire
  rendered as tall crossed planes with wall-cling faces.
- **Animated on-fire screen overlay** and a faithful first-person fire overlay.
- **Entity coverage** — entity atlas widened to 128 px slots; modelled the
  previously-missing mobs (iron golem, horse, witch, ghast, blaze, guardian,
  wither, ender dragon, rabbit, …) so no mob falls back to a placeholder box.
- **Armor & second-skin layers** on humanoids (helmet/chest/legs/boots from
  equipment metadata, plus the hat/jacket overlay).
- **Enchantment glint** — scrolling additive glint on held items, inventory
  icons, and worn armor.
- **Projectile rendering** — 3D arrow model (`RenderArrow` geometry, new `Arrow`
  atlas slot) plus 2D item billboards for snowball/egg/ender pearl/etc.
- **Falling-block** entities, **experience-orb** billboards, and the **double
  (large) chest** model.
- **Chest open/close animation** driven from `BlockAction` (normal/large/ender/
  trapped), with open/close sounds.
- **Held-item models, blocking pose, and hurt-camera shake.**
- **Death animation** — vanilla `deathTime` fall-over tilt for mobs and worn
  armor; the corpse stays red.

#### Audio
- **Sound system** (kira/cpal) — 1.8.9 `sounds.json` mapping, stereo positioning
  and distance falloff, driven by `SoundEffect`/`NamedSoundEffect` plus local
  events (place/break, footsteps, hurt, UI clicks, eat/drink, chest, note block).
- **Note block** — `BlockAction`-driven pitch/instrument playback with coloured
  note particles.

#### HUD / GUI / input
- Faithful vanilla **health/hunger/XP HUD** (absorption, max-health, potion
  effects) with the hurt sound, and the vanilla inversion-blend **crosshair**.
- **Inventory player-model preview** that tracks the mouse.
- **Boss health bar** derived client-side from in-range wither/dragon health.
- Per-type **vanilla container GUIs** (crafting/furnace/chest/dispenser/hopper/
  brewing/anvil/enchanting/beacon/villager/horse, …).
- **Customizable key bindings** for all actions, persisted to options.
- **Chat tab-completion** (`TabComplete`) and **clickable chat components**
  (`clickEvent`/`hoverEvent`: run/suggest command, open url, copy).
- **OldAnimations** toggle — 1.7-style swing/hurt/rod/blocking animations
  alongside the 1.8 set.
- **Show FPS** option; the debug HUD no longer shows unless F3 is on.

#### Core / physics
- **Client-side projectile physics** — snowball/egg/ender pearl/arrow arc
  immediately at zero latency (gravity + air drag, arrows stick into blocks),
  with the server still correcting drift through the normal lerp.
- **Vanilla mining model** — `getBreakSpeed`/`canHarvestBlock`/break-tick timing
  with the full tool tables (efficiency tiers, harvest levels, efficiency
  enchant, haste/fatigue, in-air penalty) so digging stays Grim-legal.

### Changed
- All four crates bumped to **0.2.0**.
- **Render-distance safety net** — columns that drift beyond the view distance
  have their GPU meshes dropped (block data kept, re-meshed on return) so
  resident VRAM stays bounded even on servers that never send `ChunkUnload`.

### Fixed
- **Physics precision** — `jump()` now uses the vanilla `(double)0.42F`
  promotion (`0.41999998688697815`, not the round double `0.42`); in-fluid
  held-jump buoyancy likewise uses `(double)0.04F`. Pinned with an exact-bits
  test (Grim's JumpPower mirrors the float).
- **Per-species collision boxes** (entities no longer share one `0.3×1.9` box)
  and the complete 1.8.9 **block-hardness table**.
- **Dropped-item physics** — items arc immediately instead of stalling in the
  air waiting for a server velocity packet.
- Many render orientation/coverage bugs: chest lid upside-down, quadruped body
  texture reversed (pig/cow/sheep/chicken), sign + enchant-book textures
  flipped, sign text basis (and illegible CJK — now rasterized at 2×), fire
  block upside-down, first-person arm skin mirrored, zombie/pigman/giant missing
  left arm and leg, particles back-face culled (invisible), block render parity
  (no magenta fallback for any 1.8 id), and the constant vanilla red damage
  flash.
- Held item no longer painted over by the SSR water pass (depth-write fix).

## [0.1.0]

Initial 1.8.9 client: world/chunk rendering, terrain meshing and lighting,
player physics and collision, the 1.8.9 protocol (online and offline mode),
basic entity rendering, and core GUIs.

[0.3.1]: https://github.com/FPSMasterTeam/FPSMaster-Extreme/releases/tag/v0.3.1
[0.3.0]: https://github.com/FPSMasterTeam/FPSMaster-Extreme/releases/tag/v0.3.0
[0.2.0]: https://github.com/FPSMasterTeam/FPSMaster-Extreme/releases/tag/v0.2.0
