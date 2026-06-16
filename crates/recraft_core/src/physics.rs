use glam::DVec3;

use crate::mc_math::{mc_cos, mc_sin, DEG_TO_RAD};
use crate::{BlockState, EntityState, World};

/// Vanilla/Grim collision tolerance. A box that merely touches a face — or sits
/// a hair (≤ this) past it from accumulated float error — is still treated as
/// colliding, so a player resting flush against a wall can never be nudged
/// through it by rounding. Mirrors `SimpleCollisionBox.COLLISION_EPSILON`.
const COLLISION_EPSILON: f64 = 1.0e-7;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: DVec3,
    pub max: DVec3,
}

impl Aabb {
    pub fn new(min: DVec3, max: DVec3) -> Self {
        Self { min, max }
    }

    pub fn player_at(feet: DVec3) -> Self {
        let half_width = 0.3;
        Self {
            min: DVec3::new(feet.x - half_width, feet.y, feet.z - half_width),
            max: DVec3::new(feet.x + half_width, feet.y + 1.8, feet.z + half_width),
        }
    }

    pub fn offset(self, delta: DVec3) -> Self {
        Self {
            min: self.min + delta,
            max: self.max + delta,
        }
    }

    pub fn add_coord(self, delta: DVec3) -> Self {
        let mut min = self.min;
        let mut max = self.max;

        if delta.x < 0.0 {
            min.x += delta.x;
        } else if delta.x > 0.0 {
            max.x += delta.x;
        }

        if delta.y < 0.0 {
            min.y += delta.y;
        } else if delta.y > 0.0 {
            max.y += delta.y;
        }

        if delta.z < 0.0 {
            min.z += delta.z;
        } else if delta.z > 0.0 {
            max.z += delta.z;
        }

        Self { min, max }
    }

    pub fn intersects(self, other: Self) -> bool {
        self.max.x > other.min.x
            && self.min.x < other.max.x
            && self.max.y > other.min.y
            && self.min.y < other.max.y
            && self.max.z > other.min.z
            && self.min.z < other.max.z
    }

    /// Clamp an x move so `other` (the moving box) cannot pass through `self`
    /// (a collider). Ported 1:1 from Grim `SimpleCollisionBox.collideX`: the
    /// perpendicular (y/z) overlap must exceed the epsilon, and a face merely
    /// touched — or overshot by ≤ epsilon — still blocks. Only a box already
    /// more than epsilon past the face is let through (so a deeply-embedded box
    /// is never yanked back).
    fn calculate_x_offset(self, other: Self, offset: f64) -> f64 {
        if offset != 0.0
            && (other.min.y - self.max.y) < -COLLISION_EPSILON
            && (other.max.y - self.min.y) > COLLISION_EPSILON
            && (other.min.z - self.max.z) < -COLLISION_EPSILON
            && (other.max.z - self.min.z) > COLLISION_EPSILON
        {
            if offset >= 0.0 {
                let max_move = self.min.x - other.max.x;
                if max_move < -COLLISION_EPSILON {
                    offset
                } else {
                    max_move.min(offset)
                }
            } else {
                let max_move = self.max.x - other.min.x;
                if max_move > COLLISION_EPSILON {
                    offset
                } else {
                    max_move.max(offset)
                }
            }
        } else {
            offset
        }
    }

    fn calculate_y_offset(self, other: Self, offset: f64) -> f64 {
        if offset != 0.0
            && (other.min.x - self.max.x) < -COLLISION_EPSILON
            && (other.max.x - self.min.x) > COLLISION_EPSILON
            && (other.min.z - self.max.z) < -COLLISION_EPSILON
            && (other.max.z - self.min.z) > COLLISION_EPSILON
        {
            if offset >= 0.0 {
                let max_move = self.min.y - other.max.y;
                if max_move < -COLLISION_EPSILON {
                    offset
                } else {
                    max_move.min(offset)
                }
            } else {
                let max_move = self.max.y - other.min.y;
                if max_move > COLLISION_EPSILON {
                    offset
                } else {
                    max_move.max(offset)
                }
            }
        } else {
            offset
        }
    }

