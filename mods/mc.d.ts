// Type definitions for the recraft JS extension API (`mc.*`).
//
// Drop this next to your mod and add `/// <reference path="../mc.d.ts" />` at
// the top of `main.js` (or use a `jsconfig.json`) to get autocomplete and
// type-checking in editors. It documents the globals the host injects into every
// `.js` mod: `mc`, `hud`, and a `console` shim. The API is modelled on vanilla
// (`mc.player`, `mc.world`, `mc.connection`).
//
// API version: 0.2.0

/** A color: packed 0xRRGGBBAA int, `[r,g,b]`/`[r,g,b,a]` (0–255), or `"#rrggbb"`. */
type Color = number | [number, number, number] | [number, number, number, number] | string;

/** An inventory item stack. */
interface ItemView { id: number; count: number; damage: number; }

/** The local player's abilities (vanilla PlayerCapabilities). */
interface CapabilitiesView {
  invulnerable: boolean; flying: boolean; allowFlying: boolean; creative: boolean;
  flySpeed: number; walkSpeed: number;
}

/** An active potion effect. */
interface EffectView { id: number; amplifier: number; duration: number; }

/** XP state. `bar` is the 0..1 progress to the next level. */
interface XpInfo { bar: number; level: number; }

/** The open window/container (without per-slot contents). */
interface ContainerInfo {
  windowId: number;
  /** "player" | "chest" | "furnace" | "anvil" | … */
  kind: string;
  /** Total slot count (window + the player's own 36). */
  size: number;
}

/** A block state (1.8 pre-flattening: numeric id + 4-bit meta) plus render/light
 *  properties resolved by the host. */
interface BlockView {
  id: number; meta: number;
  isAir: boolean;
  /** Emitted block light, 0..15. */
  luminance: number;
  /** Whether it fully occludes the neighbouring face. */
  opaque: boolean;
  /** Render shape: "cube" | "cross" | "fluid" | "none" | … */
  shape: string;
}

type EntityKind = "player" | "mob" | "object" | "orb";

interface EntityView {
  id: number;
  kind: EntityKind;
  /** 1.8 network type id for `mob`/`object`, else -1. */
  typeId: number;
  x: number; y: number; z: number;
  yaw: number; pitch: number;
  onGround: boolean;
  name: string | null;
  health: number | null;
}

/** The local player — a fresh snapshot each `mc.player` access (cache it in a
 *  local like vanilla), carrying the base fields, lazy read methods, and the
 *  action methods. */
interface Player {
  x: number; y: number; z: number;
  yaw: number; pitch: number;
  vx: number; vy: number; vz: number;
  onGround: boolean;
  health: number; food: number;
  sneaking: boolean; sprinting: boolean;

  // ---- extra reads (each its own query) ----
  heldItem(): ItemView | null;
  /** 45 slots; `null` per empty slot. */
  inventory(): (ItemView | null)[];
  selectedSlot(): number;
  capabilities(): CapabilitiesView;
  effects(): EffectView[];
  xp(): XpInfo;
  container(): ContainerInfo | null;

  // ---- actions ----
  /** Set the look. `opts.silent` keeps the camera put and only overrides the
   *  server-visible rotation on the next movement packet (pre-event style). */
  setRotation(yaw: number, pitch: number, opts?: { silent?: boolean }): void;
  clearRotation(): void;
  selectSlot(slot: number): void;
  swing(): void;
  /** Use the held item with no target (eat / draw bow / raise sword). */
  useItem(): void;
  attack(entity: EntityView | number): void;
  interact(entity: EntityView | number, at?: [number, number, number]): void;
  /** Place the held item against a block face. `cursor` is the in-block hit
   *  point (0..15 each); defaults to the face centre `[8,8,8]`. */
  placeBlock(x: number, y: number, z: number, face: number, cursor?: [number, number, number]): void;
  /** Raw digging: `status` 0 start, 1 cancel, 2 finish, 3 drop-stack, 4 drop,
   *  5 release-use. */
  dig(status: number, x: number, y: number, z: number, face?: number): void;
  openInventory(): void;
  closeContainer(): void;
  /** Click a slot in the open window (vanilla ClickWindow button/mode codes). */
  clickSlot(slot: number, button: number, mode: number): void;
}

interface World {
  getBlock(x: number, y: number, z: number): BlockView;
  /** World time in ticks. */
  readonly time: number;
  /** Current dimension id. */
  readonly dimension: number;
  /** Number of loaded chunks. */
  readonly loadedChunks: number;
  entities(): EntityView[];
  entity(id: number): EntityView | null;

