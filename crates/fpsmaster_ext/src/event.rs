//! Derived, read-only notifications (host -> mod) and the packet [`Verdict`].

use crate::view::EntityKindView;

/// A mod's decision about an intercepted packet at a clientbound/serverbound
/// hook. `Modify` is reserved (the host does not yet rebuild a packet from a
/// projection) and currently behaves like `Pass`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Verdict {
    /// Let the packet through unchanged.
    #[default]
    Pass,
    /// Swallow the packet; the host acts as if it never arrived / was never sent.
    Drop,
}

impl Verdict {
    /// Combine verdicts from multiple mods: any `Drop` wins.
    pub fn merge(self, other: Verdict) -> Verdict {
        if self == Verdict::Drop || other == Verdict::Drop {
            Verdict::Drop
        } else {
            Verdict::Pass
        }
    }
}

/// Higher-level, already-projected notifications about game-state changes. These
/// are derived by the host at the relevant choke points and are pure
/// notifications (no verdict). Mods that only need "a block changed" subscribe
/// to these instead of decoding raw packets.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtEvent {
    BlockChange {
        x: i32,
        y: i32,
        z: i32,
        id: u16,
        meta: u8,
    },
    ChunkLoad {
        x: i32,
        z: i32,
    },
    ChunkUnload {
        x: i32,
        z: i32,
    },
    Chat {
        text: String,
        position: i8,
        json: String,
    },
    EntitySpawn {
        id: i32,
        kind: EntityKindView,
        x: f64,
        y: f64,
        z: f64,
    },
    EntityRemove {
        id: i32,
    },
    PlayerHealth {
        health: f32,
        food: i32,
    },
    Disconnected {
        reason: String,
    },
}
