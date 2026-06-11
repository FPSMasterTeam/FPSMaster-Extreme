use glam::Vec3;

use crate::{EntityState, World};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    pub fn player_at(feet: Vec3) -> Self {
        let half_width = 0.3;
        Self {
            min: Vec3::new(feet.x - half_width, feet.y, feet.z - half_width),
            max: Vec3::new(feet.x + half_width, feet.y + 1.8, feet.z + half_width),
        }
    }

    pub fn offset(self, delta: Vec3) -> Self {
        Self {
            min: self.min + delta,
            max: self.max + delta,
        }
    }

    pub fn intersects(self, other: Self) -> bool {
        self.max.x > other.min.x
            && self.min.x < other.max.x
            && self.max.y > other.min.y
            && self.min.y < other.max.y
            && self.max.z > other.min.z
            && self.min.z < other.max.z
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
    pub gravity: f32,
    pub jump_velocity: f32,
    pub air_drag_y: f32,
    pub air_acceleration: f32,
    pub ground_acceleration: f32,
    pub base_walk_speed: f32,
    pub sprint_multiplier: f32,
    pub default_block_slipperiness: f32,
}

impl Default for PlayerPhysicsConfig {
    fn default() -> Self {
        // These constants match the broad shape of the 1.8.9 EntityLivingBase path.
        // Exact parity will be verified against MCP/black-box traces before the goal is complete.
        Self {
            gravity: 0.08,
            jump_velocity: 0.42,
            air_drag_y: 0.98,
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

        if input.jump && player.on_ground {
            velocity.y = self.config.jump_velocity;
        }

        let horizontal_drag = if player.on_ground {
            self.config.default_block_slipperiness * 0.91
        } else {
            0.91
        };

        let mut acceleration = if player.on_ground {
            self.config.ground_acceleration
                * (0.16277136 / (horizontal_drag * horizontal_drag * horizontal_drag))
        } else {
            self.config.air_acceleration
        };
        acceleration *= if input.sprint {
            self.config.sprint_multiplier
        } else {
            1.0
        };
        if input.sneak {
            acceleration *= 0.3;
        }

        velocity += movement_vector(input.forward, input.strafe, player.yaw) * acceleration;

        let (position, mut adjusted_velocity, on_ground) =
            move_with_collisions(world, player.aabb, velocity);
        adjusted_velocity.y -= self.config.gravity;
        adjusted_velocity.y *= self.config.air_drag_y;
        adjusted_velocity.x *= horizontal_drag;
        adjusted_velocity.z *= horizontal_drag;

        player.position = position;
        player.velocity = adjusted_velocity;
        player.on_ground = on_ground;
        player.sync_aabb_to_position();
    }
}

fn movement_vector(forward: f32, strafe: f32, yaw_degrees: f32) -> Vec3 {
    let mut length = strafe * strafe + forward * forward;
    if length < 1.0e-4 {
        return Vec3::ZERO;
    }
    length = length.sqrt();
    if length < 1.0 {
        length = 1.0;
    }

    let strafe = strafe / length;
    let forward = forward / length;
    let yaw = yaw_degrees.to_radians();
    let sin = yaw.sin();
    let cos = yaw.cos();
    Vec3::new(
        strafe * cos - forward * sin,
        0.0,
        forward * cos + strafe * sin,
    )
}

fn move_with_collisions(world: &World, aabb: Aabb, velocity: Vec3) -> (Vec3, Vec3, bool) {
    let mut moved = aabb;
    let mut adjusted = velocity;
    let mut on_ground = false;

    adjusted.y = clip_axis(world, moved, adjusted.y, Axis::Y);
    moved = moved.offset(Vec3::new(0.0, adjusted.y, 0.0));
    if velocity.y < 0.0 && adjusted.y != velocity.y {
        on_ground = true;
    }

    adjusted.x = clip_axis(world, moved, adjusted.x, Axis::X);
    moved = moved.offset(Vec3::new(adjusted.x, 0.0, 0.0));

    adjusted.z = clip_axis(world, moved, adjusted.z, Axis::Z);
    moved = moved.offset(Vec3::new(0.0, 0.0, adjusted.z));

    let feet = Vec3::new(
        (moved.min.x + moved.max.x) * 0.5,
        moved.min.y,
        (moved.min.z + moved.max.z) * 0.5,
    );
    (feet, adjusted, on_ground)
}

#[derive(Debug, Clone, Copy)]
enum Axis {
    X,
    Y,
    Z,
}

fn clip_axis(world: &World, aabb: Aabb, delta: f32, axis: Axis) -> f32 {
    if delta == 0.0 {
        return 0.0;
    }

    let moved = match axis {
        Axis::X => aabb.offset(Vec3::new(delta, 0.0, 0.0)),
        Axis::Y => aabb.offset(Vec3::new(0.0, delta, 0.0)),
        Axis::Z => aabb.offset(Vec3::new(0.0, 0.0, delta)),
    };

    let min_x = moved.min.x.floor() as i32;
    let max_x = moved.max.x.ceil() as i32;
    let min_y = moved.min.y.floor() as i32;
    let max_y = moved.max.y.ceil() as i32;
    let min_z = moved.min.z.floor() as i32;
    let max_z = moved.max.z.ceil() as i32;

    let mut clipped = delta;
    for x in min_x..max_x {
        for y in min_y..max_y {
            for z in min_z..max_z {
                if !world.block_at(x, y, z).is_solid_collision() {
                    continue;
                }
                let block = Aabb::new(
                    Vec3::new(x as f32, y as f32, z as f32),
                    Vec3::new(x as f32 + 1.0, y as f32 + 1.0, z as f32 + 1.0),
                );
                if !moved.intersects(block) {
                    continue;
                }

                clipped = match axis {
                    Axis::X if delta > 0.0 => clipped.min(block.min.x - aabb.max.x),
                    Axis::X => clipped.max(block.max.x - aabb.min.x),
                    Axis::Y if delta > 0.0 => clipped.min(block.min.y - aabb.max.y),
                    Axis::Y => clipped.max(block.max.y - aabb.min.y),
                    Axis::Z if delta > 0.0 => clipped.min(block.min.z - aabb.max.z),
                    Axis::Z => clipped.max(block.max.z - aabb.min.z),
                };
            }
        }
    }
    clipped
}

#[cfg(test)]
mod tests {
    use glam::Vec3;