  // ---- preset render modifications (closed, host-implemented set) ----
  /** Statically tint a block id (read by the mesher). `meta` narrows to one meta. */
  setBlockTint(id: number, color: Color, meta?: number): void;
  /** Register a content-mod full-cube block (id > 197). Takes effect only in a
   *  recraft-authoritative world. `tint` is `[r,g,b]` in 0..1. */
  registerBlock(id: number, opts?: {
    texture?: string; opaque?: boolean; alpha?: number;
    luminance?: number; tint?: [number, number, number];
  }): void;
  fullbright(on: boolean): void;
  chunkBorders(on: boolean): void;
  /** ESP-style hitbox wireframe (thick boxes) around entities. */
  entityBox(filter: "players" | "mobs" | "items" | "", color: Color, on?: boolean): void;
  nametagScale(scale: number): void;
  particleDensity(scale: number): void;
  spawnParticle(kind: number, x: number, y: number, z: number, opts?: ParticleOpts): void;
  playSound(event: string, x: number, y: number, z: number, opts?: SoundOpts): void;
}

interface Connection {
  /** Whether a server connection is active (joined + position-synced). */
  readonly connected: boolean;
  sendChat(message: string): void;
  /** Inject an outbound packet (requires the `inject_packet` capability). */
  sendPacket(packet: OutPacket): void;
}

/** Stable, direction-tagged clientbound packet type used by `mc.onPacket`. */
type PacketType =
  | "KeepAlive" | "JoinGame" | "Respawn" | "ChatMessage" | "BlockChange"
  | "MultiBlockChange" | "ChunkData" | "ChunkBulk" | "SpawnPlayer" | "SpawnMob"
  | "SpawnObject" | "SpawnExperienceOrb" | "EntityMove" | "EntityTeleport"
  | "EntityVelocity" | "DestroyEntities" | "EntityMetadata" | "PlayerPositionLook"
  | "UpdateHealth" | "SetExperience" | "SetSlot" | "WindowItems" | "HeldItemChange"
  | "SoundEffect" | "SpawnParticle" | "Effect" | "BlockAction" | "TimeUpdate"
  | "Disconnect" | "ClientboundOther";

/** Outbound (serverbound) packet type used by `mc.onServerbound`. */
type ServerboundPacketType =
  | "SbChatMessage" | "SbPlayerPosition" | "SbPlayerLook" | "SbPlayerPositionLook"
  | "SbPlayerDigging" | "SbPlayerBlockPlacement" | "SbHeldItemChange" | "SbAnimation"
  | "SbUseEntity" | "SbEntityAction" | "SbKeepAlive" | "ServerboundOther";

/** A projected clientbound packet at the `onPacket` hook. `type` is always
 *  present; other fields depend on the packet. */
interface Packet {
  type: PacketType;
  text?: string; position?: number; json?: string;     // ChatMessage
  x?: number; y?: number; z?: number; id?: number; meta?: number; // BlockChange
  mobKind?: number;                                     // SpawnMob/SpawnPlayer
  ids?: number[];                                       // DestroyEntities
  health?: number; food?: number;                       // UpdateHealth
  name?: string; volume?: number; pitch?: number;       // SoundEffect
  rawId?: number;                                       // unmodeled
}

/** A projected outbound packet at the `onServerbound` (pre-send) hook. */
interface ServerboundPacket {
  type: ServerboundPacketType;
  message?: string;                                       // SbChatMessage
  x?: number; y?: number; z?: number; onGround?: boolean; // SbPlayerPosition
  status?: number; face?: number;                         // SbPlayerDigging
  rawId?: number;                                         // unmodeled
}

interface ChatEvent { type: "Chat"; text: string; position: number; json: string; }
interface BlockChangeEvent { type: "BlockChange"; x: number; y: number; z: number; id: number; meta: number; }
interface ChunkEvent { type: "ChunkLoad" | "ChunkUnload"; x: number; z: number; }
interface EntitySpawnEvent { type: "EntitySpawn"; id: number; kind: EntityKind; typeId: number; x: number; y: number; z: number; }
interface EntityRemoveEvent { type: "EntityRemove"; id: number; }
interface PlayerHealthEvent { type: "PlayerHealth"; health: number; food: number; }

interface KeyEvent {
  /** Stable key name, e.g. "KeyW", "Escape", "F6". */
  key: string;
  pressed: boolean;
}

