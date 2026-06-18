// Type definitions for the recraft JS extension API.
//
// Drop this next to your mod and add `/// <reference path="../recraft.d.ts" />`
// at the top of `main.js` (or use a `jsconfig.json`) to get autocomplete and
// type-checking in editors. It documents the globals the host injects into every
// `.js` mod: `recraft`, `hud`, and a `console` shim.
//
// API version: 0.1.0

/** A color: packed 0xRRGGBBAA int, `[r,g,b]`/`[r,g,b,a]` (0–255), or `"#rrggbb"`. */
type Color = number | [number, number, number] | [number, number, number, number] | string;

interface PlayerView {
  x: number; y: number; z: number;
  yaw: number; pitch: number;
  vx: number; vy: number; vz: number;
  onGround: boolean;
  health: number; food: number;
  sneaking: boolean; sprinting: boolean;
}

/** A block state (1.8 pre-flattening): numeric id + 4-bit meta. */
interface BlockView { id: number; meta: number; }

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

/** Stable, direction-tagged packet type used by `onPacket(type, cb)`. */
type PacketType =
  | "KeepAlive" | "JoinGame" | "Respawn" | "ChatMessage" | "BlockChange"
  | "MultiBlockChange" | "ChunkData" | "ChunkBulk" | "SpawnPlayer" | "SpawnMob"
  | "SpawnObject" | "SpawnExperienceOrb" | "EntityMove" | "EntityTeleport"
  | "EntityVelocity" | "DestroyEntities" | "EntityMetadata" | "PlayerPositionLook"
  | "UpdateHealth" | "SetExperience" | "SetSlot" | "WindowItems" | "HeldItemChange"
  | "SoundEffect" | "SpawnParticle" | "Effect" | "BlockAction" | "TimeUpdate"
  | "Disconnect" | "ClientboundOther";

/** A projected packet at the `onPacket` hook. `type` is always present; other
 *  fields depend on the packet (only mod-relevant packets carry decoded fields). */
interface Packet {
  type: PacketType;
  // ChatMessage
  text?: string; position?: number; json?: string;
  // BlockChange
  x?: number; y?: number; z?: number; id?: number; meta?: number;
  // SpawnMob / SpawnPlayer
  mobKind?: number;
  // DestroyEntities
  ids?: number[];
  // UpdateHealth
  health?: number; food?: number;
  // SoundEffect
  name?: string; volume?: number; pitch?: number;
  // unmodeled packets
  rawId?: number;
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

/** An outbound packet for `recraft.sendPacket` (requires the `inject_packet`
 *  capability). Handshake/login packets are intentionally unreachable. */
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

interface Recraft {
  // ---- event subscription ----
  onTick(cb: () => void): void;
  onFrame(cb: () => void): void;
  onLoad(cb: () => void): void;
  /** Return `true` to consume the key (suppress default gameplay handling). */
  onKey(cb: (e: KeyEvent) => boolean | void): void;
  onChat(cb: (e: ChatEvent) => void): void;
  onBlockChange(cb: (e: BlockChangeEvent) => void): void;
  onChunkLoad(cb: (e: ChunkEvent) => void): void;
  onChunkUnload(cb: (e: ChunkEvent) => void): void;
  onEntitySpawn(cb: (e: EntitySpawnEvent) => void): void;
  onEntityRemove(cb: (e: EntityRemoveEvent) => void): void;
  onPlayerHealth(cb: (e: PlayerHealthEvent) => void): void;
  /** Subscribe to a raw clientbound packet by type. Return `false` to drop it. */
  onPacket(type: PacketType, cb: (p: Packet) => boolean | void): void;
  /** Register a per-frame HUD draw callback (use the `hud` global inside). */
  drawHud(cb: (ctx: HudContext) => void): void;

  // ---- commands ----
  sendChat(message: string): void;
  sendPacket(packet: OutPacket): void;
  log(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
  spawnParticle(kind: number, x: number, y: number, z: number, opts?: ParticleOpts): void;
  playSound(event: string, x: number, y: number, z: number, opts?: SoundOpts): void;

  // ---- read-views ----
  player(): PlayerView;
  blockAt(x: number, y: number, z: number): BlockView;
  entities(): EntityView[];
  worldTime(): number;
  dimension(): number;

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
  blockOutline(on: boolean): void;
  chunkBorders(on: boolean): void;
  entityBox(filter: "players" | "mobs" | "items" | "", color: Color, on?: boolean): void;
  nametagScale(scale: number): void;
  particleDensity(scale: number): void;
}

interface Hud {
  rect(x: number, y: number, w: number, h: number, color: Color): void;
  text(x: number, y: number, text: string, opts?: TextOpts): void;
  itemIcon(x: number, y: number, itemId: number, opts?: IconOpts): void;
  blockItem(x: number, y: number, blockId: number, meta: number, opts?: IconOpts): void;
}

declare const recraft: Recraft;
declare const hud: Hud;
declare const console: {
  log(...args: unknown[]): void;
  info(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
};
