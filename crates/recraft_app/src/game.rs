use glam::Vec3;
use recraft_core::{BlockState, EntityId, EntityState, PlayerInput, PlayerPhysics, World};
use recraft_protocol::v1_8_9::{chunk::decode_chunk_data, packets::ClientboundPlayPacket};
use recraft_render::Camera;
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, PhysicalKey},
};

#[derive(Debug, Clone, Copy)]
pub struct MovementSnapshot {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub on_ground: bool,
}

#[derive(Default)]
pub struct InputState {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    jump: bool,
    sneak: bool,
    sprint: bool,
    turn_left: bool,
    turn_right: bool,
    look_up: bool,
    look_down: bool,
}

impl InputState {
    pub fn handle_key(&mut self, event: KeyEvent) {
        let pressed = event.state == ElementState::Pressed;
        match event.physical_key {
            PhysicalKey::Code(KeyCode::KeyW) => self.forward = pressed,
            PhysicalKey::Code(KeyCode::KeyS) => self.backward = pressed,
            PhysicalKey::Code(KeyCode::KeyA) => self.left = pressed,
            PhysicalKey::Code(KeyCode::KeyD) => self.right = pressed,
            PhysicalKey::Code(KeyCode::Space) => self.jump = pressed,
            PhysicalKey::Code(KeyCode::ShiftLeft) => self.sneak = pressed,
            PhysicalKey::Code(KeyCode::ControlLeft) => self.sprint = pressed,
            PhysicalKey::Code(KeyCode::ArrowLeft) => self.turn_left = pressed,
            PhysicalKey::Code(KeyCode::ArrowRight) => self.turn_right = pressed,
            PhysicalKey::Code(KeyCode::ArrowUp) => self.look_up = pressed,
            PhysicalKey::Code(KeyCode::ArrowDown) => self.look_down = pressed,
            _ => {}
        }
    }

    fn player_input(&self) -> PlayerInput {
        PlayerInput {
            forward: f32::from(self.forward) - f32::from(self.backward),
            strafe: f32::from(self.left) - f32::from(self.right),
            jump: self.jump,
            sneak: self.sneak,
            sprint: self.sprint,
        }
    }
}

pub struct GameState {
    pub world: World,
    pub input: InputState,
    pub camera: Camera,
    player: EntityState,
    physics: PlayerPhysics,
    has_sky_light: bool,
}

impl GameState {
    pub fn demo(aspect: f32) -> Self {
        let mut world = World::new();
        build_demo_world(&mut world);
        Self::new(world, EntityId(0), Vec3::new(0.5, 2.0, 0.5), aspect)
    }

    pub fn empty_for_server(aspect: f32) -> Self {
        Self::new(World::new(), EntityId(0), Vec3::new(0.5, 80.0, 0.5), aspect)
    }

    fn new(mut world: World, player_id: EntityId, position: Vec3, aspect: f32) -> Self {
        let player = EntityState::new_local_player(player_id, position);
        world.upsert_entity(player.clone());
        let mut camera = Camera::new(position + Vec3::new(0.0, 1.62, 0.0), aspect);
        camera.yaw = 0.0;
        camera.pitch = 0.0;
        Self {
            world,
            input: InputState::default(),
            camera,
            player,
            physics: PlayerPhysics::default(),
            has_sky_light: true,
        }
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        self.camera.aspect = aspect;
    }

    pub fn tick(&mut self, dt: f32) -> MovementSnapshot {
        let turn_speed = 110.0 * dt;
        if self.input.turn_left {
            self.player.yaw += turn_speed;
        }
        if self.input.turn_right {
            self.player.yaw -= turn_speed;
        }
        if self.input.look_up {
            self.player.pitch = (self.player.pitch + turn_speed).min(89.0);
        }
        if self.input.look_down {
            self.player.pitch = (self.player.pitch - turn_speed).max(-89.0);
        }

        self.physics
            .tick(&self.world, &mut self.player, self.input.player_input());
        self.world.upsert_entity(self.player.clone());
        self.camera.position = self.player.position + Vec3::new(0.0, 1.62, 0.0);
        self.camera.yaw = self.player.yaw;
        self.camera.pitch = self.player.pitch;
        self.movement_snapshot()
    }

