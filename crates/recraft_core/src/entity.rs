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
            aabb: Aabb::player_at(position),
        }
    }

    pub fn sync_aabb_to_position(&mut self) {
        self.aabb = Aabb::player_at(self.position);
    }
}
