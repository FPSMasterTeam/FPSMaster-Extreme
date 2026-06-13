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
    /// Vanilla `Entity.isInWeb`: set when the post-move box overlapped a cobweb,
    /// consumed at the start of the next move to slow the entity to a crawl.
    pub in_web: bool,
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
    /// Body yaw/pitch at the start of the tick, for per-frame render
    /// interpolation (the body uses these like `previous_position`).
    pub prev_yaw: f32,
    pub prev_pitch: f32,
    /// Head yaw (vanilla `rotationYawHead`), tracked separately from the body
    /// so the head can turn independently; smoothed toward `server_head_yaw`.
    pub head_yaw: f32,
    pub prev_head_yaw: f32,
    pub server_head_yaw: f32,
    /// Remaining lerp steps for the head toward `server_head_yaw` (vanilla steps
    /// the head over its own increment counter, like the body's, so head and
    /// body turn in lockstep rather than the head trailing).
    pub head_increments: u32,
    /// Set once an EntityHeadLook arrives; until then the head follows the body.
    pub had_head_look: bool,
    /// Vanilla `limbSwing`/`limbSwingAmount`: the walk-cycle phase and its 0..1
    /// amplitude, driven by horizontal movement per tick.
    pub limb_swing: f32,
    pub limb_swing_amount: f32,
    pub prev_limb_swing_amount: f32,
    /// Vanilla arm-swing (attack) animation state, mirrored from the local
    /// player's: a 0..1 progress stepped over `SWING_DURATION` ticks.
    pub swing_progress: f32,
    pub prev_swing_progress: f32,
    pub swing_progress_int: i32,
    pub is_swinging: bool,
    /// Entity metadata flag (index 0 bit 0x02): drives the crouch pose.
    pub sneaking: bool,
    /// Ticks since the entity was first tracked, for the dropped-item bob/spin.
    pub age: u32,
}

