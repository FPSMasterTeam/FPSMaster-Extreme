//! Projection layer between fpsmaster's internal types (`fpsmaster_protocol` packets,
//! `fpsmaster_core`/`GameState` state) and the stable `fpsmaster_ext` vocabulary.
//!
//! Everything an extension sees or sends crosses through here, so the public
//! extension surface never leaks internal protocol/state types. The host calls
//! these at the four hook seams (clientbound dispatch, event derivation, outbound
//! build) and exposes [`GameViews`] as the live read-view.

use fpsmaster_core::{EntityId, EntityKind, RenderShape};
use fpsmaster_ext::{
    BlockView, CapabilitiesView, ContainerInfo, EffectView, EntityKindView, EntityView, ExtEvent,
    ItemView, PacketBuild, PacketType, PacketView, PlayerView, ReadViews,
};
use fpsmaster_protocol::v1_8_9::packets::{
    ClientboundPlayPacket, DiggingStatus, ServerboundPacket, SlotItem,
};

use crate::chat::flatten_chat_json;
use crate::container::WindowKind;
use crate::game::GameState;

/// Read-only adapter exposing the live `GameState` to extensions through the
/// stable `fpsmaster_ext::ReadViews` trait. Holds a shared borrow, so every hook
/// sees a consistent snapshot for the duration of one dispatch call.
pub struct GameViews<'a>(pub &'a GameState);

impl ReadViews for GameViews<'_> {
    fn player(&self) -> PlayerView {
        let gs = self.0;
        let pos = gs.player_position();
        let vel = gs.player_velocity();
        PlayerView {
            x: pos.x,
            y: pos.y,
            z: pos.z,
            yaw: gs.player_yaw(),
            pitch: gs.player_pitch(),
            vx: vel.x,
            vy: vel.y,
            vz: vel.z,
            on_ground: gs.player_on_ground(),
            health: gs.health(),
            food: gs.food(),
            sneaking: gs.player_sneaking(),
            sprinting: gs.player_sprinting(),
        }
    }

    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockView {
        let b = self.0.world.block_at(x, y, z);
        BlockView {
            id: b.id,
            meta: b.meta,
            is_air: b.is_air(),
            luminance: b.luminance(),
            opaque: b.is_opaque_cube(),
            shape: render_shape_name(b.render_shape()),
        }
    }

    fn entities(&self) -> Vec<EntityView> {
        let gs = self.0;
        let player_id = gs.player_entity_id();
        gs.world
            .entities()
            .filter(|e| e.id != player_id)
            .map(entity_to_view)
            .collect()
    }

    fn entity(&self, id: i32) -> Option<EntityView> {
        self.0.world.entity(EntityId(id)).map(entity_to_view)
    }

    fn world_time(&self) -> i64 {
        self.0.world_time_ticks()
    }

    fn dimension(&self) -> i32 {
        self.0.dimension()
    }

    fn loaded_chunk_count(&self) -> usize {
        self.0.loaded_chunk_count()
    }

    fn held_item(&self) -> Option<ItemView> {
        self.0.held_item().map(slot_to_item)
    }

    fn inventory(&self) -> Vec<Option<ItemView>> {
        self.0
            .inventory_slots()
            .iter()
            .map(|s| s.as_ref().map(slot_to_item))
            .collect()
    }

    fn selected_slot(&self) -> i32 {
        self.0.selected_slot()
    }

    fn capabilities(&self) -> CapabilitiesView {
        let c = self.0.capabilities();
        CapabilitiesView {
            invulnerable: c.invulnerable,
            flying: c.flying,
            allow_flying: c.allow_flying,
            creative: c.creative,
            fly_speed: c.fly_speed,
            walk_speed: c.walk_speed,
        }
    }

    fn effects(&self) -> Vec<EffectView> {
        self.0
            .active_effects()
            .into_iter()
            .map(|(id, amplifier, duration)| EffectView {
                id,
                amplifier,
                duration,
            })
            .collect()
    }

    fn xp(&self) -> (f32, i32) {
        (self.0.xp_bar(), self.0.xp_level())
    }

    fn open_container(&self) -> Option<ContainerInfo> {
        self.0.open_container().map(|c| ContainerInfo {
            window_id: c.window_id as i32,
            kind: window_kind_name(&c.kind).to_string(),
            size: c.slots().len(),
        })
    }

    fn connected(&self) -> bool {
        self.0.can_send_movement_packets()
    }
}

/// Project an internal inventory stack into the stable [`ItemView`].
fn slot_to_item(s: &SlotItem) -> ItemView {
    ItemView {
        id: s.id,
        count: s.count,
        damage: s.damage,
    }
}

