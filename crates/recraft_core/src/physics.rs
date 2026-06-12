use glam::DVec3;

use crate::mc_math::{mc_cos, mc_sin, DEG_TO_RAD};
use crate::{EntityState, World};

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

    fn calculate_x_offset(self, other: Self, mut offset: f64) -> f64 {
        if other.max.y > self.min.y
            && other.min.y < self.max.y
            && other.max.z > self.min.z
            && other.min.z < self.max.z
        {
            if offset > 0.0 && other.max.x <= self.min.x {
                let limit = self.min.x - other.max.x;
                if limit < offset {
                    offset = limit;
                }
            } else if offset < 0.0 && other.min.x >= self.max.x {
                let limit = self.max.x - other.min.x;
                if limit > offset {
                    offset = limit;
                }
            }
        }
        offset
    }

    fn calculate_y_offset(self, other: Self, mut offset: f64) -> f64 {
        if other.max.x > self.min.x
            && other.min.x < self.max.x
            && other.max.z > self.min.z
            && other.min.z < self.max.z
        {
            if offset > 0.0 && other.max.y <= self.min.y {
                let limit = self.min.y - other.max.y;
                if limit < offset {
                    offset = limit;
                }
            } else if offset < 0.0 && other.min.y >= self.max.y {
                let limit = self.max.y - other.min.y;
                if limit > offset {
                    offset = limit;
                }
            }
        }
        offset
    }

    fn calculate_z_offset(self, other: Self, mut offset: f64) -> f64 {
        if other.max.x > self.min.x
            && other.min.x < self.max.x
            && other.max.y > self.min.y
            && other.min.y < self.max.y
        {
            if offset > 0.0 && other.max.z <= self.min.z {
                let limit = self.min.z - other.max.z;
                if limit < offset {
                    offset = limit;
                }
            } else if offset < 0.0 && other.min.z >= self.max.z {
                let limit = self.max.z - other.min.z;
                if limit > offset {
                    offset = limit;
                }
            }
        }
        offset
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PlayerInput {
    pub forward: f32,
    pub strafe: f32,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
}

impl Default for PlayerInput {
    fn default() -> Self {
        Self {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: false,
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
    pub base_walk_speed: f32,
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
            base_walk_speed: 0.1,
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

        if velocity.x.abs() < 0.005 {
            velocity.x = 0.0;
        }
        if velocity.y.abs() < 0.005 {
            velocity.y = 0.0;
        }
        if velocity.z.abs() < 0.005 {
            velocity.z = 0.0;
        }

        if input.jump && player.on_ground {
            velocity.y = self.config.jump_velocity;
            if input.sprint {
                // Vanilla sprint-jump boost: 0.2 in the facing direction, using
                // MathHelper trig (not libm) so the direction matches the server.
                let yaw = player.yaw * DEG_TO_RAD;
                velocity.x -= (mc_sin(yaw) * 0.2) as f64;
                velocity.z += (mc_cos(yaw) * 0.2) as f64;
            }
        }

        let horizontal_drag = if player.on_ground {
            self.config.default_block_slipperiness * 0.91
        } else {
            0.91
        };

        let move_speed = self.config.base_walk_speed
            * if input.sprint {
                self.config.sprint_multiplier
            } else {
                1.0
            };
        let acceleration = if player.on_ground {
            move_speed * (0.16277136 / (horizontal_drag * horizontal_drag * horizontal_drag))
        } else {
            self.config.air_acceleration
                * if input.sprint {
                    self.config.sprint_multiplier
                } else {
                    1.0
                }
        };

        let mut forward = input.forward;
        let mut strafe = input.strafe;
        if input.sneak {
            forward *= 0.3;
            strafe *= 0.3;
        }
        forward *= 0.98;
        strafe *= 0.98;

        velocity += movement_vector(forward, strafe, player.yaw, acceleration);

        // Vanilla 1.8 sneak edge protection: while sneaking on the ground, the
        // intended horizontal movement is shrunk in 0.05 steps until the player
        // box (lowered by one block) would still rest on a collider, so the
        // player never walks off a ledge.
        if input.sneak && player.on_ground {
            let (clamped_x, clamped_z) =
                clamp_sneak_to_edges(world, player.aabb, velocity.x, velocity.z);
            velocity.x = clamped_x;
            velocity.z = clamped_z;
        }

        let result = move_with_collisions(
            world,
            player.aabb,
            velocity,
            self.config.step_height,
            player.on_ground,
        );
        let mut adjusted_velocity = result.velocity;
        adjusted_velocity.y -= self.config.gravity;
        adjusted_velocity.y *= self.config.air_drag_y;
        adjusted_velocity.x *= horizontal_drag as f64;
        adjusted_velocity.z *= horizontal_drag as f64;

        player.position = result.feet;
        player.velocity = adjusted_velocity;
        player.on_ground = result.on_ground;
        player.collided_horizontally = result.collided_horizontally;
        player.sync_aabb_to_position();
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
    }
}

struct MoveResult {
    feet: DVec3,
    velocity: DVec3,
    on_ground: bool,
    collided_horizontally: bool,
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
    use crate::{BlockState, EntityId};

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
