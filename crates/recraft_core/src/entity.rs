use glam::DVec3;

use crate::physics::Aabb;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId(pub i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    LocalPlayer,
    RemotePlayer,
    Mob(u8),
    Object(u8),
}

#[derive(Debug, Clone)]
pub struct EntityState {
    pub id: EntityId,
    pub kind: EntityKind,
    pub position: DVec3,
    pub velocity: DVec3,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    /// Set by physics when an intended horizontal move was blocked this tick
    /// (vanilla `isCollidedHorizontally`); drives the sprint wall-cancel.
    pub collided_horizontally: bool,
    pub aabb: Aabb,
}

impl EntityState {
    pub fn new_local_player(id: EntityId, position: DVec3) -> Self {
        Self {
            id,
            kind: EntityKind::LocalPlayer,
            position,
            velocity: DVec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            collided_horizontally: false,
            aabb: Aabb::player_at(position),
        }
    }

    /// A server-tracked entity (other player, mob, or object). Its bounding box
    /// is sized by kind so attack/interact ray-casts can target it.
    pub fn new_remote(
        id: EntityId,
        kind: EntityKind,
        position: DVec3,
        yaw: f32,
        pitch: f32,
    ) -> Self {
        let mut entity = Self {
            id,
            kind,
            position,
            velocity: DVec3::ZERO,
            yaw,
            pitch,
            on_ground: false,
            collided_horizontally: false,
            aabb: Aabb::player_at(position),
        };
        entity.sync_aabb_to_position();
        entity
    }

    /// Half-width and height of this entity's bounding box, in blocks.
    pub fn size(&self) -> (f64, f64) {
        entity_size(self.kind)
    }

    pub fn sync_aabb_to_position(&mut self) {
        let (half_width, height) = entity_size(self.kind);
        self.aabb = Aabb::new(
            DVec3::new(
                self.position.x - half_width,
                self.position.y,
                self.position.z - half_width,
            ),
            DVec3::new(
                self.position.x + half_width,
                self.position.y + height,
                self.position.z + half_width,
            ),
        );
    }
}

/// Approximate (half-width, height) bounding sizes per entity kind. Mob sizes
/// are simplified to the common humanoid box; objects use a small cube.
fn entity_size(kind: EntityKind) -> (f64, f64) {
    match kind {
        EntityKind::LocalPlayer | EntityKind::RemotePlayer => (0.3, 1.8),
        EntityKind::Mob(_) => (0.3, 1.9),
        EntityKind::Object(_) => (0.125, 0.25),
    }
}