    use super::*;
    use crate::{BlockState, EntityId};

    #[test]
    fn falling_player_lands_on_solid_block() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let mut player = EntityState::new_local_player(EntityId(1), Vec3::new(0.5, 1.2, 0.5));
        player.velocity = Vec3::new(0.0, -0.5, 0.0);

        PlayerPhysics::default().tick(&world, &mut player, PlayerInput::default());

        assert!(player.on_ground);
        assert!((player.position.y - 1.0).abs() < 0.001);
    }

    #[test]
    fn movement_forward_matches_minecraft_yaw_convention() {
        let forward_at_zero = movement_vector(1.0, 0.0, 0.0);
        assert!(forward_at_zero.z > 0.99);
        assert!(forward_at_zero.x.abs() < 0.001);

        let forward_at_ninety = movement_vector(1.0, 0.0, 90.0);
        assert!(forward_at_ninety.x < -0.99);
        assert!(forward_at_ninety.z.abs() < 0.001);

        let right_at_zero = movement_vector(0.0, 1.0, 0.0);
        assert!(right_at_zero.x > 0.99);
        assert!(right_at_zero.z.abs() < 0.001);
    }

    #[test]
    fn jump_moves_before_gravity_drag_for_tick() {
        let world = World::new();
        let mut player = EntityState::new_local_player(EntityId(1), Vec3::new(0.5, 1.0, 0.5));
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
}
