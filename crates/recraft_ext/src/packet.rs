//! Stable packet projections.
//!
//! [`PacketView`] is what mods observe at the clientbound/serverbound hooks;
//! [`PacketBuild`] is what a mod hands back to inject an outbound packet. Both
//! are deliberately *projections* of the internal `recraft_protocol` enums — the
//! host (`recraft_app`) converts in both directions — so the public surface
//! never leaks `SlotItem`/`NbtCompound`/wire-id details and stays stable across
//! protocol-internal refactors.
//!
//! [`PacketType`] is a direction-tagged stable id space, NOT the vanilla wire id
//! (wire ids collide across directions: 0x00 is both clientbound KeepAlive and
//! serverbound Handshake). Mods filter on `PacketType`; `raw_id` is exposed only
//! for diagnostics.

/// Stable, direction-tagged packet type id. Used by `onPacket(type, cb)` filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketType {
    // ---- clientbound (server -> client) ----
    KeepAlive,
    JoinGame,
    Respawn,
    ChatMessage,
    BlockChange,
    MultiBlockChange,
    ChunkData,
    ChunkBulk,
    SpawnPlayer,
    SpawnMob,
    SpawnObject,
    SpawnExperienceOrb,
    EntityMove,
    EntityTeleport,
    EntityVelocity,
    DestroyEntities,
    EntityMetadata,
    PlayerPositionLook,
    UpdateHealth,
    SetExperience,
    SetSlot,
    WindowItems,
    HeldItemChange,
    SoundEffect,
    SpawnParticle,
    Effect,
    BlockAction,
    TimeUpdate,
    Disconnect,
    /// Any clientbound packet recraft does not model (`raw_id` carries the id).
    ClientboundOther,

    // ---- serverbound (client -> server) ----
    SbChatMessage,
    SbPlayerPosition,
    SbPlayerLook,
    SbPlayerPositionLook,
    SbPlayerDigging,
    SbPlayerBlockPlacement,
    SbHeldItemChange,
    SbAnimation,
    SbUseEntity,
    SbEntityAction,
    SbKeepAlive,
    /// Any serverbound packet not individually projected.
    ServerboundOther,
}

impl PacketType {
    pub fn is_clientbound(self) -> bool {
        // Everything before the serverbound block is clientbound.
        !matches!(
            self,
            PacketType::SbChatMessage
                | PacketType::SbPlayerPosition
                | PacketType::SbPlayerLook
                | PacketType::SbPlayerPositionLook
                | PacketType::SbPlayerDigging
                | PacketType::SbPlayerBlockPlacement
                | PacketType::SbHeldItemChange
                | PacketType::SbAnimation
                | PacketType::SbUseEntity
                | PacketType::SbEntityAction
                | PacketType::SbKeepAlive
                | PacketType::ServerboundOther
        )
    }
}

/// A projected view of a packet at a hook point. Carries the stable
/// [`PacketType`] for every packet plus decoded fields for the mod-relevant
/// subset; unmodeled packets surface as `Other` with their stable type and wire
/// id so a mod can still observe/drop them by id.
#[derive(Debug, Clone, PartialEq)]
pub enum PacketView {
    // ---- clientbound projections ----
    KeepAlive {
        id: i32,
    },
    Chat {
        text: String,
        /// 0/1 = chat box, 2 = action bar.
        position: i8,
        json: String,
    },
    BlockChange {
        x: i32,
        y: i32,
        z: i32,
        id: u16,
        meta: u8,
    },
    SpawnMob {
        id: i32,
        kind: u8,
        x: f64,
        y: f64,
        z: f64,
    },
    SpawnPlayer {
        id: i32,
        x: f64,
        y: f64,
        z: f64,
    },
    DestroyEntities {
        ids: Vec<i32>,
    },
    UpdateHealth {
        health: f32,
        food: i32,
    },
    SoundEffect {
        name: String,
        x: f64,
        y: f64,
        z: f64,
        volume: f32,
        pitch: u8,
    },

    // ---- serverbound projections ----
    OutChat {
        message: String,
    },
    OutPlayerPosition {
        x: f64,
        y: f64,
        z: f64,
        on_ground: bool,
    },
    OutPlayerDigging {
        status: u8,
        x: i32,
        y: i32,
        z: i32,
        face: u8,
    },

    /// Catch-all carrying the stable type and the vanilla wire id.
    Other {
        ty: PacketType,
        raw_id: i32,
    },
}

impl PacketView {
    pub fn ty(&self) -> PacketType {
        match self {
            PacketView::KeepAlive { .. } => PacketType::KeepAlive,
            PacketView::Chat { .. } => PacketType::ChatMessage,
            PacketView::BlockChange { .. } => PacketType::BlockChange,
            PacketView::SpawnMob { .. } => PacketType::SpawnMob,
            PacketView::SpawnPlayer { .. } => PacketType::SpawnPlayer,
            PacketView::DestroyEntities { .. } => PacketType::DestroyEntities,
            PacketView::UpdateHealth { .. } => PacketType::UpdateHealth,
            PacketView::SoundEffect { .. } => PacketType::SoundEffect,
            PacketView::OutChat { .. } => PacketType::SbChatMessage,
            PacketView::OutPlayerPosition { .. } => PacketType::SbPlayerPosition,
            PacketView::OutPlayerDigging { .. } => PacketType::SbPlayerDigging,
            PacketView::Other { ty, .. } => *ty,
        }
    }
}

/// A mod-built outbound packet, whitelisted to the play-state packets that are
/// safe for a mod to send. The host maps these to `ServerboundPacket`; there is
/// deliberately no path to send Handshake/LoginStart/ClientSettings (would
/// corrupt the connection state machine).
#[derive(Debug, Clone, PartialEq)]
pub enum PacketBuild {
    Chat(String),
    PlayerPosition {
        x: f64,
        y: f64,
        z: f64,
        on_ground: bool,
    },
    PlayerLook {
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },
    HeldItemChange {
        slot: i16,
    },
    SwingArm,
    PlayerDigging {
        status: u8,
        x: i32,
        y: i32,
        z: i32,
        face: u8,
    },
}