/// Stable render-shape name for a block (extension block read-view).
fn render_shape_name(shape: RenderShape) -> &'static str {
    match shape {
        RenderShape::None => "none",
        RenderShape::Cube => "cube",
        RenderShape::Cross => "cross",
        RenderShape::Rail => "rail",
        RenderShape::Ladder => "ladder",
        RenderShape::Boxes => "boxes",
        RenderShape::Door => "door",
        RenderShape::Piston => "piston",
        RenderShape::PistonHead => "piston_head",
        RenderShape::Torch => "torch",
        RenderShape::Fluid => "fluid",
        RenderShape::Fire => "fire",
        RenderShape::Bed => "bed",
    }
}

/// Stable window-kind name for the open container (extension read-view).
fn window_kind_name(kind: &WindowKind) -> &'static str {
    match kind {
        WindowKind::Player => "player",
        WindowKind::Chest(_) => "chest",
        WindowKind::Dispenser => "dispenser",
        WindowKind::Hopper => "hopper",
        WindowKind::Furnace => "furnace",
        WindowKind::Crafting => "crafting",
        WindowKind::Brewing => "brewing",
        WindowKind::Enchant => "enchant",
        WindowKind::Anvil => "anvil",
        WindowKind::Beacon => "beacon",
        WindowKind::Villager => "villager",
    }
}

/// Convert a `fpsmaster_core` entity into the stable `EntityView`. The local
/// player row is filtered out by `entities()`; `LocalPlayer` is still mapped to
/// `Player` so a direct `entity(id)` lookup of the local id reads sensibly.
fn entity_to_view(e: &fpsmaster_core::EntityState) -> EntityView {
    let kind = match e.kind {
        EntityKind::LocalPlayer | EntityKind::RemotePlayer => EntityKindView::Player,
        EntityKind::Mob(id) => EntityKindView::Mob(id),
        EntityKind::Object(id) => EntityKindView::Object(id),
        EntityKind::ExperienceOrb => EntityKindView::ExperienceOrb,
    };
    EntityView {
        id: e.id.0,
        kind,
        x: e.position.x,
        y: e.position.y,
        z: e.position.z,
        yaw: e.yaw,
        pitch: e.pitch,
        on_ground: e.on_ground,
        name: e.custom_name.clone(),
        health: e.health,
    }
}

/// Classify a clientbound packet into the stable [`PacketType`] id space.
/// Variants fpsmaster does not individually model fall through to
/// [`PacketType::ClientboundOther`].
pub fn clientbound_type(p: &ClientboundPlayPacket) -> PacketType {
    match p {
        ClientboundPlayPacket::KeepAlive { .. } => PacketType::KeepAlive,
        ClientboundPlayPacket::JoinGame { .. } => PacketType::JoinGame,
        ClientboundPlayPacket::Respawn { .. } => PacketType::Respawn,
        ClientboundPlayPacket::ChatMessage { .. } => PacketType::ChatMessage,
        ClientboundPlayPacket::BlockChange { .. } => PacketType::BlockChange,
        ClientboundPlayPacket::MultiBlockChange { .. } => PacketType::MultiBlockChange,
        ClientboundPlayPacket::ChunkData { .. } => PacketType::ChunkData,
        ClientboundPlayPacket::ChunkBulk { .. } => PacketType::ChunkBulk,
        ClientboundPlayPacket::SpawnPlayer { .. } => PacketType::SpawnPlayer,
        ClientboundPlayPacket::SpawnMob { .. } => PacketType::SpawnMob,
        ClientboundPlayPacket::SpawnObject { .. } => PacketType::SpawnObject,
        ClientboundPlayPacket::SpawnExperienceOrb { .. } => PacketType::SpawnExperienceOrb,
        // fpsmaster splits movement into relative/look/teleport; the three
        // relative-style moves share the stable `EntityMove` id.
        ClientboundPlayPacket::EntityRelativeMove { .. }
        | ClientboundPlayPacket::EntityLookMove { .. }
        | ClientboundPlayPacket::EntityLook { .. } => PacketType::EntityMove,
        ClientboundPlayPacket::EntityTeleport { .. } => PacketType::EntityTeleport,
        ClientboundPlayPacket::EntityVelocity { .. } => PacketType::EntityVelocity,
        ClientboundPlayPacket::DestroyEntities { .. } => PacketType::DestroyEntities,
        ClientboundPlayPacket::EntityMetadata { .. } => PacketType::EntityMetadata,
        ClientboundPlayPacket::PlayerPositionLook { .. } => PacketType::PlayerPositionLook,
        ClientboundPlayPacket::UpdateHealth { .. } => PacketType::UpdateHealth,
        ClientboundPlayPacket::SetExperience { .. } => PacketType::SetExperience,
        ClientboundPlayPacket::SetSlot { .. } => PacketType::SetSlot,
        ClientboundPlayPacket::WindowItems { .. } => PacketType::WindowItems,
        ClientboundPlayPacket::HeldItemChange { .. } => PacketType::HeldItemChange,
        ClientboundPlayPacket::SoundEffect { .. } => PacketType::SoundEffect,
        ClientboundPlayPacket::SpawnParticle { .. } => PacketType::SpawnParticle,
        ClientboundPlayPacket::Effect { .. } => PacketType::Effect,
        ClientboundPlayPacket::BlockAction { .. } => PacketType::BlockAction,
        ClientboundPlayPacket::TimeUpdate { .. } => PacketType::TimeUpdate,
        ClientboundPlayPacket::Disconnect { .. } => PacketType::Disconnect,
        _ => PacketType::ClientboundOther,
    }
}

