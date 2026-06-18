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
    BlockOutline(bool),
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
}