    fn calculate_z_offset(self, other: Self, offset: f64) -> f64 {
        if offset != 0.0
            && (other.min.x - self.max.x) < -COLLISION_EPSILON
            && (other.max.x - self.min.x) > COLLISION_EPSILON
            && (other.min.y - self.max.y) < -COLLISION_EPSILON
            && (other.max.y - self.min.y) > COLLISION_EPSILON
        {
            if offset >= 0.0 {
                let max_move = self.min.z - other.max.z;
                if max_move < -COLLISION_EPSILON {
                    offset
                } else {
                    max_move.min(offset)
                }
            } else {
                let max_move = self.max.z - other.min.z;
                if max_move > COLLISION_EPSILON {
                    offset
                } else {
                    max_move.max(offset)
                }
            }
        } else {
            offset
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PlayerInput {
    pub forward: f32,
    pub strafe: f32,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
    /// Creative-style flight is active (vanilla `capabilities.isFlying`).
    pub flying: bool,
    /// Fly speed from the server's abilities packet (vanilla default 0.05).
    pub fly_speed: f32,
    /// Ground movement speed: the vanilla movementSpeed attribute value
    /// (base 0.1, scaled by potion/server modifiers via S20), excluding the
    /// sprint boost which physics applies itself.
    pub walk_speed: f32,
}

impl Default for PlayerInput {
    fn default() -> Self {
        Self {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: false,
            flying: false,
            fly_speed: 0.05,
            walk_speed: 0.1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PlayerPhysicsConfig {
    pub gravity: f64,
    pub jump_velocity: f64,
    pub air_drag_y: f64,
    pub step_height: f64,
    pub air_acceleration: f32,
    pub ground_acceleration: f32,
    pub sprint_multiplier: f32,
    pub default_block_slipperiness: f32,
}

impl Default for PlayerPhysicsConfig {
    fn default() -> Self {
        Self {
            gravity: 0.08,
            jump_velocity: 0.42,
            air_drag_y: 0.9800000190734863,
            step_height: 0.6,
            air_acceleration: 0.02,
            ground_acceleration: 0.1,
            sprint_multiplier: 1.3,
            default_block_slipperiness: 0.6,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PlayerPhysics {
    pub config: PlayerPhysicsConfig,
}

impl PlayerPhysics {
    pub fn tick(&self, world: &World, player: &mut EntityState, input: PlayerInput) {
        let mut velocity = player.velocity;

        // Vanilla EntityLivingBase.onLivingUpdate zeroes sub-0.005 motion before
        // anything else this tick.
        if velocity.x.abs() < 0.005 {
            velocity.x = 0.0;
        }
        if velocity.y.abs() < 0.005 {
            velocity.y = 0.0;
        }
        if velocity.z.abs() < 0.005 {
            velocity.z = 0.0;
        }

        // Fluid/ladder state for this tick. Vanilla caches isInWater in
        // handleWaterMovement (run before the heading move) and computes
        // isInLava/isOnLadder live; all read the pre-move box, so sample once.
        let in_water = is_in_water(world, player.aabb);
        let in_lava = is_in_lava(world, player.aabb);
        let on_ladder = is_on_ladder(world, player.position, player.aabb);

        // Vanilla EntityPlayerSP flight thrust, applied to motionY before the
        // move: sneak descends, jump ascends, both ±flySpeed*3.
        if input.flying {
            if input.sneak {
                velocity.y -= (input.fly_speed * 3.0) as f64;
            }
            if input.jump {
                velocity.y += (input.fly_speed * 3.0) as f64;
            }
        }
        // Vanilla EntityLivingBase.onLivingUpdate jump handling: inside a fluid a
        // held jump only adds 0.04 buoyancy; otherwise a grounded jump assigns
        // the 0.42 launch (plus the sprint boost).
        if input.jump && !input.flying && (in_water || in_lava) {
            velocity.y += 0.04;
        } else if input.jump && player.on_ground {
            velocity.y = self.config.jump_velocity;
            if input.sprint {
                // Vanilla sprint-jump boost: 0.2 in the facing direction, using
                // MathHelper trig (not libm) so the direction matches the server.
                let yaw = player.yaw * DEG_TO_RAD;
                velocity.x -= (mc_sin(yaw) * 0.2) as f64;
                velocity.z += (mc_cos(yaw) * 0.2) as f64;
            }
        }

        // Movement input shaping (vanilla onLivingUpdate): sneaking shrinks it to
        // 0.3, then everything is scaled by 0.98.
        let mut forward = input.forward;
        let mut strafe = input.strafe;
        if input.sneak {
            forward *= 0.3;
            strafe *= 0.3;
        }
        forward *= 0.98;
        strafe *= 0.98;

        // Block friction of the support block (vanilla slipperiness * 0.91),
        // only consulted while standing; airborne friction is a flat 0.91.
        let block_friction: f32 = if player.on_ground {
            block_below_slipperiness(world, player.position, player.aabb)
        } else {
            1.0
        };
        let horizontal_drag = block_friction * 0.91;

        let was_in_web = player.in_web;

        // Vanilla moveEntityWithHeading has three mutually exclusive branches
        // (water, lava, normal) plus the client-only flight override of the
        // normal branch's vertical motion.
        let (feet, on_ground, collided, out_velocity) = if input.flying {
            let acceleration = input.fly_speed * if input.sprint { 2.0 } else { 1.0 };
            velocity += movement_vector(forward, strafe, player.yaw, acceleration);
            let pre_move_y = velocity.y;
            let result = web_aware_move(
                world,
                player.aabb,
                velocity,
                self.config.step_height,
                player.on_ground,
                was_in_web,
            );
            let mut v = result.velocity;
            // While flying, vanilla restores the pre-move vertical motion (×0.6),
            // discarding gravity/drag and any vertical collision.
            v.y = pre_move_y * 0.6;
            v.x *= horizontal_drag as f64;
            v.z *= horizontal_drag as f64;
            (
                result.feet,
                result.on_ground,
                result.collided_horizontally,
                v,
            )
        } else if in_water {
            // Vanilla water branch: 0.02 accel, 0.8 horizontal drag, 0.8 vertical
            // drag, a flat 0.02 sink, and a swim-up bump against ledges.
            velocity += movement_vector(forward, strafe, player.yaw, 0.02);
            let pre_pos_y = player.position.y;
            let result = web_aware_move(
                world,
                player.aabb,
                velocity,
                self.config.step_height,
                player.on_ground,
                was_in_web,
            );
            let mut v = result.velocity;
            v.x *= 0.8;
            v.z *= 0.8;
            v.y *= 0.8;
            v.y -= 0.02;
            apply_swim_up_bump(
                world,
                result.feet,
                result.collided_horizontally,
                pre_pos_y,
                &mut v,
            );
            (
                result.feet,
                result.on_ground,
                result.collided_horizontally,
                v,
            )
        } else if in_lava {
            // Vanilla lava branch: 0.02 accel, 0.5 drag on every axis, 0.02 sink.
            velocity += movement_vector(forward, strafe, player.yaw, 0.02);
            let pre_pos_y = player.position.y;
            let result = web_aware_move(
                world,
                player.aabb,
                velocity,
                self.config.step_height,
                player.on_ground,
                was_in_web,
            );
            let mut v = result.velocity * 0.5;
            v.y -= 0.02;
            apply_swim_up_bump(
                world,
                result.feet,
                result.collided_horizontally,
                pre_pos_y,
                &mut v,
            );
            (
                result.feet,
                result.on_ground,
                result.collided_horizontally,
                v,
            )
        } else {
            // Normal branch: friction-scaled ground accel or the airborne
            // jumpMovementFactor, then climbable clamping, gravity and drag.
            let move_speed = input.walk_speed
                * if input.sprint {
                    self.config.sprint_multiplier
                } else {
                    1.0
                };
            // Airborne accel uses the PREVIOUS tick's sprint (vanilla
            // `jumpMovementFactor`, set at the end of `onLivingUpdate` after the
            // move). The ground accel above and the jump boost both use the
            // current sprint — only this air factor lags by a tick.
            let acceleration = if player.on_ground {
                move_speed * (0.16277136 / (horizontal_drag * horizontal_drag * horizontal_drag))
            } else {
                self.config.air_acceleration
                    * if player.air_sprinting {
                        self.config.sprint_multiplier
                    } else {
                        1.0
                    }
            };
            velocity += movement_vector(forward, strafe, player.yaw, acceleration);

            // Vanilla 1.8 sneak edge protection: while sneaking on the ground, the
            // intended horizontal movement is shrunk in 0.05 steps until the
            // player box (lowered by one block) would still rest on a collider,
            // so the player never walks off a ledge.
            if input.sneak && player.on_ground {
                let (clamped_x, clamped_z) =
                    clamp_sneak_to_edges(world, player.aabb, velocity.x, velocity.z);
                velocity.x = clamped_x;
                velocity.z = clamped_z;
            }

            // Vanilla isOnLadder clamp, applied to motion before the move: the
            // horizontal speed is capped at ±0.15, descent at 0.15, and a
            // sneaking player stops sliding down entirely.
            if on_ladder {
                velocity.x = velocity.x.clamp(-0.15, 0.15);
                velocity.z = velocity.z.clamp(-0.15, 0.15);
                if velocity.y < -0.15 {
                    velocity.y = -0.15;
                }
                if input.sneak && velocity.y < 0.0 {
                    velocity.y = 0.0;
                }
            }

            let pre_move_y = velocity.y;
            let result = web_aware_move(
                world,
                player.aabb,
                velocity,
                self.config.step_height,
                player.on_ground,
                was_in_web,
            );
            let mut v = result.velocity;

            // Pushing into a ladder climbs it (vanilla sets motionY = 0.2 after
            // the move when isCollidedHorizontally on a ladder).
            if result.collided_horizontally && on_ladder {
                v.y = 0.2;
            }

            // Slime block bounce (vanilla BlockSlime.onLanded): a non-sneaking
            // entity that lands reflects its descent velocity; sneaking cancels
            // it (handled by the default zero already in `result`).
            if result.landed
                && pre_move_y < 0.0
                && !input.sneak
                && slime_block_below(world, result.feet)
            {
                v.y = -pre_move_y;
            }

            v.y -= self.config.gravity;
            v.y *= self.config.air_drag_y;
            v.x *= horizontal_drag as f64;
            v.z *= horizontal_drag as f64;
            (
                result.feet,
                result.on_ground,
                result.collided_horizontally,
                v,
            )
        };

        player.position = feet;
        player.on_ground = on_ground;
        player.collided_horizontally = collided;
        player.velocity = out_velocity;
        player.sync_aabb_to_position();

        // Vanilla Entity.doBlockCollisions over the post-move box: cobwebs arm
        // the stuck flag for next tick, soul sand multiplies horizontal motion by
        // 0.4. Flying players are exempt from cobwebs (matching the client).
        let post_box = player.aabb;
        player.in_web =
            apply_inside_block_effects(world, post_box, &mut player.velocity, input.flying);

        // Vanilla updates `jumpMovementFactor` from the current sprint state at
        // the end of `EntityPlayer.onLivingUpdate` (after the move), so next
        // tick's airborne accel reflects this tick's sprint.
        player.air_sprinting = input.sprint;
    }
}

fn movement_vector(forward: f32, strafe: f32, yaw_degrees: f32, friction: f32) -> DVec3 {
    let mut length = strafe * strafe + forward * forward;
    if length < 1.0e-4 {
        return DVec3::ZERO;
    }
    length = length.sqrt();
    if length < 1.0 {
        length = 1.0;
    }

    let scale = friction / length;
    let strafe = strafe * scale;
    let forward = forward * scale;
    // Vanilla Entity.moveFlying uses MathHelper's table-based sin/cos, which the
    // server replicates exactly; using libm here drifts from the prediction.
    let yaw = yaw_degrees * DEG_TO_RAD;
    let sin = mc_sin(yaw);
    let cos = mc_cos(yaw);
    DVec3::new(
        (strafe * cos - forward * sin) as f64,
        0.0,
        (forward * cos + strafe * sin) as f64,
    )
}

fn move_with_collisions(
    world: &World,
    aabb: Aabb,
    velocity: DVec3,
    step_height: f64,
    was_on_ground: bool,
) -> MoveResult {
    let original_velocity = velocity;
    let mut x = velocity.x;
    let mut y = velocity.y;
    let mut z = velocity.z;
    let original_x = x;
    let original_y = y;
    let original_z = z;
    let original_aabb = aabb;
    let colliders = colliding_boxes(world, aabb.add_coord(velocity));

    let mut moved = aabb;
    for collider in &colliders {
        y = collider.calculate_y_offset(moved, y);
    }
    moved = moved.offset(DVec3::new(0.0, y, 0.0));
    let can_step = was_on_ground || original_y != y && original_y < 0.0;

    for collider in &colliders {
        x = collider.calculate_x_offset(moved, x);
    }
    moved = moved.offset(DVec3::new(x, 0.0, 0.0));

    for collider in &colliders {
        z = collider.calculate_z_offset(moved, z);
    }
    moved = moved.offset(DVec3::new(0.0, 0.0, z));

    if step_height > 0.0 && can_step && (original_x != x || original_z != z) {
        let normal_x = x;
        let normal_y = y;
        let normal_z = z;
        let normal_aabb = moved;
        y = step_height;

        let step_colliders = colliding_boxes(
            world,
            original_aabb.add_coord(DVec3::new(original_x, y, original_z)),
        );
        let base_aabb = original_aabb;
        let horizontal_aabb = base_aabb.add_coord(DVec3::new(original_x, 0.0, original_z));
        let mut step_y_first = y;

        for collider in &step_colliders {
            step_y_first = collider.calculate_y_offset(horizontal_aabb, step_y_first);
        }

        let mut aabb_y_first = base_aabb.offset(DVec3::new(0.0, step_y_first, 0.0));
        let mut x_y_first = original_x;
        for collider in &step_colliders {
            x_y_first = collider.calculate_x_offset(aabb_y_first, x_y_first);
        }
        aabb_y_first = aabb_y_first.offset(DVec3::new(x_y_first, 0.0, 0.0));

        let mut z_y_first = original_z;
        for collider in &step_colliders {
            z_y_first = collider.calculate_z_offset(aabb_y_first, z_y_first);
        }
        aabb_y_first = aabb_y_first.offset(DVec3::new(0.0, 0.0, z_y_first));

        let mut aabb_xz_first = base_aabb;
        let mut step_y_xz_first = y;
        for collider in &step_colliders {
            step_y_xz_first = collider.calculate_y_offset(aabb_xz_first, step_y_xz_first);
        }
        aabb_xz_first = aabb_xz_first.offset(DVec3::new(0.0, step_y_xz_first, 0.0));

        let mut x_xz_first = original_x;
        for collider in &step_colliders {
            x_xz_first = collider.calculate_x_offset(aabb_xz_first, x_xz_first);
        }
        aabb_xz_first = aabb_xz_first.offset(DVec3::new(x_xz_first, 0.0, 0.0));

        let mut z_xz_first = original_z;
        for collider in &step_colliders {
            z_xz_first = collider.calculate_z_offset(aabb_xz_first, z_xz_first);
        }
        aabb_xz_first = aabb_xz_first.offset(DVec3::new(0.0, 0.0, z_xz_first));

        if x_y_first * x_y_first + z_y_first * z_y_first
            > x_xz_first * x_xz_first + z_xz_first * z_xz_first
        {
            x = x_y_first;
            z = z_y_first;
            y = -step_y_first;
            moved = aabb_y_first;
        } else {
            x = x_xz_first;
            z = z_xz_first;
            y = -step_y_xz_first;
            moved = aabb_xz_first;
        }

        for collider in &step_colliders {
            y = collider.calculate_y_offset(moved, y);
        }
        moved = moved.offset(DVec3::new(0.0, y, 0.0));

        if normal_x * normal_x + normal_z * normal_z >= x * x + z * z {
            x = normal_x;
            y = normal_y;
            z = normal_z;
            moved = normal_aabb;
        }
    }

    let feet = DVec3::new(
        (moved.min.x + moved.max.x) * 0.5,
        moved.min.y,
        (moved.min.z + moved.max.z) * 0.5,
    );
    let mut adjusted_velocity = DVec3::new(x, y, z);
    if original_velocity.x != x {
        adjusted_velocity.x = 0.0;
    }
    if original_velocity.y != y {
        adjusted_velocity.y = 0.0;
    }
    if original_velocity.z != z {
        adjusted_velocity.z = 0.0;
    }
    // Vanilla sets onGround when a downward move is stopped by collision. A player
    // resting with zero vertical velocity — e.g. the tick after a teleport/setback
    // zeroes motion — has no downward move to collide, yet is still standing on a
    // block. Detect that support directly so on_ground reporting matches the server
    // (otherwise the client claims it is airborne while grounded, tripping anti-cheat
    // ground checks and provoking an inescapable setback loop).
    let landed = original_y != y && original_y < 0.0;
    let on_ground = landed || (original_y == 0.0 && supported_below(world, moved));
    // Vanilla isCollidedHorizontally: set whenever an intended horizontal move
    // was clamped by collision. Drives the sprint wall-cancel.
    let collided_horizontally = original_x != x || original_z != z;

    MoveResult {
        feet,
        velocity: adjusted_velocity,
        on_ground,
        collided_horizontally,
        landed,
    }
}

struct MoveResult {
    feet: DVec3,
    velocity: DVec3,
    on_ground: bool,
    collided_horizontally: bool,
    /// A downward move was stopped by collision this tick (vanilla
    /// `isCollidedVertically && d1 < 0`). Drives the slime-block bounce, which
    /// only fires on a genuine landing rather than mere resting support.
    landed: bool,
}

/// Run a move while honouring the cobweb stuck-speed. Vanilla
/// `Entity.moveEntity` scales the desired motion to (0.25, 0.05, 0.25) and
/// zeroes the stored motion when the entity entered a web the previous tick;
/// the scaled motion still drives collision so the box barely creeps along.
fn web_aware_move(
    world: &World,
    aabb: Aabb,
    velocity: DVec3,
    step_height: f64,
    was_on_ground: bool,
    in_web: bool,
) -> MoveResult {
    let move_velocity = if in_web {
        DVec3::new(velocity.x * 0.25, velocity.y * 0.05, velocity.z * 0.25)
    } else {
        velocity
    };
    let mut result = move_with_collisions(world, aabb, move_velocity, step_height, was_on_ground);
    if in_web {
        result.velocity = DVec3::ZERO;
    }
    result
}

/// Vanilla `EntityItem.onUpdate` physics for a dropped item (object type 2).
/// Apply gravity (`-0.04`), move with world collision (no step-up), then
/// horizontal drag (`0.98`, scaled by the supporting block's slipperiness on
/// the ground), vertical drag (`0.98`) and the `-0.5` ground bounce. Running
/// this client-side each tick makes a freshly-dropped item arc immediately
/// instead of freezing at its spawn height until the next server position
/// packet; server updates still correct the position via the normal lerp path.
///
/// Advances `previous_position`/`age` like `tick_interpolation` so the render
/// interpolation and the dropped-item bob/spin stay continuous.
pub fn tick_item(world: &World, item: &mut EntityState) {
    item.previous_position = item.position;
    item.age = item.age.wrapping_add(1);

    // Gravity is applied before the move in vanilla.
    item.velocity.y -= 0.04;
    let result = move_with_collisions(world, item.aabb, item.velocity, 0.0, item.on_ground);
    item.position = result.feet;
    item.on_ground = result.on_ground;
    item.velocity = result.velocity;

    let horizontal = if item.on_ground {
        block_below_slipperiness(world, item.position, item.aabb) as f64 * 0.98
    } else {
        0.98
    };
    item.velocity.x *= horizontal;
    item.velocity.y *= 0.98;
    item.velocity.z *= horizontal;
    if item.on_ground {
        item.velocity.y *= -0.5;
    }
    item.sync_aabb_to_position();
}

/// Slipperiness of the block supporting the player. Vanilla samples one block
/// below `floor(boundingBox.minY)` in the feet column.
fn block_below_slipperiness(world: &World, position: DVec3, aabb: Aabb) -> f32 {
    let x = position.x.floor() as i32;
    let y = (aabb.min.y.floor() as i32) - 1;
    let z = position.z.floor() as i32;
    world.block_at(x, y, z).slipperiness()
}

/// Vanilla `Entity.isOnLadder`: a ladder or vine in the block the feet occupy.
fn is_on_ladder(world: &World, position: DVec3, aabb: Aabb) -> bool {
    let x = position.x.floor() as i32;
    let y = aabb.min.y.floor() as i32;
    let z = position.z.floor() as i32;
    world.block_at(x, y, z).is_climbable()
}

/// Whether the block the player just landed on (vanilla `floor(posY - 0.2)`) is
/// a slime block.
fn slime_block_below(world: &World, feet: DVec3) -> bool {
    let x = feet.x.floor() as i32;
    let y = (feet.y - 0.2).floor() as i32;
    let z = feet.z.floor() as i32;
    world.block_at(x, y, z).is_slime_block()
}

/// True if any block cell touching `b` satisfies `pred`.
fn any_cell<F: Fn(BlockState) -> bool>(world: &World, b: Aabb, pred: F) -> bool {
    let min_x = b.min.x.floor() as i32;
    let max_x = b.max.x.floor() as i32;
    let min_y = b.min.y.floor() as i32;
    let max_y = b.max.y.floor() as i32;
    let min_z = b.min.z.floor() as i32;
    let max_z = b.max.z.floor() as i32;
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                if pred(world.block_at(x, y, z)) {
                    return true;
                }
            }
        }
    }
    false
}

/// Vanilla `handleWaterMovement` test box: the player box pulled in by 0.4 on
/// the top and bottom and 0.001 on every side, intersected against water cells.
/// The 1.8 liquid-height nuance (a source surface sits 1/9 below the cell top)
/// and the current flow vector are not modelled, so each water cell is treated
/// as full — exact for submerged play, slightly eager at a flowing edge, and
/// with no downstream push.
fn is_in_water(world: &World, aabb: Aabb) -> bool {
    let b = Aabb::new(
        DVec3::new(aabb.min.x + 0.001, aabb.min.y + 0.401, aabb.min.z + 0.001),
        DVec3::new(aabb.max.x - 0.001, aabb.max.y - 0.401, aabb.max.z - 0.001),
    );
    any_cell(world, b, |s| s.is_water())
}

/// Vanilla `isInLava` test box: the player box pulled in by 0.1 on x/z and 0.4
/// on y, intersected against lava cells.
fn is_in_lava(world: &World, aabb: Aabb) -> bool {
    let b = Aabb::new(
        DVec3::new(aabb.min.x + 0.1, aabb.min.y + 0.4, aabb.min.z + 0.1),
        DVec3::new(aabb.max.x - 0.1, aabb.max.y - 0.4, aabb.max.z - 0.1),
    );
    any_cell(world, b, |s| s.is_lava())
}

/// Vanilla water/lava swim-up: when an entity is pinned against a wall and the
/// position 0.6 above its feet is free of collision and liquid (a ledge to climb
/// onto), its vertical motion is bumped to 0.3 so it hops out.
fn apply_swim_up_bump(
    world: &World,
    feet: DVec3,
    collided_horizontally: bool,
    pre_pos_y: f64,
    velocity: &mut DVec3,
) {
    if !collided_horizontally {
        return;
    }
    // Vanilla offset: (motionX, motionY + 0.6 - posY + d0, motionZ), where d0 is
    // the feet height before this tick's move.
    let offset = DVec3::new(
        velocity.x,
        velocity.y + 0.6 - feet.y + pre_pos_y,
        velocity.z,
    );
    if is_offset_position_free(world, feet, offset) {
        velocity.y = 0.3;
    }
}

/// Vanilla `isOffsetPositionInLiquid`: the player box moved to `feet + offset`
/// overlaps neither a collider nor any liquid.
fn is_offset_position_free(world: &World, feet: DVec3, offset: DVec3) -> bool {
    let probe = Aabb::player_at(feet).offset(offset);
    !has_collision(world, probe) && !any_cell(world, probe, |s| s.is_liquid())
}

/// Vanilla `Entity.doBlockCollisions` over the post-move box (pulled in 0.001):
/// a cobweb arms the stuck flag (returned, consumed at the next move), and each
/// soul-sand cell multiplies horizontal motion by 0.4. Flying players ignore
/// cobwebs (the client clears the stuck multiplier for them).
fn apply_inside_block_effects(
    world: &World,
    aabb: Aabb,
    velocity: &mut DVec3,
    flying: bool,
) -> bool {
    let b = Aabb::new(
        DVec3::new(aabb.min.x + 0.001, aabb.min.y + 0.001, aabb.min.z + 0.001),
        DVec3::new(aabb.max.x - 0.001, aabb.max.y - 0.001, aabb.max.z - 0.001),
    );
    let min_x = b.min.x.floor() as i32;
    let max_x = b.max.x.floor() as i32;
    let min_y = b.min.y.floor() as i32;
    let max_y = b.max.y.floor() as i32;
    let min_z = b.min.z.floor() as i32;
    let max_z = b.max.z.floor() as i32;
    let mut in_web = false;
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                let block = world.block_at(x, y, z);
                if block.is_air() {
                    continue;
                }
                if block.is_cobweb() {
                    in_web = true;
                }
                if block.is_soul_sand() {
                    velocity.x *= 0.4;
                    velocity.z *= 0.4;
                }
            }
        }
    }
    in_web && !flying
}

/// Reduce the desired sneak movement so the player stays supported. Mirrors the
/// 1.8 `Entity.moveEntity` ledge check: each axis (and finally both together) is
/// stepped down by 0.05 while the box offset by `(dx, -1, dz)` finds no collider.
fn clamp_sneak_to_edges(world: &World, aabb: Aabb, mut dx: f64, mut dz: f64) -> (f64, f64) {
    const STEP: f64 = 0.05;

    while dx != 0.0 && !has_collision(world, aabb.offset(DVec3::new(dx, -1.0, 0.0))) {
        dx = shrink_sneak_step(dx, STEP);
    }
    while dz != 0.0 && !has_collision(world, aabb.offset(DVec3::new(0.0, -1.0, dz))) {
        dz = shrink_sneak_step(dz, STEP);
    }
    while dx != 0.0 && dz != 0.0 && !has_collision(world, aabb.offset(DVec3::new(dx, -1.0, dz))) {
        dx = shrink_sneak_step(dx, STEP);
        dz = shrink_sneak_step(dz, STEP);
    }

    (dx, dz)
}

fn shrink_sneak_step(value: f64, step: f64) -> f64 {
    if value < step && value >= -step {
        0.0
    } else if value > 0.0 {
        value - step
    } else {
        value + step
    }
}

/// Public ground check: whether the player box is resting on solid ground. Used
/// to set on_ground correctly right after a teleport (which zeroes motion, so the
/// next tick has no downward move for the collision-based ground test to catch).
pub fn resting_on_ground(world: &World, aabb: Aabb) -> bool {
    supported_below(world, aabb)
}

/// True when a block surface sits immediately beneath the AABB's feet (within a
/// 0.001 epsilon) — i.e. the box is resting on the ground even with no downward
/// motion this tick. Mirrors how the server treats a grounded player and keeps
/// on_ground reporting correct across teleports that zero the player's motion.
fn supported_below(world: &World, aabb: Aabb) -> bool {
    let slab = Aabb::new(
        DVec3::new(aabb.min.x, aabb.min.y - 0.001, aabb.min.z),
        DVec3::new(aabb.max.x, aabb.min.y, aabb.max.z),
    );
    has_collision(world, slab)
}

fn has_collision(world: &World, query: Aabb) -> bool {
    !colliding_boxes(world, query).is_empty()
}

fn colliding_boxes(world: &World, query: Aabb) -> Vec<Aabb> {
    // Match vanilla World.getCollidingBoundingBoxes which scans
    // floor(min)..floor(max + 1); using ceil(max) would miss the block on the
    // far side when the box edge lands exactly on an integer boundary. The y
    // scan starts one block BELOW floor(min) — that is how vanilla picks up
    // fence/wall boxes that extend 1.5 above their cell (standing on a fence
    // top, the fence block itself is below the query range).
    let min_x = query.min.x.floor() as i32;
    let max_x = (query.max.x + 1.0).floor() as i32;
    let min_y = (query.min.y.floor() as i32) - 1;
    let max_y = (query.max.y + 1.0).floor() as i32;
    let min_z = query.min.z.floor() as i32;
    let max_z = (query.max.z + 1.0).floor() as i32;
    let mut colliders = Vec::new();

    for x in min_x..max_x {
        for z in min_z..max_z {
            for y in min_y..max_y {
                crate::collision::add_block_collision_boxes(world, x, y, z, query, &mut colliders);
            }
        }
    }

    colliders
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;
    use crate::{BlockState, EntityId, EntityKind};

    #[test]
    fn falling_player_lands_on_solid_block() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.2, 0.5));
        player.velocity = DVec3::new(0.0, -0.5, 0.0);

        PlayerPhysics::default().tick(&world, &mut player, PlayerInput::default());

        assert!(player.on_ground);
        assert!((player.position.y - 1.0).abs() < 0.001);
    }

