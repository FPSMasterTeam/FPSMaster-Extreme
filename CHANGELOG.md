# Changelog

All notable changes to recraft are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project adheres to
[Semantic Versioning](https://semver.org/). All four crates (`recraft_app`,
`recraft_core`, `recraft_protocol`, `recraft_render`) are versioned in lockstep.

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

[0.2.0]: https://github.com/gaoyu06/MiniCraft/releases/tag/v0.2.0
