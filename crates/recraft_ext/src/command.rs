//! Commands issued by extensions back to the host, collected in the
//! [`crate::ExtManager`] command queue and drained by the host each tick/frame.

use crate::packet::PacketBuild;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

/// A preset, host-implemented render toggle/parameter (the JS layer's "controlled
/// rendering" surface — a closed, enumerated set; adding one means changing host
/// code). The Native layer bypasses this with arbitrary render hooks.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderPreset {
    /// Static per-block tint override applied during meshing (`None` meta = all metas).
    BlockTint {
        block_id: u16,
        meta: Option<u8>,
        color: [f32; 3],
    },
    Fullbright(bool),
    ChunkBorders(bool),
    /// ESP-style entity box for entities matching `kind` (`None` = all).
    EntityBox {
        kind: Option<EntityFilter>,
        color: [f32; 3],
        enabled: bool,
    },
    NametagScale(f32),
    ParticleDensity(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityFilter {
    Players,
    Mobs,
    Items,
}

/// A command from a mod to the host. The host drains the queue each tick and
/// applies each command on the main thread.
#[derive(Debug, Clone)]
pub enum ExtCommand {
    /// Inject a serverbound packet (high privilege; needs `inject_packet`).
    SendServerbound(PacketBuild),
    /// Send a chat message / command (maps to serverbound ChatMessage).
    Chat(String),
    /// Log through the host logger, tagged with the originating mod id.
    Log(LogLevel, String),
    /// Spawn a built-in particle (vanilla 1.8 particle id semantics).
    SpawnParticle {
        kind: i32,
        x: f64,
        y: f64,
        z: f64,
        ox: f32,
        oy: f32,
        oz: f32,
        speed: f32,
        count: i32,
    },
    /// Play a built-in sound by its `sounds.json` event key.
    PlaySound {
        event: String,
        x: f64,
        y: f64,
        z: f64,
        volume: f32,
        pitch: f32,
    },
    /// Apply a preset render modification (closed set; see [`RenderPreset`]).
    Render(RenderPreset),
    /// Replace the native render-hook geometry. Each vertex is
    /// `[x, y, z, r, g, b, a, u, v]` (world position, RGBA, entity-atlas UV).
    /// Native-only; replaces the previous submission, empty clears it.
    SubmitGeometry {
        vertices: Vec<[f32; 9]>,
        indices: Vec<u32>,
    },
    /// Register a content-mod block (full cube). Only takes effect in a
    /// recraft-authoritative world; `texture` reuses a vanilla base name until
    /// per-mod texture registration exists.
    RegisterBlock {
        id: u16,
        texture: String,
        opaque: bool,
        alpha: f32,
        luminance: u8,
        tint: Option<[f32; 3]>,
    },

    // ---- world / player interactions (mod -> host -> server) ----
    /// Place the held item against a block face (C08 PlayerBlockPlacement).
    PlaceBlock {
        x: i32,
        y: i32,
        z: i32,
        face: u8,
        cursor: [u8; 3],
    },
    /// Raw block digging (C07): status 0 start, 1 cancel, 2 finish, 3 drop-stack,
    /// 4 drop-item, 5 release-use.
    Digging {
        status: u8,
        x: i32,
        y: i32,
        z: i32,
        face: u8,
    },
    /// Attack an entity by id (C02 UseEntity Attack + swing).
    AttackEntity { id: i32 },
    /// Interact with / right-click an entity (`at` = hit point for InteractAt).
    InteractEntity { id: i32, at: Option<[f32; 3]> },
    /// Click a slot in the open window (C0E ClickWindow). `mode`/`button` are the
    /// vanilla inventory-action codes.
    ContainerClick { slot: i16, button: i8, mode: i8 },
    /// Close the open window (C0D CloseWindow).
    ContainerClose,
    /// Open the player inventory window.
    OpenInventory,
    /// Select a hotbar slot 0..8 (C09 HeldItemChange).
    SelectSlot { slot: i32 },
    /// Swing the arm (C0A Animation).
    SwingArm,
    /// Use the held item with no target (C08 at -1,255,-1 — eat/draw bow/block).
    UseItem,
    /// Set the look the *server* sees. `silent` keeps the camera/view unchanged
    /// (the rotation only rides the next movement packet) — the pre-event style.
    SetRotation { yaw: f32, pitch: f32, silent: bool },
    /// Clear a silent look override (resume sending the real camera rotation).
    ClearSilentRotation,
    /// Persist this mod's config JSON to `<mod dir>/config.json`.
    SaveConfig { dir: String, json: String },
}