    pub fn apply_play_packet(&mut self, packet: ClientboundPlayPacket) -> bool {
        match packet {
            ClientboundPlayPacket::JoinGame {
                entity_id,
                dimension,
                ..
            } => {
                self.player.id = EntityId(entity_id);
                self.has_sky_light = dimension == 0;
                self.world.upsert_entity(self.player.clone());
                false
            }
            ClientboundPlayPacket::PlayerPositionLook {
                x,
                y,
                z,
                yaw,
                pitch,
                flags,
            } => {
                if flags & 0x01 != 0 {
                    self.player.position.x += x as f32;
                } else {
                    self.player.position.x = x as f32;
                }
                if flags & 0x02 != 0 {
                    self.player.position.y += y as f32;
                } else {
                    self.player.position.y = y as f32;
                }
                if flags & 0x04 != 0 {
                    self.player.position.z += z as f32;
                } else {
                    self.player.position.z = z as f32;
                }
                if flags & 0x08 != 0 {
                    self.player.yaw += yaw;
                } else {
                    self.player.yaw = yaw;
                }
                if flags & 0x10 != 0 {
                    self.player.pitch += pitch;
                } else {
                    self.player.pitch = pitch;
                }
                self.player.sync_aabb_to_position();
                self.world.upsert_entity(self.player.clone());
                false
            }
            ClientboundPlayPacket::ChunkData {
                x,
                z,
                ground_up,
                primary_bit_mask,
                data,
            } => {
                self.apply_chunk_data(x, z, ground_up, primary_bit_mask, &data, self.has_sky_light)
            }
            ClientboundPlayPacket::ChunkBulk {
                sky_light_sent,
                chunks,
            } => {
                let mut changed = false;
                let count = chunks.len();
                for chunk in chunks {
                    changed |= self.apply_chunk_data(
                        chunk.x,
                        chunk.z,
                        true,
                        chunk.primary_bit_mask,
                        &chunk.data,
                        sky_light_sent,
                    );
                }
                if changed {
                    log::info!("applied chunk bulk: {count} chunks");
                }
                changed
            }
            ClientboundPlayPacket::Disconnect { reason_json } => {
                log::warn!("server disconnected: {reason_json}");
                false
            }
            ClientboundPlayPacket::KeepAlive { .. } | ClientboundPlayPacket::Unknown { .. } => {
                false
            }
        }
    }

    fn apply_chunk_data(
        &mut self,
        x: i32,
        z: i32,
        ground_up: bool,
        primary_bit_mask: u16,
        data: &[u8],
        has_sky_light: bool,
    ) -> bool {
        match decode_chunk_data(data, primary_bit_mask, ground_up, has_sky_light) {
            Ok(decoded) => {
                for section in decoded.sections {
                    for block in section.blocks {
                        let wx = x * 16 + block.x as i32;
                        let wy = section.y as i32 * 16 + block.y as i32;
                        let wz = z * 16 + block.z as i32;
                        self.world
                            .set_block(wx, wy, wz, BlockState::new(block.id, block.meta));
                    }
                }
                true
            }
            Err(err) => {
                log::warn!("failed to decode chunk {x},{z}: {err}");
                false
            }
        }
    }

    fn movement_snapshot(&self) -> MovementSnapshot {
        MovementSnapshot {
            x: self.player.position.x as f64,
            y: self.player.position.y as f64,
            z: self.player.position.z as f64,
            on_ground: self.player.on_ground,
        }
    }
}

fn build_demo_world(world: &mut World) {
    for x in -16..32 {
        for z in -16..32 {
            world.set_block(x, 0, z, BlockState::GRASS);
            for y in 1..4 {
                if (x + z + y) % 17 == 0 {
                    world.set_block(x, y, z, BlockState::STONE);
                }
            }
        }
    }
    for x in 4..10 {
        for y in 1..5 {
            for z in 4..10 {
                if x == 4 || x == 9 || z == 4 || z == 9 || y == 4 {
                    world.set_block(x, y, z, BlockState::STONE);
                }
            }
        }
    }
    for y in 1..7 {
        world.set_block(-4, y, -4, BlockState::new(17, 0));
    }
    for x in -7..0 {
        for y in 5..9 {
            for z in -7..0 {
                let dx = x + 4;
                let dy = y - 7;
                let dz = z + 4;
                if dx * dx + dy * dy + dz * dz < 12 {
                    world.set_block(x, y, z, BlockState::new(18, 0));
                }
            }
        }
    }
}