/// Project a clientbound packet into the stable [`PacketView`] mods observe.
/// Only the mod-relevant subset carries decoded fields; everything else surfaces
/// as [`PacketView::Other`].
pub fn clientbound_view(p: &ClientboundPlayPacket) -> PacketView {
    match p {
        ClientboundPlayPacket::KeepAlive { id } => PacketView::KeepAlive { id: *id },
        ClientboundPlayPacket::ChatMessage { json, position } => PacketView::Chat {
            text: flatten_chat_json(json),
            position: *position,
            json: json.clone(),
        },
        ClientboundPlayPacket::BlockChange { x, y, z, id, meta } => PacketView::BlockChange {
            x: *x,
            y: *y,
            z: *z,
            id: *id,
            meta: *meta,
        },
        ClientboundPlayPacket::SpawnMob {
            entity_id,
            kind,
            x,
            y,
            z,
            ..
        } => PacketView::SpawnMob {
            id: *entity_id,
            kind: *kind,
            x: *x,
            y: *y,
            z: *z,
        },
        ClientboundPlayPacket::SpawnPlayer {
            entity_id, x, y, z, ..
        } => PacketView::SpawnPlayer {
            id: *entity_id,
            x: *x,
            y: *y,
            z: *z,
        },
        ClientboundPlayPacket::DestroyEntities { entity_ids } => PacketView::DestroyEntities {
            ids: entity_ids.clone(),
        },
        ClientboundPlayPacket::UpdateHealth { health, food, .. } => PacketView::UpdateHealth {
            health: *health,
            food: *food,
        },
        ClientboundPlayPacket::SoundEffect {
            name,
            x,
            y,
            z,
            volume,
            pitch,
        } => PacketView::SoundEffect {
            name: name.clone(),
            x: *x,
            y: *y,
            z: *z,
            volume: *volume,
            pitch: *pitch,
        },
        other => PacketView::Other {
            ty: clientbound_type(other),
            raw_id: 0,
        },
    }
}

/// Derive the higher-level [`ExtEvent`] notifications a clientbound packet
/// produces. Packets with no derived event yield an empty vec.
pub fn derive_events(p: &ClientboundPlayPacket) -> Vec<ExtEvent> {
    match p {
        ClientboundPlayPacket::ChatMessage { json, position } => vec![ExtEvent::Chat {
            text: flatten_chat_json(json),
            position: *position,
            json: json.clone(),
        }],
        ClientboundPlayPacket::BlockChange { x, y, z, id, meta } => vec![ExtEvent::BlockChange {
            x: *x,
            y: *y,
            z: *z,
            id: *id,
            meta: *meta,
        }],
        ClientboundPlayPacket::MultiBlockChange {
            chunk_x,
            chunk_z,
            changes,
        } => changes
            .iter()
            .map(|rec| ExtEvent::BlockChange {
                x: chunk_x * 16 + rec.x as i32,
                y: rec.y as i32,
                z: chunk_z * 16 + rec.z as i32,
                id: rec.id,
                meta: rec.meta,
            })
            .collect(),
        ClientboundPlayPacket::SpawnPlayer {
            entity_id, x, y, z, ..
        } => vec![ExtEvent::EntitySpawn {
            id: *entity_id,
            kind: EntityKindView::Player,
            x: *x,
            y: *y,
            z: *z,
        }],
        ClientboundPlayPacket::SpawnMob {
            entity_id,
            kind,
            x,
            y,
            z,
            ..
        } => vec![ExtEvent::EntitySpawn {
            id: *entity_id,
            kind: EntityKindView::Mob(*kind),
            x: *x,
            y: *y,
            z: *z,
        }],
        ClientboundPlayPacket::SpawnObject {
            entity_id,
            kind,
            x,
            y,
            z,
            ..
        } => vec![ExtEvent::EntitySpawn {
            id: *entity_id,
            // SpawnObject.kind is i8 on the wire; EntityKindView::Object wraps u8.
            kind: EntityKindView::Object(*kind as u8),
            x: *x,
            y: *y,
            z: *z,
        }],
        ClientboundPlayPacket::SpawnExperienceOrb {
            entity_id, x, y, z, ..
        } => vec![ExtEvent::EntitySpawn {
            id: *entity_id,
            kind: EntityKindView::ExperienceOrb,
            x: *x,
            y: *y,
            z: *z,
        }],
        ClientboundPlayPacket::DestroyEntities { entity_ids } => entity_ids
            .iter()
            .map(|id| ExtEvent::EntityRemove { id: *id })
            .collect(),
        ClientboundPlayPacket::UpdateHealth { health, food, .. } => vec![ExtEvent::PlayerHealth {
            health: *health,
            food: *food,
        }],
        _ => Vec::new(),
    }
}

