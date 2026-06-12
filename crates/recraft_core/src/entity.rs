use glam::DVec3;

use crate::mc_math::wrap_degrees;
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
    /// Position at the start of the current tick, for render interpolation.
    pub previous_position: DVec3,
    /// Latest server-reported target (vanilla `newPosX/newPosY/newPosZ`);
    /// remote entities lerp toward it over `position_increments` ticks.
    pub server_position: DVec3,
    pub server_yaw: f32,
    pub server_pitch: f32,
    /// Remaining lerp steps toward the server target (vanilla
    /// `newPosRotationIncrements`; movement packets arm 3).
    pub position_increments: u32,
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
            previous_position: position,
            server_position: position,
            server_yaw: 0.0,
            server_pitch: 0.0,
            position_increments: 0,
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
            previous_position: position,
            server_position: position,
            server_yaw: yaw,
            server_pitch: pitch,
            position_increments: 0,
        };
        entity.sync_aabb_to_position();
        entity
    }

    /// Queue a server movement update (vanilla `setPositionAndRotation2`):
    /// the entity lerps toward it over the next 3 ticks instead of snapping.
    pub fn set_server_target(&mut self, position: DVec3, yaw: f32, pitch: f32) {
        self.server_position = position;
        self.server_yaw = yaw;
        self.server_pitch = pitch;
        self.position_increments = 3;
    }

    /// Advance one interpolation tick toward the server target (vanilla
    /// `newPosRotationIncrements` stepping in `onLivingUpdate`).
    pub fn tick_interpolation(&mut self) {
        self.previous_position = self.position;
        if self.position_increments == 0 {
            return;
        }
        let steps = self.position_increments as f64;
        self.position += (self.server_position - self.position) / steps;
        let steps = self.position_increments as f32;
        self.yaw += wrap_degrees(self.server_yaw - self.yaw) / steps;
        self.pitch += (self.server_pitch - self.pitch) / steps;
        self.position_increments -= 1;
        self.sync_aabb_to_position();
    }

    /// Position interpolated between the previous and current tick, for
    /// frame-rate-independent rendering (vanilla partialTicks).
    pub fn render_position(&self, alpha: f64) -> DVec3 {
        self.previous_position
            .lerp(self.position, alpha.clamp(0.0, 1.0))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mob_at(position: DVec3) -> EntityState {
        EntityState::new_remote(EntityId(1), EntityKind::Mob(54), position, 0.0, 0.0)
    }

    #[test]
    fn remote_entity_lerps_to_the_server_target_over_three_ticks() {
        let mut entity = mob_at(DVec3::ZERO);
        entity.set_server_target(DVec3::new(3.0, 0.0, 0.0), 90.0, 30.0);
        assert_eq!(entity.position.x, 0.0, "packets must not snap the position");
        entity.tick_interpolation();
        assert!((entity.position.x - 1.0).abs() < 1.0e-9);
        entity.tick_interpolation();
        assert!((entity.position.x - 2.0).abs() < 1.0e-9);
        entity.tick_interpolation();
        assert!((entity.position.x - 3.0).abs() < 1.0e-9);
        assert!((entity.yaw - 90.0).abs() < 1.0e-4);
        assert!((entity.pitch - 30.0).abs() < 1.0e-4);
        // Settled: further ticks hold position.
        entity.tick_interpolation();
        assert!((entity.position.x - 3.0).abs() < 1.0e-9);
        // The collision/targeting box follows the interpolated position.
        assert!((entity.aabb.min.x - (3.0 - 0.3)).abs() < 1.0e-9);
    }

    #[test]
    fn render_position_blends_between_ticks() {
        let mut entity = mob_at(DVec3::ZERO);
        entity.set_server_target(DVec3::new(3.0, 0.0, 0.0), 0.0, 0.0);
        entity.tick_interpolation(); // previous = 0, current = 1
        let mid = entity.render_position(0.5);
        assert!((mid.x - 0.5).abs() < 1.0e-9);
    }

    #[test]
    fn yaw_interpolation_takes_the_short_way_around() {
        let mut entity = mob_at(DVec3::ZERO);
        entity.yaw = 170.0;
        entity.set_server_target(DVec3::ZERO, -170.0, 0.0);
        entity.tick_interpolation();
        // Shortest path is +20° across the 180 boundary, not -340°.
        assert!(
            entity.yaw > 170.0 && entity.yaw < 180.0,
            "yaw went the long way: {}",
            entity.yaw
        );
    }
}