interface HudContext {
  /** Screen size in GUI pixels. */
  width: number; height: number;
  /** Active GUI scale factor. */
  scale: number;
  /** Whether an in-game screen (inventory/menu) is open. */
  screenOpen: boolean;
}

/** An outbound packet for `mc.connection.sendPacket` (requires the
 *  `inject_packet` capability). Handshake/login packets are intentionally
 *  unreachable. */
type OutPacket =
  | { type: "chat"; message: string }
  | { type: "playerPosition"; x: number; y: number; z: number; onGround?: boolean }
  | { type: "playerLook"; yaw: number; pitch: number; onGround?: boolean }
  | { type: "heldItemChange"; slot: number }
  | { type: "swingArm" }
  | { type: "playerDigging"; status: number; x: number; y: number; z: number; face: number };

interface ParticleOpts { ox?: number; oy?: number; oz?: number; speed?: number; count?: number; }
interface SoundOpts { volume?: number; pitch?: number; }
interface TextOpts { color?: Color; scale?: number; shadow?: boolean; }
interface IconOpts { size?: number; }

/** A Forge-style key binding, driven by the raw key event stream. */
interface KeyBinding {
  name: string;
  /** The bound key name (e.g. "KeyG", "F7"). */
  key: string;
  /** Whether the key is currently held. */
  pressed: boolean;
  onPress(cb: (kb: KeyBinding) => void): KeyBinding;
  onRelease(cb: (kb: KeyBinding) => void): KeyBinding;
  isPressed(): boolean;
}

/** A tick-based scheduler (QuickJS has no timers). */
interface Scheduler {
  /** Run `cb` once after `ticks` ticks. Returns a cancellation id. */
  after(ticks: number, cb: () => void): number;
  /** Run `cb` every `ticks` ticks. Returns a cancellation id. */
  every(ticks: number, cb: () => void): number;
  clear(id: number): void;
}

/** Per-mod JSON config, persisted to `<mod>/config.json`. */
interface Config {
  /** The live config object (mutate then `save()`). */
  readonly data: Record<string, unknown>;
  /** Merge `defaults` under the loaded data and return it. */
  load<T extends object>(defaults: T): T & Record<string, unknown>;
  get<T = unknown>(key: string, def?: T): T;
  set(key: string, value: unknown): Config;
  save(): void;
}

interface Mc {
  /** The local player (fresh snapshot per access). */
  readonly player: Player;
  readonly world: World;
  readonly connection: Connection;

  log(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;

  /** Monotonic real-time milliseconds (for rate-limiting actions in real time). */
  now(): number;

  // ---- event subscription ----
  on(event: "tick" | "frame" | "load", cb: () => void): void;
  /** Return `true` to consume the key (suppress default gameplay handling). */
  on(event: "key", cb: (e: KeyEvent) => boolean | void): void;
  on(event: "chat", cb: (e: ChatEvent) => void): void;
  on(event: "blockChange", cb: (e: BlockChangeEvent) => void): void;
  on(event: "chunkLoad" | "chunkUnload", cb: (e: ChunkEvent) => void): void;
  on(event: "entitySpawn", cb: (e: EntitySpawnEvent) => void): void;
  on(event: "entityRemove", cb: (e: EntityRemoveEvent) => void): void;
  on(event: "playerHealth", cb: (e: PlayerHealthEvent) => void): void;
  /** Subscribe to a raw clientbound packet by type. Return `false` to drop it. */
  onPacket(type: PacketType, cb: (p: Packet) => boolean | void): void;
  /** Pre-send hook for an outbound packet by type. Return `false` to drop it. */
  onServerbound(type: ServerboundPacketType, cb: (p: ServerboundPacket) => boolean | void): void;
  /** Register a per-frame HUD draw callback (use the `hud` global inside). */
  drawHud(cb: (ctx: HudContext) => void): void;

  // ---- Forge-style helpers ----
  keyBinding(name: string, defaultKey: string): KeyBinding;
  scheduler: Scheduler;
  config: Config;
}

interface Hud {
  rect(x: number, y: number, w: number, h: number, color: Color): void;
  text(x: number, y: number, text: string, opts?: TextOpts): void;
  itemIcon(x: number, y: number, itemId: number, opts?: IconOpts): void;
  blockItem(x: number, y: number, blockId: number, meta: number, opts?: IconOpts): void;
}

declare const mc: Mc;
declare const hud: Hud;
declare const console: {
  log(...args: unknown[]): void;
  info(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
};