/// Map a mod-built [`PacketBuild`] to the internal [`ServerboundPacket`] the
/// network thread sends. Only play-safe packets are reachable from `PacketBuild`.
pub fn build_to_serverbound(b: PacketBuild) -> ServerboundPacket {
    match b {
        PacketBuild::Chat(message) => ServerboundPacket::ChatMessage { message },
        PacketBuild::PlayerPosition { x, y, z, on_ground } => {
            ServerboundPacket::PlayerPosition { x, y, z, on_ground }
        }
        PacketBuild::PlayerLook {
            yaw,
            pitch,
            on_ground,
        } => ServerboundPacket::PlayerLook {
            yaw,
            pitch,
            on_ground,
        },
        PacketBuild::HeldItemChange { slot } => ServerboundPacket::HeldItemChange { slot },
        PacketBuild::SwingArm => ServerboundPacket::SwingArm,
        PacketBuild::PlayerDigging { status, x, y, z, face } => ServerboundPacket::PlayerDigging {
            status: digging_status_from_u8(status),
            x,
            y,
            z,
            face,
        },
    }
}

/// Classify a serverbound packet into the stable [`PacketType`] id space.
pub fn serverbound_type(p: &ServerboundPacket) -> PacketType {
    match p {
        ServerboundPacket::ChatMessage { .. } => PacketType::SbChatMessage,
        ServerboundPacket::PlayerPosition { .. } => PacketType::SbPlayerPosition,
        ServerboundPacket::PlayerLook { .. } => PacketType::SbPlayerLook,
        ServerboundPacket::PlayerPositionLook { .. } => PacketType::SbPlayerPositionLook,
        ServerboundPacket::PlayerDigging { .. } => PacketType::SbPlayerDigging,
        ServerboundPacket::PlayerBlockPlacement { .. } => PacketType::SbPlayerBlockPlacement,
        ServerboundPacket::HeldItemChange { .. } => PacketType::SbHeldItemChange,
        ServerboundPacket::SwingArm => PacketType::SbAnimation,
        ServerboundPacket::UseEntity { .. } => PacketType::SbUseEntity,
        ServerboundPacket::EntityAction { .. } => PacketType::SbEntityAction,
        ServerboundPacket::KeepAlive { .. } => PacketType::SbKeepAlive,
        _ => PacketType::ServerboundOther,
    }
}

/// Project an outgoing packet into the stable [`PacketView`] a mod observes in
/// its `on_serverbound` (pre-send) hook. Only the mod-relevant subset carries
/// decoded fields; everything else surfaces as [`PacketView::Other`].
pub fn serverbound_view(p: &ServerboundPacket) -> PacketView {
    match p {
        ServerboundPacket::ChatMessage { message } => PacketView::OutChat {
            message: message.clone(),
        },
        ServerboundPacket::PlayerPosition { x, y, z, on_ground } => PacketView::OutPlayerPosition {
            x: *x,
            y: *y,
            z: *z,
            on_ground: *on_ground,
        },
        ServerboundPacket::PlayerPositionLook {
            x, y, z, on_ground, ..
        } => PacketView::OutPlayerPosition {
            x: *x,
            y: *y,
            z: *z,
            on_ground: *on_ground,
        },
        ServerboundPacket::PlayerDigging {
            status,
            x,
            y,
            z,
            face,
        } => PacketView::OutPlayerDigging {
            status: *status as u8,
            x: *x,
            y: *y,
            z: *z,
            face: *face,
        },
        other => PacketView::Other {
            ty: serverbound_type(other),
            raw_id: 0,
        },
    }
}

/// Map a stable digging-status byte to the internal [`DiggingStatus`]. Unknown
/// values default to `StartDestroy`.
fn digging_status_from_u8(status: u8) -> DiggingStatus {
    match status {
        1 => DiggingStatus::CancelDestroy,
        2 => DiggingStatus::FinishDestroy,
        3 => DiggingStatus::DropItemStack,
        4 => DiggingStatus::DropItem,
        5 => DiggingStatus::ReleaseUseItem,
        _ => DiggingStatus::StartDestroy,
    }
}
