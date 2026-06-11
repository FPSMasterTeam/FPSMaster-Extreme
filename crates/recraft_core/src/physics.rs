use glam::DVec3;

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
                let yaw = player.yaw.to_radians();
                velocity.x -= (yaw.sin() * 0.2) as f64;
                velocity.z += (yaw.cos() * 0.2) as f64;
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

        let (position, mut adjusted_velocity, on_ground) = move_with_collisions(
            world,
            player.aabb,
            velocity,
            self.config.step_height,
            player.on_ground,
        );
        adjusted_velocity.y -= self.config.gravity;
        adjusted_velocity.y *= self.config.air_drag_y;
        adjusted_velocity.x *= horizontal_drag as f64;
        adjusted_velocity.z *= horizontal_drag as f64;

        player.position = position;
        player.velocity = adjusted_velocity;
        player.on_ground = on_ground;
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
    let yaw = yaw_degrees.to_radians();
    let sin = yaw.sin();
    let cos = yaw.cos();
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
) -> (DVec3, DVec3, bool) {
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
    let on_ground = original_y != y && original_y < 0.0;

    (feet, adjusted_velocity, on_ground)
}

fn colliding_boxes(world: &World, query: Aabb) -> Vec<Aabb> {
    let min_x = query.min.x.floor() as i32;
    let max_x = query.max.x.ceil() as i32;
    let min_y = query.min.y.floor() as i32;
    let max_y = query.max.y.ceil() as i32;
    let min_z = query.min.z.floor() as i32;
    let max_z = query.max.z.ceil() as i32;
    let mut colliders = Vec::new();

    for x in min_x..max_x {
        for y in min_y..max_y {
            for z in min_z..max_z {
                if !world.block_at(x, y, z).is_solid_collision() {
                    continue;
                }
                let block = Aabb::new(
                    DVec3::new(x as f64, y as f64, z as f64),
                    DVec3::new(x as f64 + 1.0, y as f64 + 1.0, z as f64 + 1.0),
                );
                if block.intersects(query) {
                    colliders.push(block);
                }
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