/// Vanilla `getArmSwingAnimationEnd` with no haste: a swing runs 6 ticks.
const SWING_DURATION: i32 = 6;

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
            in_web: false,
            aabb: Aabb::player_at(position),
            previous_position: position,
            server_position: position,
            server_yaw: 0.0,
            server_pitch: 0.0,
            position_increments: 0,
            ..Self::anim_defaults(0.0)
        }
    }

    /// Zeroed animation/render-interpolation state for body yaw `yaw`.
    fn anim_defaults(yaw: f32) -> Self {
        // Only the animation fields matter; the positional fields are
        // overwritten by the caller's struct-update.
        Self {
            id: EntityId(0),
            kind: EntityKind::LocalPlayer,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            yaw,
            pitch: 0.0,
            on_ground: false,
            collided_horizontally: false,
            in_web: false,
            aabb: Aabb::player_at(DVec3::ZERO),
            previous_position: DVec3::ZERO,
            server_position: DVec3::ZERO,
            server_yaw: yaw,
            server_pitch: 0.0,
            position_increments: 0,
            prev_yaw: yaw,
            prev_pitch: 0.0,
            head_yaw: yaw,
            prev_head_yaw: yaw,
            server_head_yaw: yaw,
            head_increments: 0,
            had_head_look: false,
            limb_swing: 0.0,
            limb_swing_amount: 0.0,
            prev_limb_swing_amount: 0.0,
            swing_progress: 0.0,
            prev_swing_progress: 0.0,
            swing_progress_int: 0,
            is_swinging: false,
            sneaking: false,
            age: 0,
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
            in_web: false,
            aabb: Aabb::player_at(position),
            previous_position: position,
            server_position: position,
            server_yaw: yaw,
            server_pitch: pitch,
            position_increments: 0,
            prev_yaw: yaw,
            prev_pitch: pitch,
            head_yaw: yaw,
            prev_head_yaw: yaw,
            server_head_yaw: yaw,
            ..Self::anim_defaults(yaw)
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
        // Until an explicit head-look arrives, keep the head aligned with the
        // body so a turning mob doesn't render with a frozen head — and step it
        // over the same 3 ticks so head and body turn together (no lag).
        if !self.had_head_look {
            self.server_head_yaw = yaw;
            self.head_increments = 3;
        }
    }

    /// Apply an S19 EntityHeadLook: the head turns independently from now on,
    /// lerping over 3 ticks like vanilla `rotationYawHead`.
    pub fn set_head_yaw(&mut self, head_yaw: f32) {
        self.server_head_yaw = head_yaw;
        self.head_increments = 3;
        self.had_head_look = true;
    }

    /// Trigger the arm-swing animation (S0B Animation 0 from the server), the
    /// same restart gate vanilla uses so held swings flow into each other.
    pub fn start_swing(&mut self) {
        if !self.is_swinging
            || self.swing_progress_int >= SWING_DURATION / 2
            || self.swing_progress_int < 0
        {
            self.swing_progress_int = -1;
            self.is_swinging = true;
        }
    }

    /// Advance one interpolation tick toward the server target (vanilla
    /// `newPosRotationIncrements` stepping in `onLivingUpdate`), then advance the
    /// walk cycle, head turn and arm swing from the resulting motion.
    pub fn tick_interpolation(&mut self) {
        self.age = self.age.wrapping_add(1);
        self.previous_position = self.position;
        self.prev_yaw = self.yaw;
        self.prev_pitch = self.pitch;
        self.prev_head_yaw = self.head_yaw;
        if self.position_increments != 0 {
            let steps = self.position_increments as f64;
            self.position += (self.server_position - self.position) / steps;
            let steps = self.position_increments as f32;
            self.yaw += wrap_degrees(self.server_yaw - self.yaw) / steps;
            self.pitch += (self.server_pitch - self.pitch) / steps;
            self.position_increments -= 1;
            self.sync_aabb_to_position();
        }
        // Step the head over its own increment counter so it reaches the target
        // in the same number of ticks as the body (no per-turn head lag).
        if self.head_increments != 0 {
            let steps = self.head_increments as f32;
            self.head_yaw += wrap_degrees(self.server_head_yaw - self.head_yaw) / steps;
            self.head_increments -= 1;
        }
        // Walk cycle from horizontal movement this tick.
        self.prev_limb_swing_amount = self.limb_swing_amount;
        let dx = (self.position.x - self.previous_position.x) as f32;
        let dz = (self.position.z - self.previous_position.z) as f32;
        let speed = ((dx * dx + dz * dz).sqrt() * 4.0).min(1.0);
        self.limb_swing_amount += (speed - self.limb_swing_amount) * 0.4;
        self.limb_swing += self.limb_swing_amount;
        self.tick_swing();
    }

    /// Per-tick arm-swing stepping (vanilla `updateArmSwingProgress`).
    fn tick_swing(&mut self) {
        self.prev_swing_progress = self.swing_progress;
        if self.is_swinging {
            self.swing_progress_int += 1;
            if self.swing_progress_int >= SWING_DURATION {
                self.swing_progress_int = 0;
                self.is_swinging = false;
            }
        } else {
            self.swing_progress_int = 0;
        }
        self.swing_progress = self.swing_progress_int as f32 / SWING_DURATION as f32;
    }

    /// Body yaw interpolated for a render frame (vanilla partialTicks).
    pub fn render_yaw(&self, alpha: f32) -> f32 {
        self.prev_yaw + wrap_degrees(self.yaw - self.prev_yaw) * alpha.clamp(0.0, 1.0)
    }

    /// Head yaw interpolated for a render frame.
    pub fn render_head_yaw(&self, alpha: f32) -> f32 {
        self.prev_head_yaw + wrap_degrees(self.head_yaw - self.prev_head_yaw) * alpha.clamp(0.0, 1.0)
    }

    /// Pitch interpolated for a render frame.
    pub fn render_pitch(&self, alpha: f32) -> f32 {
        self.prev_pitch + (self.pitch - self.prev_pitch) * alpha.clamp(0.0, 1.0)
    }

    /// Walk-cycle phase and amplitude for a render frame (vanilla
    /// `limbSwing - limbSwingAmount * (1 - partialTicks)` and the lerped amount).
    pub fn render_limb_swing(&self, alpha: f32) -> (f32, f32) {
        let alpha = alpha.clamp(0.0, 1.0);
        let phase = self.limb_swing - self.limb_swing_amount * (1.0 - alpha);
        let amount = self.prev_limb_swing_amount
            + (self.limb_swing_amount - self.prev_limb_swing_amount) * alpha;
        (phase, amount)
    }

    /// Arm-swing progress for a render frame (vanilla `getSwingProgress` with
    /// its wrap-around handling).
    pub fn render_swing(&self, alpha: f32) -> f32 {
        let mut delta = self.swing_progress - self.prev_swing_progress;
        if delta < 0.0 {
            delta += 1.0;
        }
        self.prev_swing_progress + delta * alpha.clamp(0.0, 1.0)
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
    fn moving_accumulates_the_walk_cycle_then_it_decays_at_rest() {
        let mut entity = mob_at(DVec3::ZERO);
        // Walk a block per tick for several ticks.
        for step in 1..=5 {
            entity.set_server_target(DVec3::new(step as f64, 0.0, 0.0), 0.0, 0.0);
            entity.tick_interpolation();
        }
        assert!(
            entity.limb_swing_amount > 0.2,
            "walking should raise the swing amplitude: {}",
            entity.limb_swing_amount
        );
        assert!(entity.limb_swing > 0.0, "the walk phase should advance");
        // Standing still lets the amplitude decay back toward zero.
        let walking_amount = entity.limb_swing_amount;
        for _ in 0..20 {
            entity.tick_interpolation();
        }
        assert!(
            entity.limb_swing_amount < walking_amount * 0.2,
            "the swing should decay when stationary"
        );
    }

    #[test]
    fn head_follows_body_until_a_head_look_then_turns_independently() {
        let mut entity = mob_at(DVec3::ZERO);
        // No head-look yet: a body turn drags the head along in lockstep (head
        // and body share the target and step over the same 3 increments).
        entity.set_server_target(DVec3::ZERO, 90.0, 0.0);
        for _ in 0..3 {
            entity.tick_interpolation();
            assert!(
                (entity.head_yaw - entity.yaw).abs() < 1.0e-4,
                "head must turn in lockstep with the body: head {} vs body {}",
                entity.head_yaw,
                entity.yaw
            );
        }
        assert!((entity.head_yaw - 90.0).abs() < 1.0e-4, "head should reach the body target");

        // After an explicit head-look the head is independent of the body.
        entity.set_head_yaw(20.0);
        entity.set_server_target(DVec3::ZERO, 180.0, 0.0); // body keeps turning
        for _ in 0..10 {
            entity.tick_interpolation();
        }
        assert!(
            (entity.head_yaw - 20.0).abs() < 1.0,
            "head should hold its own target: {}",
            entity.head_yaw
        );
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