    #[test]
    fn dropped_item_falls_under_gravity_and_settles_on_ground() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let mut item =
            EntityState::new_remote(EntityId(7), EntityKind::Object(2), DVec3::new(0.5, 4.0, 0.5), 0.0, 0.0);

        // One tick applies gravity and moves the item down (no stall).
        let start_y = item.position.y;
        tick_item(&world, &mut item);
        assert!(item.position.y < start_y, "item should start falling immediately");
        assert!(item.velocity.y < 0.0, "downward velocity accumulates");
        assert_eq!(item.previous_position.y, start_y, "render-interp prev tracks last tick");

        // It eventually rests on top of the block (box bottom at y=1).
        for _ in 0..200 {
            tick_item(&world, &mut item);
        }
        assert!(item.on_ground, "item should land");
        assert!((item.position.y - 1.0).abs() < 0.05, "rests on the block top: {}", item.position.y);
    }

    #[test]
    fn dropped_item_keeps_horizontal_motion_with_drag() {
        let world = World::new(); // empty: item flies freely
        let mut item =
            EntityState::new_remote(EntityId(8), EntityKind::Object(2), DVec3::new(0.0, 64.0, 0.0), 0.0, 0.0);
        item.velocity = DVec3::new(0.5, 0.0, 0.0);
        tick_item(&world, &mut item);
        assert!(item.position.x > 0.0, "horizontal velocity carries the item");
        // Air drag 0.98 reduces horizontal speed below the initial 0.5.
        assert!(item.velocity.x < 0.5 && item.velocity.x > 0.45);
    }

    #[test]
    fn movement_forward_matches_minecraft_yaw_convention() {
        let forward_at_zero = movement_vector(1.0, 0.0, 0.0, 1.0);
        assert!(forward_at_zero.z > 0.99);
        assert!(forward_at_zero.x.abs() < 0.001);

        let forward_at_ninety = movement_vector(1.0, 0.0, 90.0, 1.0);
        assert!(forward_at_ninety.x < -0.99);
        assert!(forward_at_ninety.z.abs() < 0.001);

        let left_at_zero = movement_vector(0.0, 1.0, 0.0, 1.0);
        assert!(left_at_zero.x > 0.99);
        assert!(left_at_zero.z.abs() < 0.001);

        let right_at_zero = movement_vector(0.0, -1.0, 0.0, 1.0);
        assert!(right_at_zero.x < -0.99);
        assert!(right_at_zero.z.abs() < 0.001);
    }

    #[test]
    fn jump_moves_before_gravity_drag_for_tick() {
        let world = World::new();
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.0, 0.5));
        player.on_ground = true;

        PlayerPhysics::default().tick(
            &world,
            &mut player,
            PlayerInput {
                jump: true,
                ..PlayerInput::default()
            },
        );

        assert!((player.position.y - 1.42).abs() < 0.001);
        assert!((player.velocity.y - 0.3332).abs() < 0.001);
    }

    #[test]
    fn head_collision_stops_upward_motion_before_gravity() {
        let mut world = World::new();
        world.set_block(0, 3, 0, BlockState::STONE);
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.0, 0.5));
        player.velocity = DVec3::new(0.0, 0.42, 0.0);

        PlayerPhysics::default().tick(&world, &mut player, PlayerInput::default());

        assert!((player.position.y - 1.2).abs() < 0.001);
        assert!((player.velocity.y + 0.0784).abs() < 0.001);
    }

    #[test]
    fn player_does_not_auto_climb_full_block() {
        let world = flat_world_with_one_block_step();
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.0, 0.2));
        player.on_ground = true;
        let physics = PlayerPhysics::default();

        for _ in 0..20 {
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    forward: 1.0,
                    ..PlayerInput::default()
                },
            );
        }

        assert!(player.position.y < 1.01);
        assert!(player.position.z <= 1.701);
    }

    #[test]
    fn player_can_jump_onto_full_block() {
        let world = flat_world_with_one_block_step();
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.0, 0.2));
        player.on_ground = true;
        let physics = PlayerPhysics::default();

        for _ in 0..30 {
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    forward: 1.0,
                    jump: true,
                    ..PlayerInput::default()
                },
            );
        }

        assert!(player.position.y >= 1.99);
        assert!(player.position.z > 2.0);
    }

    #[test]
    fn sneaking_player_stays_on_ledge_while_walking_off() {
        let world = single_block_platform();
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.0, 0.5));
        player.on_ground = true;
        let physics = PlayerPhysics::default();

        for _ in 0..60 {
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    forward: 1.0,
                    sneak: true,
                    ..PlayerInput::default()
                },
            );
        }

        assert!(player.on_ground, "sneaking player should not fall off");
        assert!(
            player.position.y > 0.99,
            "sneaking player should stay at block height, was {}",
            player.position.y
        );
    }

    #[test]
    fn walking_player_falls_off_ledge_without_sneak() {
        let world = single_block_platform();
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.0, 0.5));
        player.on_ground = true;
        let physics = PlayerPhysics::default();

        for _ in 0..60 {
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    forward: 1.0,
                    ..PlayerInput::default()
                },
            );
        }

        assert!(
            player.position.y < 0.5,
            "walking player should fall off the ledge, was {}",
            player.position.y
        );
    }

    #[test]
    fn player_rests_on_top_of_bottom_slab() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(44, 0)); // bottom stone slab
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 2.0, 0.5));
        let physics = PlayerPhysics::default();
        for _ in 0..40 {
            physics.tick(&world, &mut player, PlayerInput::default());
        }
        assert!(player.on_ground);
        assert!(
            (player.position.y - 0.5).abs() < 0.01,
            "should rest on the half slab, was {}",
            player.position.y
        );
    }

    /// Drop a player onto the block at the origin and return the resting feet
    /// height. Asserts the player actually lands.
    fn resting_height_on(block: BlockState) -> f64 {
        let mut world = World::new();
        world.set_block(0, 0, 0, block);
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 3.0, 0.5));
        let physics = PlayerPhysics::default();
        for _ in 0..60 {
            physics.tick(&world, &mut player, PlayerInput::default());
        }
        assert!(player.on_ground, "player never landed on {block:?}");
        player.position.y
    }

    #[test]
    fn resting_heights_match_vanilla_partial_blocks() {
        for (block, height) in [
            (BlockState::new(60, 0), 1.0),     // farmland is a full cube in 1.8
            (BlockState::new(54, 2), 0.875),   // chest
            (BlockState::new(88, 0), 0.875),   // soul sand
            (BlockState::new(26, 0), 0.5625),  // bed
            (BlockState::new(96, 0), 0.1875),  // closed bottom trapdoor
            (BlockState::new(78, 0), 0.0),     // 1 snow layer: zero-height box
            (BlockState::new(78, 3), 0.375),   // 4 snow layers
            (BlockState::new(116, 0), 0.75),   // enchantment table
            (BlockState::new(93, 1), 0.125),   // repeater
            (BlockState::new(118, 0), 0.3125), // cauldron floor (walls miss the centred box)
            (BlockState::new(53, 4), 1.0),     // upside-down stairs: flush top
        ] {
            let rest = resting_height_on(block);
            assert!(
                (rest - height).abs() < 1.0e-6,
                "{block:?}: rested at {rest}, vanilla {height}"
            );
        }
    }

    #[test]
    fn player_rests_on_fence_top_at_1_5() {
        // Also exercises the y-1 collider scan: while standing at 1.5 the
        // fence block (y=0) is below the query box's floor(min.y).
        let rest = resting_height_on(BlockState::new(85, 0));
        assert!((rest - 1.5).abs() < 1.0e-6, "fence rest height was {rest}");
    }

    #[test]
    fn player_rests_on_wall_top_at_1_5() {
        let rest = resting_height_on(BlockState::new(139, 0));
        assert!((rest - 1.5).abs() < 1.0e-6, "wall rest height was {rest}");
    }

    #[test]
    fn stairs_low_and_high_halves_have_vanilla_heights() {
        // East-facing bottom stairs: low half on x < 0.5 (height 0.5), high
        // quarter on x >= 0.5 (height 1.0).
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(53, 0));
        let physics = PlayerPhysics::default();

        let mut low = EntityState::new_local_player(EntityId(1), DVec3::new(0.18, 3.0, 0.5));
        let mut high = EntityState::new_local_player(EntityId(2), DVec3::new(0.82, 3.0, 0.5));
        for _ in 0..60 {
            physics.tick(&world, &mut low, PlayerInput::default());
            physics.tick(&world, &mut high, PlayerInput::default());
        }
        assert!(
            (low.position.y - 0.5).abs() < 1.0e-6,
            "low half was {}",
            low.position.y
        );
        assert!(
            (high.position.y - 1.0).abs() < 1.0e-6,
            "high half was {}",
            high.position.y
        );
    }

    #[test]
    fn sneak_edge_guard_sees_fence_below() {
        // Sneaking on a fence top must not shrink movement to zero only after
        // falling: the ledge probe (offset -1) has to find the fence box.
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::new(85, 0));
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.5, 0.5));
        player.on_ground = true;
        let physics = PlayerPhysics::default();
        for _ in 0..40 {
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    forward: 1.0,
                    sneak: true,
                    ..PlayerInput::default()
                },
            );
        }
        assert!(player.on_ground, "sneaking player fell off the fence");
        assert!(
            (player.position.y - 1.5).abs() < 1.0e-6,
            "sneaking player should stay on the fence top, was {}",
            player.position.y
        );
    }

    #[test]
    fn ground_speed_scales_with_the_walk_speed_attribute() {
        let mut world = World::new();
        for x in -2..=2 {
            for z in -2..=2 {
                world.set_block(x, 0, z, BlockState::STONE);
            }
        }
        let physics = PlayerPhysics::default();
        let first_tick_speed = |walk_speed: f32| {
            let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.0, 0.5));
            player.on_ground = true;
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    forward: 1.0,
                    walk_speed,
                    ..PlayerInput::default()
                },
            );
            player.position.z - 0.5
        };
        let normal = first_tick_speed(0.1);
        let potioned = first_tick_speed(0.12); // Speed I
        assert!(normal > 0.0);
        assert!(
            (potioned / normal - 1.2).abs() < 1.0e-6,
            "speed ratio was {}",
            potioned / normal
        );
    }

    #[test]
    fn flying_player_hovers_without_gravity() {
        let world = World::new();
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 40.0, 0.5));
        let physics = PlayerPhysics::default();
        for _ in 0..40 {
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    flying: true,
                    ..PlayerInput::default()
                },
            );
        }
        assert!(
            (player.position.y - 40.0).abs() < 1.0e-9,
            "hovering player drifted to {}",
            player.position.y
        );
        assert_eq!(player.velocity.y, 0.0);
    }

    #[test]
    fn flying_jump_ascends_and_decays_by_point_six() {
        let world = World::new();
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 40.0, 0.5));
        let physics = PlayerPhysics::default();
        physics.tick(
            &world,
            &mut player,
            PlayerInput {
                flying: true,
                jump: true,
                ..PlayerInput::default()
            },
        );
        // Thrust 0.05*3 = 0.15 moves the player up; velocity keeps 0.15*0.6.
        assert!((player.position.y - 40.15).abs() < 1.0e-6);
        assert!((player.velocity.y - 0.09).abs() < 1.0e-6);

        let mut sneaker = EntityState::new_local_player(EntityId(2), DVec3::new(0.5, 40.0, 0.5));
        physics.tick(
            &world,
            &mut sneaker,
            PlayerInput {
                flying: true,
                sneak: true,
                ..PlayerInput::default()
            },
        );
        assert!((sneaker.position.y - 39.85).abs() < 1.0e-6);
    }

    #[test]
    fn airborne_accel_lags_sprint_by_one_tick() {
        // Vanilla `jumpMovementFactor` is set after the move, so a tick's
        // airborne acceleration uses the PREVIOUS tick's sprint state, not the
        // current one. Grim mirrors this (`lastSprintingForSpeed`); a mid-air
        // sprint flip while brushing a block desyncs without it.
        let world = World::new(); // empty: the player stays airborne
        let physics = PlayerPhysics::default();
        // One airborne tick of forward input from rest; returns the +z velocity
        // gained, given the lagged (air_sprinting) and current (sprint) flags.
        let air_gain = |air_sprinting: bool, sprint: bool| {
            let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 40.0, 0.5));
            player.air_sprinting = air_sprinting;
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    forward: 1.0,
                    sprint,
                    ..PlayerInput::default()
                },
            );
            (player.velocity.z, player.air_sprinting)
        };

        // Per airborne tick the +z gain is 0.98 (move-input scale) × accel ×
        // 0.91 (airborne drag); accel is 0.02, or ×1.3 when sprint-boosted.
        let base = 0.98 * 0.02 * 0.91;
        let boosted = 0.98 * 0.026 * 0.91;

        // Current sprint is irrelevant to the air accel: only the lagged flag is.
        let (z_lag_off_cur_on, next) = air_gain(false, true);
        assert!(
            (z_lag_off_cur_on - base).abs() < 1.0e-6,
            "air accel must ignore current sprint when last tick wasn't sprinting, z={z_lag_off_cur_on}"
        );
        // The stored flag now reflects this tick's sprint, arming next tick.
        assert!(next, "air_sprinting must latch the current sprint after the move");

        let (z_lag_on_cur_off, _) = air_gain(true, false);
        assert!(
            (z_lag_on_cur_off - boosted).abs() < 1.0e-6,
            "air accel must keep the sprint boost the tick after sprint drops, z={z_lag_on_cur_off}"
        );
    }

    #[test]
    fn sprint_flying_accelerates_twice_as_fast() {
        let world = World::new();
        let physics = PlayerPhysics::default();
        let fly = |sprint: bool| {
            let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 40.0, 0.5));
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    flying: true,
                    forward: 1.0,
                    sprint,
                    ..PlayerInput::default()
                },
            );
            player.position.z - 0.5
        };
        let normal = fly(false);
        let sprinting = fly(true);
        assert!(normal > 0.0);
        assert!(
            (sprinting / normal - 2.0).abs() < 1.0e-6,
            "sprint-fly ratio was {}",
            sprinting / normal
        );
    }

    #[test]
    fn player_falls_through_tall_grass() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        world.set_block(0, 1, 0, BlockState::new(31, 1)); // tall grass, no collision
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 3.0, 0.5));
        let physics = PlayerPhysics::default();
        for _ in 0..60 {
            physics.tick(&world, &mut player, PlayerInput::default());
        }
        // Lands on the stone at y=1, having passed through the grass at y=1.
        assert!(
            (player.position.y - 1.0).abs() < 0.01,
            "y was {}",
            player.position.y
        );
    }

    /// A flat floor of `block` over a generous footprint at y=0.
    fn flat_floor(block: BlockState) -> World {
        let mut world = World::new();
        for x in -3..=6 {
            for z in -3..=6 {
                world.set_block(x, 0, z, block);
            }
        }
        world
    }

    /// One tick of pure horizontal coasting (no input) from 0.1 m/tick, resting
    /// at `feet_y`. Returns the retained x velocity.
    fn coast_x(world: &World, feet_y: f64) -> f64 {
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, feet_y, 0.5));
        player.on_ground = true;
        player.velocity = DVec3::new(0.1, 0.0, 0.0);
        PlayerPhysics::default().tick(world, &mut player, PlayerInput::default());
        player.velocity.x
    }

    #[test]
    fn ground_friction_varies_by_block_underfoot() {
        // Coasting drag is slipperiness * 0.91 applied to the 0.1 start speed.
        let stone = coast_x(&flat_floor(BlockState::STONE), 1.0);
        let ice = coast_x(&flat_floor(BlockState::new(79, 0)), 1.0);
        let packed_ice = coast_x(&flat_floor(BlockState::new(174, 0)), 1.0);
        let slime = coast_x(&flat_floor(BlockState::new(165, 0)), 1.0);
        assert!((stone - 0.1 * 0.6 * 0.91).abs() < 1.0e-6, "stone {stone}");
        assert!((ice - 0.1 * 0.98 * 0.91).abs() < 1.0e-6, "ice {ice}");
        assert!(
            (packed_ice - 0.1 * 0.98 * 0.91).abs() < 1.0e-6,
            "packed ice {packed_ice}"
        );
        assert!((slime - 0.1 * 0.8 * 0.91).abs() < 1.0e-6, "slime {slime}");
        assert!(
            ice > stone && slime > stone,
            "slick blocks should coast further"
        );
    }

    fn fluid_column(id: u16) -> World {
        let mut world = World::new();
        for y in -5..=8 {
            for x in -1..=1 {
                for z in -1..=1 {
                    world.set_block(x, y, z, BlockState::new(id, 0));
                }
            }
        }
        world
    }

    #[test]
    fn submerged_player_sinks_at_water_gravity() {
        let world = fluid_column(8); // still water
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 2.0, 0.5));
        PlayerPhysics::default().tick(&world, &mut player, PlayerInput::default());
        // From rest: motionY = 0 * 0.8 - 0.02.
        assert!(
            (player.velocity.y + 0.02).abs() < 1.0e-9,
            "water sink was {}",
            player.velocity.y
        );
        assert!(!player.on_ground);
    }

    #[test]
    fn water_applies_strong_vertical_drag() {
        let world = fluid_column(8);
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 2.0, 0.5));
        player.velocity.y = -0.4;
        PlayerPhysics::default().tick(&world, &mut player, PlayerInput::default());
        assert!(
            (player.velocity.y - (-0.4 * 0.8 - 0.02)).abs() < 1.0e-9,
            "water drag y was {}",
            player.velocity.y
        );
    }

    #[test]
    fn jump_in_water_pushes_up() {
        let world = fluid_column(8);
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 2.0, 0.5));
        PlayerPhysics::default().tick(
            &world,
            &mut player,
            PlayerInput {
                jump: true,
                ..PlayerInput::default()
            },
        );
        // (0 + 0.04) * 0.8 - 0.02 = 0.012.
        assert!(
            (player.velocity.y - 0.012).abs() < 1.0e-9,
            "y {}",
            player.velocity.y
        );
    }

    #[test]
    fn lava_applies_half_drag() {
        let world = fluid_column(10); // still lava
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 2.0, 0.5));
        player.velocity.y = -0.4;
        PlayerPhysics::default().tick(&world, &mut player, PlayerInput::default());
        assert!(
            (player.velocity.y - (-0.4 * 0.5 - 0.02)).abs() < 1.0e-9,
            "lava drag y was {}",
            player.velocity.y
        );
    }

    /// A single vine column (fully non-colliding) at x=z=0.
    fn vine_column() -> World {
        let mut world = World::new();
        for y in 0..=6 {
            world.set_block(0, y, 0, BlockState::new(106, 0));
        }
        world
    }

    #[test]
    fn ladder_clamps_descent_speed() {
        let world = vine_column();
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 3.0, 0.5));
        player.velocity.y = -0.5;
        let before = player.position.y;
        PlayerPhysics::default().tick(&world, &mut player, PlayerInput::default());
        // Descent is clamped to 0.15 this tick regardless of the -0.5 momentum.
        assert!(
            (before - player.position.y - 0.15).abs() < 1.0e-9,
            "descended {}",
            before - player.position.y
        );
    }

    #[test]
    fn sneaking_on_ladder_stops_descent() {
        let world = vine_column();
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 3.0, 0.5));
        player.velocity.y = -0.5;
        let before = player.position.y;
        PlayerPhysics::default().tick(
            &world,
            &mut player,
            PlayerInput {
                sneak: true,
                ..PlayerInput::default()
            },
        );
        assert!(
            (player.position.y - before).abs() < 1.0e-9,
            "sneaking should cling to the ladder, moved {}",
            before - player.position.y
        );
    }

    #[test]
    fn pressing_into_a_ladder_climbs() {
        let mut world = vine_column();
        for y in 0..=6 {
            world.set_block(0, y, 1, BlockState::STONE); // wall on the +z side
        }
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 3.0, 0.7));
        player.yaw = 0.0; // forward drives +z, into the wall
        PlayerPhysics::default().tick(
            &world,
            &mut player,
            PlayerInput {
                forward: 1.0,
                ..PlayerInput::default()
            },
        );
        // Colliding horizontally on a ladder bumps motionY to 0.2 (0.1176 after
        // this tick's gravity and drag), so the player climbs.
        assert!(
            player.velocity.y > 0.1,
            "should climb, velocity.y was {}",
            player.velocity.y
        );
    }

    #[test]
    fn cobweb_drastically_slows_falling() {
        let mut world = World::new();
        for y in 0..=12 {
            world.set_block(0, y, 0, BlockState::new(30, 0)); // cobweb
        }
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 8.0, 0.5));
        let physics = PlayerPhysics::default();
        let start = player.position.y;
        for _ in 0..40 {
            physics.tick(&world, &mut player, PlayerInput::default());
        }
        assert!(player.in_web, "player should be flagged as stuck in web");
        assert!(
            start - player.position.y < 0.5,
            "web fall too fast: descended {}",
            start - player.position.y
        );
    }

    #[test]
    fn soul_sand_quarters_horizontal_speed() {
        // Both stand on a 0.6-slipperiness surface, so only the soul-sand 0.4
        // inside-block multiplier should differ. Soul sand rests the feet at its
        // 0.875 top; stone at 1.0.
        let stone = coast_x(&flat_floor(BlockState::STONE), 1.0);
        let soul = coast_x(&flat_floor(BlockState::new(88, 0)), 0.875);
        assert!(
            (soul - stone * 0.4).abs() < 1.0e-9,
            "soul {soul} vs stone {stone}"
        );
    }

    #[test]
    fn slime_block_bounces_a_falling_player() {
        let mut world = World::new();
        for x in -1..=1 {
            for z in -1..=1 {
                world.set_block(x, 0, z, BlockState::new(165, 0)); // slime
            }
        }
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 4.0, 0.5));
        let physics = PlayerPhysics::default();
        let mut landed = false;
        let mut max_upward = f64::MIN;
        for _ in 0..80 {
            physics.tick(&world, &mut player, PlayerInput::default());
            if player.position.y <= 1.5 {
                landed = true;
            }
            if landed {
                max_upward = max_upward.max(player.velocity.y);
            }
        }
        assert!(
            max_upward > 0.3,
            "player should rebound off slime, peak upward velocity {max_upward}"
        );
    }

    /// Whether the player box penetrates any block collision box by more than a
    /// hair (1e-4) — i.e. is genuinely clipped inside, not merely resting against
    /// a face.
    fn deeply_penetrating(world: &World, aabb: Aabb) -> bool {
        let probe = Aabb::new(
            aabb.min + DVec3::splat(1.0e-4),
            aabb.max - DVec3::splat(1.0e-4),
        );
        !colliding_boxes(world, probe).is_empty()
    }

    /// Sealed 5x5 room (interior x,z in [0,5]) walled with `wall`, 5 blocks tall.
    fn sealed_room(wall: BlockState) -> World {
        let mut world = World::new();
        for x in -1..=5 {
            for z in -1..=5 {
                world.set_block(x, 0, z, BlockState::STONE);
            }
        }
        for y in 1..=5 {
            for a in -1..=5 {
                world.set_block(a, y, -1, wall);
                world.set_block(a, y, 5, wall);
                world.set_block(-1, y, a, wall);
                world.set_block(5, y, a, wall);
            }
        }
        world
    }

    #[test]
    fn fuzz_full_block_walls_are_never_clipped() {
        // Full solid cubes: the 0.6-wide player must stay strictly inside the
        // interior [0.3, 4.7] at every tick, for every launch angle and speed.
        let physics = PlayerPhysics::default();
        for &wall in &[
            BlockState::STONE,
            BlockState::new(20, 0),  // glass
            BlockState::new(98, 0),  // stone bricks
            BlockState::new(49, 0),  // obsidian
        ] {
            let world = sealed_room(wall);
            for angle in (0..360).step_by(12) {
                for &speed in &[0.5, 1.0, 2.0, 4.0, 8.0, 16.0] {
                    let mut player =
                        EntityState::new_local_player(EntityId(1), DVec3::new(2.5, 1.0, 2.5));
                    player.on_ground = true;
                    player.yaw = angle as f32;
                    let rad = (angle as f64).to_radians();
                    player.velocity = DVec3::new(speed * rad.cos(), 0.0, speed * rad.sin());
                    for tick in 0..50 {
                        physics.tick(
                            &world,
                            &mut player,
                            PlayerInput {
                                forward: 1.0,
                                sprint: true,
                                ..PlayerInput::default()
                            },
                        );
                        assert!(
                            player.position.x > 0.299
                                && player.position.x < 4.701
                                && player.position.z > 0.299
                                && player.position.z < 4.701
                                && !deeply_penetrating(&world, player.aabb),
                            "clip: wall {:?} angle {angle} speed {speed} tick {tick} pos {:?}",
                            wall,
                            player.position,
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fuzz_thin_walls_are_never_clipped_through() {
        // Fences/panes/walls/bars don't fill their cell, so the player may dip
        // partway in; what must never happen is full passage to the far side or
        // deep penetration of an actual collision box.
        let physics = PlayerPhysics::default();
        for &wall in &[
            BlockState::new(85, 0),  // oak fence
            BlockState::new(102, 0), // glass pane
            BlockState::new(101, 0), // iron bars
            BlockState::new(139, 0), // cobblestone wall
            BlockState::new(53, 0),  // oak stairs
        ] {
            let world = sealed_room(wall);
            for angle in (0..360).step_by(12) {
                for &speed in &[0.5, 1.0, 2.0, 4.0, 8.0, 16.0] {
                    let mut player =
                        EntityState::new_local_player(EntityId(1), DVec3::new(2.5, 1.0, 2.5));
                    player.on_ground = true;
                    player.yaw = angle as f32;
                    let rad = (angle as f64).to_radians();
                    player.velocity = DVec3::new(speed * rad.cos(), 0.0, speed * rad.sin());
                    for tick in 0..50 {
                        physics.tick(
                            &world,
                            &mut player,
                            PlayerInput {
                                forward: 1.0,
                                sprint: true,
                                ..PlayerInput::default()
                            },
                        );
                        // The far face of the perimeter wall cell is at ±0/6; if
                        // the centre reaches past the wall cell entirely it has
                        // tunnelled.
                        assert!(
                            player.position.x > -0.51
                                && player.position.x < 5.51
                                && player.position.z > -0.51
                                && player.position.z < 5.51
                                && !deeply_penetrating(&world, player.aabb),
                            "tunnel: wall {:?} angle {angle} speed {speed} tick {tick} pos {:?}",
                            wall,
                            player.position,
                        );
                    }
                }
            }
        }
    }

    fn single_block_platform() -> World {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        world
    }

    /// A flat stone floor at y=0 over x,z in -2..=8, with a wall `height` tall
    /// standing at x=3 across z in -2..=2.
    fn floor_with_wall(height: i32) -> World {
        let mut world = World::new();
        for x in -2..=8 {
            for z in -4..=4 {
                world.set_block(x, 0, z, BlockState::STONE);
            }
        }
        for y in 1..=height {
            for z in -4..=4 {
                world.set_block(3, y, z, BlockState::STONE);
            }
        }
        world
    }

    fn player_overlaps_solid(world: &World, player: &EntityState) -> bool {
        let aabb = player.aabb;
        let min_x = aabb.min.x.floor() as i32;
        let max_x = (aabb.max.x - 1.0e-7).floor() as i32;
        let min_y = aabb.min.y.floor() as i32;
        let max_y = (aabb.max.y - 1.0e-7).floor() as i32;
        let min_z = aabb.min.z.floor() as i32;
        let max_z = (aabb.max.z - 1.0e-7).floor() as i32;
        for x in min_x..=max_x {
            for y in min_y..=max_y {
                for z in min_z..=max_z {
                    if world.block_at(x, y, z).is_solid_collision() {
                        return true;
                    }
                }
            }
        }
        false
    }

    // At yaw 0, `strafe` drives +x (toward the x=3 wall) and `forward` drives +z.
    // The wall spans z -4..=4, so a large strafe-dominant push tests head-on
    // collision while keeping the player within the wall's extent.
    fn walk_into_wall(
        start: DVec3,
        forward: f32,
        strafe: f32,
        sprint: bool,
        ticks: usize,
    ) -> (World, EntityState) {
        let world = floor_with_wall(3);
        let mut player = EntityState::new_local_player(EntityId(1), start);
        player.on_ground = true;
        let physics = PlayerPhysics::default();
        for _ in 0..ticks {
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    forward,
                    strafe,
                    sprint,
                    ..PlayerInput::default()
                },
            );
        }
        (world, player)
    }

    #[test]
    fn player_box_is_full_height_and_width() {
        let player = EntityState::new_local_player(EntityId(1), DVec3::new(0.0, 0.0, 0.0));
        assert!((player.aabb.max.y - player.aabb.min.y - 1.8).abs() < 1.0e-9);
        assert!((player.aabb.max.x - player.aabb.min.x - 0.6).abs() < 1.0e-9);
        assert!((player.aabb.max.z - player.aabb.min.z - 0.6).abs() < 1.0e-9);
    }

    #[test]
    fn walking_into_wall_does_not_clip_through() {
        // strafe=+1 => pure +x into the wall at x=3.
        let (world, player) = walk_into_wall(DVec3::new(0.5, 1.0, 0.5), 0.0, 1.0, false, 80);
        assert!(
            !player_overlaps_solid(&world, &player),
            "player ended inside wall at {:?}",
            player.position
        );
        assert!(
            player.position.x < 2.7 + 1.0e-6,
            "player clipped past wall face: x={}",
            player.position.x
        );
    }

    #[test]
    fn sprinting_into_wall_does_not_clip_through() {
        let (world, player) = walk_into_wall(DVec3::new(0.5, 1.0, 0.5), 0.0, 1.0, true, 120);
        assert!(
            !player_overlaps_solid(&world, &player),
            "sprinting player ended inside wall at {:?}",
            player.position
        );
        assert!(
            player.position.x < 2.7 + 1.0e-6,
            "sprinting player clipped past wall: x={}",
            player.position.x
        );
    }

    #[test]
    fn sprinting_cannot_escape_enclosed_room() {
        // A sealed 1..=2 walled room (floor at y=0, 5-tall walls). Sprinting in
        // every direction for many ticks must never push the player outside.
        let mut world = World::new();
        for x in -1..=5 {
            for z in -1..=5 {
                world.set_block(x, 0, z, BlockState::STONE);
            }
        }
        for y in 1..=5 {
            for x in -1..=5 {
                world.set_block(x, y, -1, BlockState::STONE);
                world.set_block(x, y, 5, BlockState::STONE);
                world.set_block(-1, y, x, BlockState::STONE);
                world.set_block(5, y, x, BlockState::STONE);
            }
        }
        let physics = PlayerPhysics::default();
        for &(yaw, fwd, strafe) in &[
            (0.0_f32, 1.0_f32, 0.0_f32),
            (0.0, 0.0, 1.0),
            (0.0, 1.0, 1.0),
            (45.0, 1.0, 0.0),
            (135.0, 1.0, 1.0),
            (250.0, 1.0, -1.0),
        ] {
            let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(2.0, 1.0, 2.0));
            player.on_ground = true;
            player.yaw = yaw;
            for _ in 0..200 {
                physics.tick(
                    &world,
                    &mut player,
                    PlayerInput {
                        forward: fwd,
                        strafe,
                        sprint: true,
                        ..PlayerInput::default()
                    },
                );
            }
            assert!(
                !player_overlaps_solid(&world, &player),
                "player escaped/clipped (yaw {yaw}) to {:?}",
                player.position
            );
            // Interior free space for the 0.6-wide box is x,z in [0.3, 4.7].
            assert!(
                player.position.x > 0.29 && player.position.x < 4.71,
                "player x escaped: {}",
                player.position.x
            );
            assert!(
                player.position.z > 0.29 && player.position.z < 4.71,
                "player z escaped: {}",
                player.position.z
            );
        }
    }

    #[test]
    fn head_block_is_solid_against_body() {
        // A block only at head height (y=2) with floor at y=0; walking forward the
        // upper body must collide and not pass through.
        let mut world = World::new();
        for x in -2..=8 {
            for z in -2..=2 {
                world.set_block(x, 0, z, BlockState::STONE);
            }
        }
        for z in -2..=2 {
            world.set_block(3, 2, z, BlockState::STONE);
        }
        let mut player = EntityState::new_local_player(EntityId(1), DVec3::new(0.5, 1.0, 0.5));
        player.on_ground = true;
        let physics = PlayerPhysics::default();
        for _ in 0..80 {
            physics.tick(
                &world,
                &mut player,
                PlayerInput {
                    forward: 1.0,
                    ..PlayerInput::default()
                },
            );
        }
        assert!(
            !player_overlaps_solid(&world, &player),
            "player clipped into head block at {:?}",
            player.position
        );
        assert!(
            player.position.x < 2.7 + 1.0e-6,
            "player passed under/through head block: x={}",
            player.position.x
        );
    }

    fn flat_world_with_one_block_step() -> World {
        let mut world = World::new();
        for x in -2..=2 {
            for z in -2..=4 {
                world.set_block(x, 0, z, BlockState::STONE);
            }
        }
        world.set_block(0, 1, 2, BlockState::STONE);
        world
    }
}
