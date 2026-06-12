use std::collections::HashSet;

use glam::{DVec3, Vec3};
use recraft_core::{
    resting_on_ground, BlockState, ChunkPos, EntityId, EntityKind, EntityState, PlayerInput,
    PlayerPhysics, World,
};
use recraft_protocol::v1_8_9::{
    chunk::{decode_chunk_column, ChunkColumnData},
    packets::{ClientboundPlayPacket, DiggingStatus, ServerboundPacket, SlotItem, UseEntityKind},
};
use recraft_render::{Camera, ModelMesh};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, PhysicalKey},
};

use crate::chat::{self, ChatState};
use crate::scoreboard::Scoreboard;

#[derive(Debug, Clone, Copy)]
pub struct MovementSnapshot {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub entity_id: i32,
    pub sneaking: bool,
    pub sprinting: bool,
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
            // Sprint is a toggle (not hold): each press of the sprint key flips
            // the intent. It is cleared by the wall-cancel and by sneaking.
            PhysicalKey::Code(KeyCode::ControlLeft) if pressed => self.sprint = !self.sprint,
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
            // Match vanilla 1.8 MovementInput semantics: left is positive,
            // right is negative. Entity.moveFlying then applies yaw.
            strafe: f32::from(self.left) - f32::from(self.right),
            jump: self.jump,
            sneak: self.sneak,
            sprint: self.effective_sprinting(),
            // Flight state is filled in by GameState::tick from capabilities.
            ..PlayerInput::default()
        }
    }

    /// Sprint requires the toggle to be on and the player to be actively pushing
    /// forward, never while sneaking (mirrors vanilla `moveForward >= 0.8`).
    fn effective_sprinting(&self) -> bool {
        self.sprint && self.forward && !self.backward && !self.sneak
    }

    /// Clear every held key. Used when the pause menu opens so a key released
    /// while the menu is up (and therefore ignored) doesn't stay "stuck on"
    /// after resuming.
    pub fn release_all(&mut self) {
        *self = Self::default();
    }
}

/// Vanilla `PlayerCapabilities`: driven by the server's S39 abilities packet
/// and echoed back in C13 whenever the client toggles flight.
#[derive(Debug, Clone, Copy)]
pub struct PlayerCapabilities {
    pub invulnerable: bool,
    pub flying: bool,
    pub allow_flying: bool,
    pub creative: bool,
    pub fly_speed: f32,
    pub walk_speed: f32,
}

impl Default for PlayerCapabilities {
    fn default() -> Self {
        Self {
            invulnerable: false,
            flying: false,
            allow_flying: false,
            creative: false,
            fly_speed: 0.05,
            walk_speed: 0.1,
        }
    }
}

/// Standing eye height above the feet, in blocks (vanilla 1.8).
const STANDING_EYE_HEIGHT: f64 = 1.62;
/// How far the camera drops below the standing eye height while sneaking.
const SNEAK_EYE_DROP: f64 = 0.08;
/// Field-of-view added on top of the base FOV while sprinting, in degrees.
const SPRINT_FOV_BOOST: f32 = 10.0;
const BASE_FOV: f32 = 70.0;

pub struct GameState {
    pub world: World,
    pub input: InputState,
    pub camera: Camera,
    player: EntityState,
    previous_player_position: DVec3,
    physics: PlayerPhysics,
    has_sky_light: bool,
    joined_game: bool,
    /// Set once the server has sent the initial PlayerPositionLook.
    position_synced: bool,
    /// Set when a server position correction needs to be echoed back to confirm
    /// the teleport (1.8 keeps the player in limbo until it is).
    pending_confirm: bool,
    /// Set on a teleport so the very next tick holds position and emits no
    /// movement packet — only the teleport ack goes out. Vanilla resumes movement
    /// the following tick; sending the ack and a moved-away position in the same
    /// tick makes a strict anti-cheat reject the ack and setback-loop forever.
    freeze_movement_after_teleport: bool,
    /// Set when the server reports we died (health <= 0); the main loop sends a
    /// respawn request. A dead player is frozen server-side — its movement is
    /// never processed and the server resends our position every tick — so we
    /// must respawn to move at all.
    needs_respawn: bool,
    /// Last server-reported health (20 == full). Drives the death screen.
    health: f32,
    /// Last server-reported food level.
    food: i32,
    /// Experience bar fill (0..1) and level, from SetExperience.
    xp_bar: f32,
    xp_level: i32,
    /// Set when health hit 0; cleared on respawn. The UI shows the death screen
    /// and the player must click respawn (vanilla holds a dead player frozen).
    is_dead: bool,
    /// Player inventory window (window id 0): 45 slots — 0 craft output, 1-4
    /// crafting, 5-8 armor, 9-35 main, 36-44 hotbar. Synced from the server.
    inventory: Vec<Option<SlotItem>>,
    creative: bool,
    dirty_chunks: HashSet<ChunkPos>,
    /// Block changes received for chunks that weren't loaded yet, replayed once
    /// the chunk arrives (otherwise spawn-platform blocks can be lost).
    pending_block_changes: std::collections::HashMap<ChunkPos, Vec<(i32, i32, i32, BlockState)>>,
    // Smoothed 0..1 view-state amounts, advanced once per physics tick so the
    // sneak camera dip and sprint FOV widen ease in/out instead of snapping.
    sneak_amount: f32,
    previous_sneak_amount: f32,
    sprint_amount: f32,
    previous_sprint_amount: f32,
    // Seconds remaining in the arm-swing animation (0 == idle).
    swing_timer: f32,
    // Selected hotbar slot, 0..9.
    selected_slot: i32,
    /// The block currently being mined (survival), with accumulated progress.
    breaking: Option<BreakProgress>,
    /// Persistent sprint state, vanilla semantics: a sprint can only START on the
    /// ground; once started it CONTINUES (through a jump) until forward is released,
    /// the player sneaks, or a horizontal collision occurs. Modeling this (rather
    /// than recomputing per tick) is what lets a mid-air sprint-cancel and a sprint
    /// into a block edge stay in sync with the server's movement prediction.
    sprinting: bool,
    /// Server-driven abilities (S39); `flying` is also toggled locally.
    capabilities: PlayerCapabilities,
    /// Vanilla flyToggleTimer: a second jump press while this is non-zero
    /// (a 7-tick window) toggles flight.
    fly_toggle_timer: u8,
    /// Jump key state last tick, for the press-edge detection.
    was_jump_down: bool,
    /// Set when capabilities changed locally and a C13 echo must be sent.
    abilities_dirty: bool,
    /// Received/sent chat lines and the action bar (S02 ChatMessage).
    pub chat: ChatState,
    /// Objectives/scores/teams driving the sidebar (S3B/S3C/S3D/S3E).
    pub scoreboard: Scoreboard,
}

/// In-progress survival block break: the target cell, the face the dig started
/// on, accumulated 0..1 progress and how much to add each 20 Hz tick.
#[derive(Debug, Clone, Copy)]
struct BreakProgress {
    x: i32,
    y: i32,
    z: i32,
    progress: f32,
    per_tick: f32,
}

/// Duration of one hand swing, ~6 ticks like vanilla.
const SWING_DURATION: f32 = 0.3;

impl GameState {
    pub fn demo(aspect: f32) -> Self {
        let mut world = World::new();
        build_demo_world(&mut world);
        let mut state = Self::new(world, EntityId(0), DVec3::new(0.5, 2.0, 0.5), aspect);
        // Demo sandbox: allow flight (double-tap space) so the abilities
        // mechanics are usable offline.
        state.capabilities.allow_flying = true;
        state
    }

    pub fn empty_for_server(aspect: f32) -> Self {
        Self::new(
            World::new(),
            EntityId(0),
            DVec3::new(0.5, 80.0, 0.5),
            aspect,
        )
    }

    fn new(mut world: World, player_id: EntityId, position: DVec3, aspect: f32) -> Self {
        let player = EntityState::new_local_player(player_id, position);
        world.upsert_entity(player.clone());
        let mut camera = Camera::new(
            to_render_vec3(position + DVec3::new(0.0, STANDING_EYE_HEIGHT, 0.0)),
            aspect,
        );
        camera.yaw = 0.0;
        camera.pitch = 0.0;
        camera.fovy_degrees = BASE_FOV;
        Self {
            world,
            input: InputState::default(),
            camera,
            previous_player_position: player.position,
            player,
            physics: PlayerPhysics::default(),
            has_sky_light: true,
            joined_game: false,
            position_synced: false,
            pending_confirm: false,
            freeze_movement_after_teleport: false,
            needs_respawn: false,
            health: 20.0,
            food: 20,
            xp_bar: 0.0,
            xp_level: 0,
            is_dead: false,
            inventory: vec![None; 45],
            creative: false,
            dirty_chunks: HashSet::new(),
            pending_block_changes: std::collections::HashMap::new(),
            sneak_amount: 0.0,
            previous_sneak_amount: 0.0,
            sprint_amount: 0.0,
            previous_sprint_amount: 0.0,
            swing_timer: 0.0,
            selected_slot: 0,
            breaking: None,
            sprinting: false,
            capabilities: PlayerCapabilities::default(),
            fly_toggle_timer: 0,
            was_jump_down: false,
            abilities_dirty: false,
            chat: ChatState::default(),
            scoreboard: Scoreboard::default(),
        }
    }

    /// The queued C13 abilities echo, if flight was toggled since the last
    /// call. Sent before the tick's movement packet, like vanilla
    /// `sendPlayerAbilities`.
    pub fn take_abilities_packet(&mut self) -> Option<ServerboundPacket> {
        if !std::mem::take(&mut self.abilities_dirty) {
            return None;
        }
        let caps = self.capabilities;
        Some(ServerboundPacket::PlayerAbilities {
            invulnerable: caps.invulnerable,
            flying: caps.flying,
            allow_flying: caps.allow_flying,
            creative: caps.creative,
            fly_speed: caps.fly_speed,
            walk_speed: caps.walk_speed,
        })
    }

    pub fn selected_slot(&self) -> i32 {
        self.selected_slot
    }

    /// Select an absolute hotbar slot (0..9). Returns the HeldItemChange packet
    /// to send when the selection actually changed.
    pub fn set_selected_slot(&mut self, slot: i32) -> Option<ServerboundPacket> {
        let slot = slot.clamp(0, 8);
        if slot == self.selected_slot {
            return None;
        }
        self.selected_slot = slot;
        Some(ServerboundPacket::HeldItemChange { slot: slot as i16 })
    }

    /// Scroll the hotbar selection by `delta`, wrapping around the 9 slots.
    pub fn cycle_slot(&mut self, delta: i32) -> Option<ServerboundPacket> {
        let next = (self.selected_slot + delta).rem_euclid(9);
        self.set_selected_slot(next)
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        self.camera.aspect = aspect;
    }

    /// Apply raw mouse deltas to the view. `sensitivity` is degrees of rotation
    /// per pixel of mouse motion (see `Settings::mouse_factor`); the vanilla
    /// default of ~0.15 corresponds to the 50% sensitivity slider position.
    pub fn rotate_view(&mut self, mouse_dx: f32, mouse_dy: f32, sensitivity: f32) {
        self.player.yaw += mouse_dx * sensitivity;
        // Minecraft pitch grows downward; mouse-down (positive dy) increases it.
        self.player.pitch = (self.player.pitch + mouse_dy * sensitivity).clamp(-89.0, 89.0);
        self.camera.yaw = self.player.yaw;
        self.camera.pitch = self.player.pitch;
    }

    /// Force the player's look direction (test driver only).
    pub fn debug_set_look(&mut self, yaw: f32, pitch: f32) {
        self.player.yaw = yaw;
        self.player.pitch = pitch.clamp(-89.0, 89.0);
        self.camera.yaw = self.player.yaw;
        self.camera.pitch = self.player.pitch;
    }

    /// Whether the crosshair is currently on a block (test driver only).
    pub fn debug_has_block_target(&self) -> bool {
        matches!(self.pick_target(), Some(InteractionTarget::Block { .. }))
    }

    pub fn apply_scripted_smoke_input(&mut self, elapsed_seconds: f32, total_seconds: f32) {
        let active = elapsed_seconds < total_seconds - 1.0;
        self.input.forward = active;
        self.input.sprint = active;
        self.input.jump = active && elapsed_seconds % 2.0 < 0.25;
        self.input.turn_right = active;
        self.input.look_up = active && elapsed_seconds % 3.0 < 0.5;
    }

    pub fn can_send_movement_packets(&self) -> bool {
        // Wait for the server's initial position so we never send the pre-spawn
        // placeholder location (which triggers correction storms).
        self.joined_game && self.position_synced
    }

    /// If the server just corrected our position, return the exact corrected
    /// snapshot to echo back (confirming the teleport) before local physics
    /// moves us off it. Consumed once.
    pub fn take_position_confirm(&mut self) -> Option<MovementSnapshot> {
        if self.pending_confirm && self.can_send_movement_packets() {
            self.pending_confirm = false;
            // Echo our true on_ground (set from block support in the teleport
            // handler). Hardcoding false here makes a strict anti-cheat's ground
            // check disagree with its own simulation and setback-loop us when the
            // teleport target is a solid resting spot.
            Some(self.movement_snapshot())
        } else {
            self.pending_confirm = false;
            None
        }
    }

    /// If the server told us we died, consume the flag and return true so the
    /// caller sends a respawn request (ClientStatus action 0).
    pub fn take_respawn_request(&mut self) -> bool {
        std::mem::take(&mut self.needs_respawn)
    }

    /// Current health (0..=20). 0 means dead.
    pub fn health(&self) -> f32 {
        self.health
    }

    pub fn food(&self) -> i32 {
        self.food
    }

    /// Experience bar fill, 0..1.
    pub fn xp_bar(&self) -> f32 {
        self.xp_bar
    }

    pub fn xp_level(&self) -> i32 {
        self.xp_level
    }

    /// Whether the server reported us dead and we have not respawned yet.
    pub fn is_dead(&self) -> bool {
        self.is_dead
    }

    /// Request a respawn (called when the player clicks the respawn button). The
    /// main loop then sends ClientStatus action 0 on the next tick. Clears the
    /// dead flag optimistically so the UI leaves the death screen immediately;
    /// the server confirms with a fresh Respawn + UpdateHealth.
    pub fn request_respawn(&mut self) {
        self.needs_respawn = true;
        self.is_dead = false;
    }

    /// The 9 hotbar slots (inventory indices 36..45) for the HUD.
    pub fn hotbar_items(&self) -> &[Option<SlotItem>] {
        let end = self.inventory.len().min(45);
        let start = end.saturating_sub(9);
        &self.inventory[start..end]
    }

    /// The full inventory window (45 slots) for the inventory screen.
    pub fn inventory_slots(&self) -> &[Option<SlotItem>] {
        &self.inventory
    }

    pub fn loaded_chunk_count(&self) -> usize {
        self.world.chunk_count()
    }

    /// Advance one 20 Hz simulation tick, returning the movement to report — or
    /// `None` on the tick right after a teleport, where vanilla emits only the
    /// teleport ack (already sent via [`take_position_confirm`]) and resumes
    /// movement next tick. The caller must not send a movement packet on `None`.
    pub fn tick(&mut self, dt: f32) -> Option<MovementSnapshot> {
        self.previous_player_position = self.player.position;
        let turn_speed = 110.0 * dt;
        if self.input.turn_left {
            self.player.yaw -= turn_speed;
        }
        if self.input.turn_right {
            self.player.yaw += turn_speed;
        }
        // Minecraft pitch grows downward: looking up decreases it, down increases.
        if self.input.look_up {
            self.player.pitch = (self.player.pitch - turn_speed).max(-89.0);
        }
        if self.input.look_down {
            self.player.pitch = (self.player.pitch + turn_speed).min(89.0);
        }

        // Vanilla EntityPlayerSP fly toggle: a second jump press while the
        // 7-tick flyToggleTimer runs flips flight and queues the C13 echo.
        // The timer decrements after the check (in EntityPlayer.onLivingUpdate),
        // which is what makes the double-tap window exactly 7 ticks.
        let jump_pressed = self.input.jump && !self.was_jump_down;
        self.was_jump_down = self.input.jump;
        if self.capabilities.allow_flying && jump_pressed {
            if self.fly_toggle_timer == 0 {
                self.fly_toggle_timer = 7;
            } else {
                self.capabilities.flying = !self.capabilities.flying;
                self.abilities_dirty = true;
                self.fly_toggle_timer = 0;
            }
        }
        if self.fly_toggle_timer > 0 {
            self.fly_toggle_timer -= 1;
        }

        // First tick after a teleport: hold exactly on the target and emit no
        // movement packet — the teleport ack (take_position_confirm) is the only
        // packet this tick, matching vanilla, which resumes movement next tick.
        if self.freeze_movement_after_teleport {
            self.freeze_movement_after_teleport = false;
            self.world.upsert_entity(self.player.clone());
            self.advance_view_state();
            self.update_camera(1.0);
            return None;
        }

        // Hold the player still (no physics) while:
        //  - the chunk under us hasn't arrived yet, so we don't fall through
        //    not-yet-generated terrain on join.
        let bx = self.player.position.x.floor() as i32;
        let bz = self.player.position.z.floor() as i32;
        // Update the persistent sprint state at the START of the tick (before the
        // move), using the PREVIOUS tick's on_ground / collision — exactly like
        // vanilla EntityPlayerSP.onLivingUpdate. This keeps the simulated movement
        // and the reported sprint flag in agreement: starting only on the ground
        // (no mid-air sprint), and stopping the tick after a collision rather than
        // mid-move (so block edges don't desync the prediction).
        let wants_sprint =
            self.input.sprint && self.input.forward && !self.input.backward && !self.input.sneak;
        self.sprinting = if self.sprinting {
            wants_sprint && !self.player.collided_horizontally
        } else {
            // Vanilla's sprint-key branch has no onGround requirement; keep
            // the ground gate for normal play but allow starting while flying.
            wants_sprint
                && (self.player.on_ground || self.capabilities.flying)
                && !self.player.collided_horizontally
        };
        if self.world.is_block_column_loaded(bx, bz) {
            let mut input = self.input.player_input();
            input.sprint = self.sprinting;
            input.flying = self.capabilities.flying;
            input.fly_speed = self.capabilities.fly_speed;
            self.physics.tick(&self.world, &mut self.player, input);
        } else {
            self.player.velocity = DVec3::ZERO;
        }
        // Vanilla: touching the ground while flying turns flight off (checked
        // after the move) and sends the abilities echo.
        if self.player.on_ground && self.capabilities.flying {
            self.capabilities.flying = false;
            self.abilities_dirty = true;
        }
        self.world.upsert_entity(self.player.clone());
        self.advance_view_state();
        self.update_camera(1.0);
        Some(self.movement_snapshot())
    }

    /// Ease the sneak/sprint view amounts toward their targets, once per tick so
    /// the transition is framerate independent. The per-frame `update_camera`
    /// then interpolates between the previous and current values.
    fn advance_view_state(&mut self) {
        const APPROACH: f32 = 0.4;
        self.previous_sneak_amount = self.sneak_amount;
        self.previous_sprint_amount = self.sprint_amount;
        let sneak_target = f32::from(self.input.sneak);
        let sprint_target = f32::from(self.sprinting);
        self.sneak_amount += (sneak_target - self.sneak_amount) * APPROACH;
        self.sprint_amount += (sprint_target - self.sprint_amount) * APPROACH;
    }

    pub fn update_camera(&mut self, tick_alpha: f32) {
        let alpha = tick_alpha.clamp(0.0, 1.0);
        let position = self
            .previous_player_position
            .lerp(self.player.position, alpha as f64);
        let sneak = lerp(self.previous_sneak_amount, self.sneak_amount, alpha);
        let sprint = lerp(self.previous_sprint_amount, self.sprint_amount, alpha);
        let eye_height = STANDING_EYE_HEIGHT - SNEAK_EYE_DROP * sneak as f64;
        self.camera.position = to_render_vec3(position + DVec3::new(0.0, eye_height, 0.0));
        // Vanilla getFovModifier widens the FOV by 1.1x while flying.
        let fly_factor = if self.capabilities.flying { 1.1 } else { 1.0 };
        self.camera.fovy_degrees = (BASE_FOV + SPRINT_FOV_BOOST * sprint) * fly_factor;
        self.camera.yaw = self.player.yaw;
        self.camera.pitch = self.player.pitch;
    }

    /// Start (or restart) the hand-swing animation.
    pub fn swing_arm(&mut self) {
        self.swing_timer = SWING_DURATION;
    }

    /// Advance time-based view animations (the hand swing). Called once per
    /// rendered frame with the real frame delta.
    pub fn advance_animations(&mut self, dt: f32) {
        self.swing_timer = (self.swing_timer - dt).max(0.0);
    }

    /// Build the per-frame entity model geometry: a textured model per tracked
    /// entity (a skinned humanoid for players, a colored model for mobs and
    /// objects). Entities are drawn even while paused/dead so they stay
    /// visible behind menu overlays. The first-person hand/held item is
    /// appended by [`crate::item_renderer::ItemRenderer`]; the mining crack
    /// overlay is drawn by the renderer from `breaking_overlay()`.
    pub fn build_entity_model(&self) -> ModelMesh {
        let mut mesh = ModelMesh::new();
        for entity in self.world.entities() {
            if entity.id == self.player.id {
                continue;
            }
            let feet = to_render_vec3(entity.position);
            let (half_width, height) = entity.size();
            mesh.push_entity(
                entity.kind,
                feet,
                half_width as f32,
                height as f32,
                entity.yaw,
            );
        }
        mesh
    }

    /// Current 0..1 hand-swing progress (0 when idle).
    pub fn swing_progress(&self) -> f32 {
        if self.swing_timer > 0.0 {
            1.0 - self.swing_timer / SWING_DURATION
        } else {
            0.0
        }
    }

    /// The item in the selected hotbar slot, if any.
    pub fn held_item(&self) -> Option<SlotItem> {
        self.inventory
            .get(36 + self.selected_slot.clamp(0, 8) as usize)
            .copied()
            .flatten()
    }

    /// Block-interaction reach: 5.0 in creative, 4.5 otherwise (vanilla
    /// PlayerControllerMP.getBlockReachDistance).
    fn block_reach(&self) -> f64 {
        if self.creative {
            5.0
        } else {
            4.5
        }
    }

    /// Entity-interaction reach: 6.0 in creative, 3.0 otherwise (vanilla
    /// EntityRenderer.getMouseOver clamps survival entity reach to 3.0).
    fn entity_reach(&self) -> f64 {
        if self.creative {
            6.0
        } else {
            3.0
        }
    }

    /// Eye position/direction the crosshair ray is cast from — the live camera,
    /// so the ray matches exactly what the player sees (eye height already
    /// includes the sneak dip).
    fn eye_ray(&self) -> (DVec3, DVec3) {
        let p = self.camera.position;
        let d = self.camera.direction();
        (
            DVec3::new(p.x as f64, p.y as f64, p.z as f64),
            DVec3::new(d.x as f64, d.y as f64, d.z as f64),
        )
    }

    /// Resolve what the crosshair is pointing at, following vanilla precedence:
    /// the nearest entity within entity reach wins only when it is closer than
    /// the block hit (or there is no block hit).
    pub fn pick_target(&self) -> Option<InteractionTarget> {
        let (origin, dir) = self.eye_ray();
        let block = raycast_block(&self.world, origin, dir, self.block_reach());
        let entity = self.raycast_entity(origin, dir, self.entity_reach());

        let entity_wins = match (&entity, &block) {
            (Some(e), Some(b)) => e.distance < b.distance,
            (Some(_), None) => true,
            _ => false,
        };
        if entity_wins {
            let e = entity.unwrap();
            let hit = origin + dir * e.distance;
            let rel = hit - e.origin;
            return Some(InteractionTarget::Entity {
                id: e.id,
                cursor: [rel.x as f32, rel.y as f32, rel.z as f32],
            });
        }
        block.map(|b| InteractionTarget::Block {
            x: b.x,
            y: b.y,
            z: b.z,
            face: b.face,
            cursor_x: b.cursor[0],
            cursor_y: b.cursor[1],
            cursor_z: b.cursor[2],
        })
    }

    fn raycast_entity(&self, origin: DVec3, dir: DVec3, max_dist: f64) -> Option<EntityHit> {
        let mut best: Option<EntityHit> = None;
        for entity in self.world.entities() {
            if entity.id == self.player.id {
                continue;
            }
            // Vanilla expands the target box by collisionBorderSize (0.1).
            let pad = DVec3::splat(0.1);
            if let Some(t) = ray_aabb(origin, dir, entity.aabb.min - pad, entity.aabb.max + pad) {
                if (0.0..=max_dist).contains(&t) && best.as_ref().is_none_or(|b| t < b.distance) {
                    best = Some(EntityHit {
                        id: entity.id.0,
                        distance: t,
                        origin: entity.position,
                    });
                }
            }
        }
        best
    }

    /// Left-mouse press: attack the targeted entity, instantly break the
    /// targeted block in creative, or begin a timed survival dig. Matches the
    /// vanilla packet order (UseEntity/PlayerDigging before the swing).
    pub fn on_attack_press(&mut self) -> Vec<ServerboundPacket> {
        self.breaking = None;
        let mut packets = Vec::new();
        match self.pick_target() {
            Some(InteractionTarget::Entity { id, .. }) => {
                self.swing_arm();
                packets.push(ServerboundPacket::UseEntity {
                    target: id,
                    kind: UseEntityKind::Attack,
                });
                packets.push(ServerboundPacket::SwingArm);
            }
            Some(InteractionTarget::Block { x, y, z, face, .. }) => {
                if self.creative {
                    // Creative breaks on StartDestroy alone.
                    self.swing_arm();
                    packets.push(ServerboundPacket::PlayerDigging {
                        status: DiggingStatus::StartDestroy,
                        x,
                        y,
                        z,
                        face,
                    });
                    self.predict_break(x, y, z);
                    packets.push(ServerboundPacket::SwingArm);
                } else {
                    packets.extend(self.begin_break(x, y, z, face));
                }
            }
            None => {
                self.swing_arm();
                packets.push(ServerboundPacket::SwingArm);
            }
        }
        packets
    }

    /// Called once per 20 Hz tick while the left mouse is held (survival): keep
    /// advancing the current dig, finish it when complete, and seamlessly start
    /// digging the next block when the crosshair moves on (vanilla "hold to
    /// mine" sweep).
    pub fn on_attack_hold(&mut self) -> Vec<ServerboundPacket> {
        let mut packets = Vec::new();
        if self.creative {
            return packets;
        }
        let target = match self.pick_target() {
            Some(InteractionTarget::Block { x, y, z, face, .. }) => Some((x, y, z, face)),
            _ => None,
        };
        let current = self.breaking.as_ref().map(|b| (b.x, b.y, b.z));
        match (current, target) {
            // Still mining the same block — advance and finish. Vanilla swings the
            // arm every tick while mining (clickMouse → swingItem each tick), so
            // every dig packet is paired with an animation; omitting the swing on a
            // START/FINISH tick trips NoSwingBreak.
            (Some((bx, by, bz)), Some((x, y, z, face))) if bx == x && by == y && bz == z => {
                let done = {
                    let b = self.breaking.as_mut().expect("breaking is Some");
                    b.progress += b.per_tick;
                    b.progress >= 1.0
                };
                self.swing_arm();
                packets.push(ServerboundPacket::SwingArm);
                if done {
                    packets.push(ServerboundPacket::PlayerDigging {
                        status: DiggingStatus::FinishDestroy,
                        x,
                        y,
                        z,
                        face,
                    });
                    self.breaking = None;
                    self.predict_break(x, y, z);
                }
            }
            // Crosshair moved off the block being mined — cancel it, then begin
            // the new target if there is one. Vanilla's resetBlockRemoving always
            // aborts with EnumFacing.DOWN (face 0); sending the block's real face
            // makes Grim's PositionBreakB remember it and flag the next dig.
            (Some((bx, by, bz)), maybe_new) => {
                packets.push(ServerboundPacket::PlayerDigging {
                    status: DiggingStatus::CancelDestroy,
                    x: bx,
                    y: by,
                    z: bz,
                    face: 0,
                });
                self.breaking = None;
                if let Some((x, y, z, face)) = maybe_new {
                    packets.extend(self.begin_break(x, y, z, face));
                }
            }
            // Not mining yet but holding over a block — start it.
            (None, Some((x, y, z, face))) => {
                packets.extend(self.begin_break(x, y, z, face));
            }
            (None, None) => {}
        }
        packets
    }

    /// Left-mouse release: cancel any in-progress dig.
    pub fn on_attack_release(&mut self) -> Vec<ServerboundPacket> {
        self.cancel_breaking().into_iter().collect()
    }

    /// Cancel an in-progress dig (e.g. on release or when leaving the game
    /// screen), returning the CancelDestroy packet to send if one was active.
    pub fn cancel_breaking(&mut self) -> Option<ServerboundPacket> {
        self.breaking
            .take()
            .map(|b| ServerboundPacket::PlayerDigging {
                status: DiggingStatus::CancelDestroy,
                x: b.x,
                y: b.y,
                z: b.z,
                // Vanilla resetBlockRemoving aborts with EnumFacing.DOWN (face 0).
                face: 0,
            })
    }

    /// Begin a survival dig on the block, sending StartDestroy + a swing and
    /// arming the per-tick progress (instant for hardness-0 / unbreakable blocks
    /// are simply not tracked).
    fn begin_break(&mut self, x: i32, y: i32, z: i32, face: u8) -> Vec<ServerboundPacket> {
        self.swing_arm();
        let ticks = block_break_ticks(self.world.block_at(x, y, z));
        self.breaking = if ticks.is_finite() {
            Some(BreakProgress {
                x,
                y,
                z,
                progress: 0.0,
                per_tick: 1.0 / ticks.max(1.0),
            })
        } else {
            None
        };
        vec![
            ServerboundPacket::PlayerDigging {
                status: DiggingStatus::StartDestroy,
                x,
                y,
                z,
                face,
            },
            ServerboundPacket::SwingArm,
        ]
    }

    /// Locally clear a just-broken block so the crosshair stops targeting it
    /// (prevents an immediate re-dig before the server's BlockChange arrives).
    fn predict_break(&mut self, x: i32, y: i32, z: i32) {
        if self
            .world
            .set_block_if_chunk_loaded(x, y, z, BlockState::AIR)
        {
            self.mark_chunk_dirty(ChunkPos::new(x.div_euclid(16), z.div_euclid(16)));
        }
    }

    /// The block currently being mined and its 0..9 crack stage, for the HUD /
    /// breaking overlay.
    pub fn breaking_overlay(&self) -> Option<(i32, i32, i32, u8)> {
        self.breaking.as_ref().map(|b| {
            let stage = (b.progress.clamp(0.0, 0.999) * 10.0) as u8;
            (b.x, b.y, b.z, stage)
        })
    }

    /// Right-click: interact with the targeted entity (InteractAt then Interact)
    /// or place against the targeted block, then swing.
    pub fn on_use(&mut self) -> Vec<ServerboundPacket> {
        let mut packets = Vec::new();
        match self.pick_target() {
            Some(InteractionTarget::Entity { id, cursor }) => {
                packets.push(ServerboundPacket::UseEntity {
                    target: id,
                    kind: UseEntityKind::InteractAt {
                        x: cursor[0],
                        y: cursor[1],
                        z: cursor[2],
                    },
                });
                packets.push(ServerboundPacket::UseEntity {
                    target: id,
                    kind: UseEntityKind::Interact,
                });
                self.swing_arm();
                packets.push(ServerboundPacket::SwingArm);
            }
            Some(InteractionTarget::Block {
                x,
                y,
                z,
                face,
                cursor_x,
                cursor_y,
                cursor_z,
            }) => {
                packets.push(ServerboundPacket::PlayerBlockPlacement {
                    x,
                    y,
                    z,
                    face,
                    held_item: None,
                    cursor_x,
                    cursor_y,
                    cursor_z,
                });
                self.swing_arm();
                packets.push(ServerboundPacket::SwingArm);
            }
            None => {}
        }
        packets
    }

    /// One-line diagnostic of the local player's physics state and the block it
    /// is standing on — to reveal why we might fall through the floor.
    pub fn debug_state(&self) -> String {
        let bx = self.player.position.x.floor() as i32;
        let by = self.player.position.y.floor() as i32;
        let bz = self.player.position.z.floor() as i32;
        // Highest non-air block in each of the 3x3 columns around the player, so
        // we can see the surface height (and whether the supporting column is a
        // neighbour the single-column log misses).
        let mut surface = String::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let (cx, cz) = (bx + dx, bz + dz);
                let mut top = i32::MIN;
                for y in (by - 4..=by + 2).rev() {
                    if !self.world.block_at(cx, y, cz).is_air() {
                        top = y;
                        break;
                    }
                }
                surface += &format!("({cx},{cz})top={top} ");
            }
        }
        format!(
            "pos=({:.2},{:.2},{:.2}) on_ground={} | surfaces: {surface}",
            self.player.position.x,
            self.player.position.y,
            self.player.position.z,
            self.player.on_ground,
        )
    }

    /// Log a server position correction (a teleport / setback). These should be
    /// rare in normal play; a continuous stream means the server is rejecting
    /// our movement.
    fn log_correction(&self, flags: i8, old_pos: DVec3) {
        let delta = self.player.position - old_pos;
        log::debug!(
            "server correction -> ({:.3},{:.3},{:.3}) d=({:.3},{:.3},{:.3}) flags={flags:#x}",
            self.player.position.x,
            self.player.position.y,
            self.player.position.z,
            delta.x,
            delta.y,
            delta.z,
        );
    }

    pub fn apply_play_packet(&mut self, packet: ClientboundPlayPacket) -> bool {
        match packet {
            ClientboundPlayPacket::JoinGame {
                entity_id,
                game_mode,
                dimension,
                ..
            } => {
                self.player.id = EntityId(entity_id);
                self.has_sky_light = dimension == 0;
                // Low 3 bits are the gamemode; bit 3 is the hardcore flag.
                self.creative = (game_mode & 0x7) == 1;
                self.joined_game = true;
                self.world.upsert_entity(self.player.clone());
                false
            }
            ClientboundPlayPacket::UpdateHealth { health, food, .. } => {
                self.health = health;
                self.food = food;
                // A dead player is frozen by the server until it respawns. We no
                // longer auto-respawn — the UI shows a death screen and the player
                // clicks "respawn" (which sets needs_respawn via request_respawn).
                if health <= 0.0 {
                    if !self.is_dead {
                        log::info!("player died (health {health}); showing death screen");
                    }
                    self.is_dead = true;
                } else {
                    self.is_dead = false;
                }
                false
            }
            ClientboundPlayPacket::SetExperience { bar, level } => {
                self.xp_bar = bar.clamp(0.0, 1.0);
                self.xp_level = level;
                false
            }
            ClientboundPlayPacket::Respawn {
                dimension,
                game_mode,
                ..
            } => {
                // The server resends chunks plus a fresh PlayerPositionLook at the
                // respawn point; wait for that position before reporting movement
                // again so we don't send a stale (death-location) position.
                self.has_sky_light = dimension == 0;
                self.creative = (game_mode & 0x7) == 1;
                self.needs_respawn = false;
                self.is_dead = false;
                self.health = 20.0;
                self.position_synced = false;
                self.pending_confirm = false;
                self.player.velocity = DVec3::ZERO;
                log::info!("respawned into dimension {dimension}");
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
                let old_pos = self.player.position;
                if flags & 0x01 != 0 {
                    self.player.position.x += x;
                } else {
                    self.player.position.x = x;
                }
                if flags & 0x02 != 0 {
                    self.player.position.y += y;
                } else {
                    self.player.position.y = y;
                }
                if flags & 0x04 != 0 {
                    self.player.position.z += z;
                } else {
                    self.player.position.z = z;
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
                // Vanilla zeroes motion on every position-look. Without this a
                // falling player keeps its downward velocity after a correction
                // and immediately falls again, fighting the server forever.
                self.player.velocity = DVec3::ZERO;
                self.previous_player_position = self.player.position;
                self.player.sync_aabb_to_position();
                // Zeroing motion (above) leaves the next tick with no downward move
                // for the collision-based ground test to catch, so derive on_ground
                // from actual block support here. Otherwise the client claims it is
                // airborne while resting on the teleport target.
                self.player.on_ground = resting_on_ground(&self.world, self.player.aabb);
                self.world.upsert_entity(self.player.clone());
                // Only start sending movement once the server has told us where
                // we are, so we never report the pre-spawn placeholder position.
                self.position_synced = true;
                self.pending_confirm = true;
                self.freeze_movement_after_teleport = true;
                self.log_correction(flags, old_pos);
                false
            }
            ClientboundPlayPacket::ChunkData {
                x,
                z,
                ground_up,
                primary_bit_mask,
                data,
            } => self.apply_raw_chunk(x, z, ground_up, primary_bit_mask, &data, self.has_sky_light),
            ClientboundPlayPacket::MultiBlockChange {
                chunk_x,
                chunk_z,
                changes,
            } => {
                let mut changed = false;
                for block in changes {
                    let x = chunk_x * 16 + block.x as i32;
                    let z = chunk_z * 16 + block.z as i32;
                    changed |= self.apply_block_change(x, block.y as i32, z, block.id, block.meta);
                }
                changed
            }
            ClientboundPlayPacket::BlockChange { x, y, z, id, meta } => {
                self.apply_block_change(x, y, z, id, meta)
            }
            ClientboundPlayPacket::ChunkBulk {
                sky_light_sent,
                chunks,
            } => {
                let mut changed = false;
                for chunk in chunks {
                    changed |= self.apply_raw_chunk(
                        chunk.x,
                        chunk.z,
                        true,
                        chunk.primary_bit_mask,
                        &chunk.data,
                        sky_light_sent,
                    );
                }
                changed
            }
            ClientboundPlayPacket::SpawnPlayer {
                entity_id,
                x,
                y,
                z,
                yaw,
                pitch,
            } => {
                self.spawn_remote_entity(entity_id, EntityKind::RemotePlayer, x, y, z, yaw, pitch);
                false
            }
            ClientboundPlayPacket::SpawnMob {
                entity_id,
                kind,
                x,
                y,
                z,
                yaw,
                pitch,
            } => {
                self.spawn_remote_entity(entity_id, EntityKind::Mob(kind), x, y, z, yaw, pitch);
                false
            }
            ClientboundPlayPacket::SpawnObject {
                entity_id,
                kind,
                x,
                y,
                z,
            } => {
                self.spawn_remote_entity(
                    entity_id,
                    EntityKind::Object(kind as u8),
                    x,
                    y,
                    z,
                    0.0,
                    0.0,
                );
                false
            }
            ClientboundPlayPacket::EntityRelativeMove {
                entity_id,
                dx,
                dy,
                dz,
            } => {
                self.move_remote_entity(entity_id, dx, dy, dz, None);
                false
            }
            ClientboundPlayPacket::EntityLookMove {
                entity_id,
                dx,
                dy,
                dz,
                yaw,
                pitch,
            } => {
                self.move_remote_entity(entity_id, dx, dy, dz, Some((yaw, pitch)));
                false
            }
            ClientboundPlayPacket::EntityLook {
                entity_id,
                yaw,
                pitch,
            } => {
                if let Some(entity) = self.remote_entity_mut(entity_id) {
                    entity.yaw = yaw;
                    entity.pitch = pitch;
                }
                false
            }
            ClientboundPlayPacket::EntityTeleport {
                entity_id,
                x,
                y,
                z,
                yaw,
                pitch,
            } => {
                if let Some(entity) = self.remote_entity_mut(entity_id) {
                    entity.position = DVec3::new(x, y, z);
                    entity.yaw = yaw;
                    entity.pitch = pitch;
                    entity.sync_aabb_to_position();
                }
                false
            }
            ClientboundPlayPacket::EntityVelocity {
                entity_id,
                vx,
                vy,
                vz,
            } => {
                let velocity = DVec3::new(vx, vy, vz);
                if entity_id == self.player.id.0 {
                    // Server knockback: vanilla sets the player's motion directly
                    // from this packet, then local physics carries it out.
                    self.player.velocity = velocity;
                } else if let Some(entity) = self.remote_entity_mut(entity_id) {
                    entity.velocity = velocity;
                }
                false
            }
            ClientboundPlayPacket::SetSlot {
                window_id,
                slot,
                item,
            } => {
                // Window 0 is the player inventory. Slot -1 is the held cursor
                // item during drags; other windows aren't modelled here.
                if window_id == 0 && (0..self.inventory.len() as i16).contains(&slot) {
                    self.inventory[slot as usize] = item;
                }
                false
            }
            ClientboundPlayPacket::WindowItems { window_id, items } => {
                if window_id == 0 {
                    self.inventory = items;
                    self.inventory.resize(45, None);
                }
                false
            }
            ClientboundPlayPacket::HeldItemChange { slot } => {
                // The server tells us which hotbar slot is selected.
                self.selected_slot = (slot as i32).clamp(0, 8);
                false
            }
            ClientboundPlayPacket::DestroyEntities { entity_ids } => {
                for id in entity_ids {
                    self.world.remove_entity(EntityId(id));
                }
                false
            }
            ClientboundPlayPacket::PlayerAbilities {
                invulnerable,
                flying,
                allow_flying,
                creative,
                fly_speed,
                walk_speed,
            } => {
                // Vanilla handlePlayerAbilities applies every field
                // unconditionally (no echo from the handler itself).
                self.capabilities = PlayerCapabilities {
                    invulnerable,
                    flying,
                    allow_flying,
                    creative,
                    fly_speed,
                    walk_speed,
                };
                false
            }
            ClientboundPlayPacket::ChatMessage { json, position } => {
                let text = chat::flatten_chat_json(&json);
                if position == 2 {
                    self.chat.set_action_bar(text);
                } else {
                    log::info!("[chat] {}", chat::strip_legacy_codes(&text));
                    self.chat.push_message(text);
                }
                false
            }
            ClientboundPlayPacket::ScoreboardObjective {
                name,
                mode,
                display_name,
            } => {
                self.scoreboard.apply_objective(name, mode, display_name);
                false
            }
            ClientboundPlayPacket::UpdateScore {
                name,
                objective,
                value,
            } => {
                self.scoreboard.apply_score(&name, &objective, value);
                false
            }
            ClientboundPlayPacket::DisplayScoreboard {
                position,
                objective,
            } => {
                self.scoreboard.apply_display(position, objective);
                false
            }
            ClientboundPlayPacket::Teams { name, action } => {
                self.scoreboard.apply_team(name, action);
                false
            }
            ClientboundPlayPacket::Disconnect { reason_json } => {
                log::warn!("server disconnected: {reason_json}");
                false
            }
            // ConfirmTransaction is ponged on the network thread (vanilla replies
            // immediately), so the game loop never needs to act on it.
            ClientboundPlayPacket::KeepAlive { .. }
            | ClientboundPlayPacket::ConfirmTransaction { .. }
            | ClientboundPlayPacket::Unknown { .. } => false,
        }
    }

    /// World entity by id, skipping the local player (whose movement is driven
    /// by local physics and PlayerPositionLook, not entity packets).
    fn remote_entity_mut(&mut self, entity_id: i32) -> Option<&mut EntityState> {
        if entity_id == self.player.id.0 {
            return None;
        }
        self.world.entity_mut(EntityId(entity_id))
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_remote_entity(
        &mut self,
        entity_id: i32,
        kind: EntityKind,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    ) {
        if entity_id == self.player.id.0 {
            return;
        }
        self.world.upsert_entity(EntityState::new_remote(
            EntityId(entity_id),
            kind,
            DVec3::new(x, y, z),
            yaw,
            pitch,
        ));
    }

    fn move_remote_entity(
        &mut self,
        entity_id: i32,
        dx: f64,
        dy: f64,
        dz: f64,
        look: Option<(f32, f32)>,
    ) {
        if let Some(entity) = self.remote_entity_mut(entity_id) {
            entity.position += DVec3::new(dx, dy, dz);
            if let Some((yaw, pitch)) = look {
                entity.yaw = yaw;
                entity.pitch = pitch;
            }
            entity.sync_aabb_to_position();
        }
    }

    /// Drain up to `max` dirty chunks, nearest to the player first, leaving the
    /// rest queued for following frames. Bounds the per-frame mesh-rebuild cost
    /// so a join-time burst of chunks doesn't stall rendering.
    pub fn take_dirty_chunks_budget(&mut self, max: usize) -> Vec<ChunkPos> {
        if max == 0 || self.dirty_chunks.is_empty() {
            return Vec::new();
        }
        if self.dirty_chunks.len() <= max {
            return self.dirty_chunks.drain().collect();
        }
        let pcx = (self.player.position.x.floor() as i32).div_euclid(16);
        let pcz = (self.player.position.z.floor() as i32).div_euclid(16);
        let mut all: Vec<ChunkPos> = self.dirty_chunks.iter().copied().collect();
        all.sort_by_key(|p| {
            let dx = (p.x - pcx) as i64;
            let dz = (p.z - pcz) as i64;
            dx * dx + dz * dz
        });
        all.truncate(max);
        for p in &all {
            self.dirty_chunks.remove(p);
        }
        all
    }

    fn apply_block_change(&mut self, x: i32, y: i32, z: i32, id: u16, meta: u8) -> bool {
        let block = BlockState::new(id, meta);
        if !self.world.set_block_if_chunk_loaded(x, y, z, block) {
            // Chunk not loaded yet — buffer and replay when it arrives so we
            // don't lose blocks the server places before sending the chunk.
            self.pending_block_changes
                .entry(ChunkPos::new(x.div_euclid(16), z.div_euclid(16)))
                .or_default()
                .push((x, y, z, block));
            return false;
        }
        self.mark_chunk_dirty(ChunkPos::new(x.div_euclid(16), z.div_euclid(16)));
        true
    }

    /// Decode-and-apply a raw chunk payload on the calling thread. This is the
    /// fallback path; the network thread normally decodes chunks off-thread and
    /// delivers them via [`apply_chunk_column`](Self::apply_chunk_column) so the
    /// render loop never blocks on chunk decoding.
    fn apply_raw_chunk(
        &mut self,
        x: i32,
        z: i32,
        ground_up: bool,
        primary_bit_mask: u16,
        data: &[u8],
        has_sky_light: bool,
    ) -> bool {
        if ground_up && primary_bit_mask == 0 {
            self.unload_chunk(x, z);
            return true;
        }
        match decode_chunk_column(data, primary_bit_mask, ground_up, has_sky_light) {
            Ok(column) => self.apply_chunk_column(x, z, &column),
            Err(err) => {
                log::warn!("failed to decode chunk {x},{z}: {err}");
                false
            }
        }
    }

    /// Apply an already-decoded chunk column (fast path). Each section is loaded
    /// directly into the world with no per-block hashmap work, then any block
    /// changes that arrived before the chunk are replayed.
    pub fn apply_chunk_column(&mut self, x: i32, z: i32, column: &ChunkColumnData) -> bool {
        let pos = ChunkPos::new(x, z);
        // A ground-up packet (biomes present) is the authoritative full column;
        // drop stale sections first so removed terrain doesn't linger.
        if column.biomes.is_some() {
            self.world.remove_chunk(pos);
        }
        for section in &column.sections {
            self.world.load_section(
                x,
                z,
                section.y as i32,
                &section.blocks,
                &section.block_light,
                &section.sky_light,
            );
        }
        if let Some(pending) = self.pending_block_changes.remove(&pos) {
            for (wx, wy, wz, block) in pending {
                self.world.set_block_if_chunk_loaded(wx, wy, wz, block);
            }
        }
        self.mark_chunk_dirty(pos);
        true
    }

    /// Unload a chunk (the server sent an empty ground-up column for it).
    pub fn unload_chunk(&mut self, x: i32, z: i32) {
        let pos = ChunkPos::new(x, z);
        self.world.remove_chunk(pos);
        self.mark_chunk_dirty(pos);
    }

    fn mark_chunk_dirty(&mut self, pos: ChunkPos) {
        for dirty in [
            pos,
            ChunkPos::new(pos.x - 1, pos.z),
            ChunkPos::new(pos.x + 1, pos.z),
            ChunkPos::new(pos.x, pos.z - 1),
            ChunkPos::new(pos.x, pos.z + 1),
        ] {
            self.dirty_chunks.insert(dirty);
        }
    }

    fn movement_snapshot(&self) -> MovementSnapshot {
        MovementSnapshot {
            x: self.player.position.x,
            y: self.player.position.y,
            z: self.player.position.z,
            yaw: self.player.yaw,
            pitch: self.player.pitch,
            on_ground: self.player.on_ground,
            entity_id: self.player.id.0,
            sneaking: self.input.sneak,
            sprinting: self.sprinting,
        }
    }
}

fn to_render_vec3(position: DVec3) -> Vec3 {
    Vec3::new(position.x as f32, position.y as f32, position.z as f32)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Number of 20 Hz ticks to break a block by hand in survival, matching vanilla's
/// `Block.getPlayerRelativeBlockHardness`. Returns INFINITY for unbreakable blocks.
///
/// Per-tick damage = digSpeed / hardness / divisor, where digSpeed = 1 (bare hand,
/// standing) and divisor = 30 if the block is hand-harvestable or 100 if it needs a
/// tool (stone/ore/metal take 5x longer by hand). Ticks = ceil(1 / damage). The
/// server (Grim FastBreak) predicts exactly this, so an under-estimate breaks "too
/// fast" and flags; matching it keeps breaking legal.
fn block_break_ticks(block: BlockState) -> f32 {
    let hardness = block_hardness(block.id);
    if hardness < 0.0 {
        return f32::INFINITY; // unbreakable (bedrock, etc.)
    }
    if hardness == 0.0 {
        return 1.0; // instant-break blocks (plants, snow layer) finish in one tick
    }
    let divisor = if block_needs_tool(block.id) {
        100.0
    } else {
        30.0
    };
    (hardness * divisor).ceil().max(1.0)
}

/// Whether the block requires a tool to harvest at full speed (vanilla material
/// `isToolNotRequired() == false`). By hand these break 5x slower (divisor 100).
fn block_needs_tool(id: u16) -> bool {
    matches!(
        id,
        1 | 4
            | 14
            | 15
            | 16
            | 21
            | 22
            | 24
            | 41
            | 42
            | 43
            | 44
            | 45
            | 48
            | 49
            | 56
            | 57
            | 61
            | 62
            | 73
            | 74
            | 87
            | 98
            | 112
            | 129
            | 133
            | 152
    )
}

/// Block hardness in vanilla units; negative means unbreakable.
fn block_hardness(id: u16) -> f32 {
    match id {
        7 | 119 | 120 => -1.0,      // bedrock, end portal frame
        49 => 50.0,                 // obsidian
        42 | 57 | 133 | 152 => 5.0, // iron / diamond / emerald / redstone block
        61 | 62 => 3.5,             // furnace
        // Ores and gold/lapis blocks (need a pickaxe).
        14 | 15 | 16 | 21 | 22 | 41 | 56 | 73 | 74 | 129 => 3.0,
        58 => 2.5, // crafting table
        // Cobblestone, planks, logs, brick, slabs, mossy cobble, nether brick.
        4 | 5 | 17 | 43 | 44 | 45 | 48 | 53 | 85 | 112 | 162 => 2.0,
        // Stone, stone bricks, bookshelf.
        1 | 47 | 98 => 1.5,
        24 | 35 => 0.8,                // sandstone, wool
        2 | 13 | 60 | 82 | 110 => 0.6, // grass, gravel, farmland, clay, mycelium
        3 | 12 | 79 => 0.5,            // dirt, sand, ice
        87 => 0.4,                     // netherrack
        18 | 161 => 0.2,               // leaves
        20 | 89 | 95 | 102 => 0.3,     // glass, glowstone, stained glass, glass pane
        // Instant-break: saplings, plants, snow layer, flowers.
        6 | 31 | 32 | 37 | 38 | 78 => 0.0,
        _ => 1.0,
    }
}

/// What the crosshair is pointing at. Block faces use the vanilla EnumFacing
/// indices (0=down,1=up,2=north,3=south,4=west,5=east).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InteractionTarget {
    Block {
        x: i32,
        y: i32,
        z: i32,
        face: u8,
        cursor_x: u8,
        cursor_y: u8,
        cursor_z: u8,
    },
    Entity {
        id: i32,
        /// Hit point relative to the entity origin, for UseEntity InteractAt.
        cursor: [f32; 3],
    },
}

struct BlockHit {
    x: i32,
    y: i32,
    z: i32,
    face: u8,
    cursor: [u8; 3],
    distance: f64,
}

struct EntityHit {
    id: i32,
    distance: f64,
    origin: DVec3,
}

/// Targetable = a non-air block that has a selection/collision box (excludes
/// air and fluids; includes leaves and glass).
fn is_pickable(block: BlockState) -> bool {
    !block.is_air() && (block.is_solid_collision() || block.is_opaque_cube())
}

/// Voxel DDA (Amanatides–Woo) returning the first targetable block, the face
/// entered, and the sub-block cursor of the hit point.
fn raycast_block(world: &World, origin: DVec3, dir: DVec3, max_dist: f64) -> Option<BlockHit> {
    let mut bx = origin.x.floor() as i32;
    let mut by = origin.y.floor() as i32;
    let mut bz = origin.z.floor() as i32;

    let step = |d: f64| {
        if d > 0.0 {
            1
        } else if d < 0.0 {
            -1
        } else {
            0
        }
    };
    let (sx, sy, sz) = (step(dir.x), step(dir.y), step(dir.z));

    let t_delta = |d: f64| {
        if d != 0.0 {
            (1.0 / d).abs()
        } else {
            f64::INFINITY
        }
    };
    let (dtx, dty, dtz) = (t_delta(dir.x), t_delta(dir.y), t_delta(dir.z));

    let boundary = |b: i32, s: i32, o: f64, d: f64| -> f64 {
        if d == 0.0 {
            return f64::INFINITY;
        }
        let next = if s > 0 { (b + 1) as f64 } else { b as f64 };
        (next - o) / d
    };
    let mut tmx = boundary(bx, sx, origin.x, dir.x);
    let mut tmy = boundary(by, sy, origin.y, dir.y);
    let mut tmz = boundary(bz, sz, origin.z, dir.z);

    let mut face = 1u8;
    let mut t = 0.0;
    loop {
        if is_pickable(world.block_at(bx, by, bz)) {
            let hit = origin + dir * t;
            return Some(BlockHit {
                x: bx,
                y: by,
                z: bz,
                face,
                cursor: block_cursor(hit, bx, by, bz),
                distance: t,
            });
        }
        if tmx <= tmy && tmx <= tmz {
            bx += sx;
            t = tmx;
            tmx += dtx;
            face = if sx > 0 { 4 } else { 5 };
        } else if tmy <= tmz {
            by += sy;
            t = tmy;
            tmy += dty;
            face = if sy > 0 { 0 } else { 1 };
        } else {
            bz += sz;
            t = tmz;
            tmz += dtz;
            face = if sz > 0 { 2 } else { 3 };
        }
        if t > max_dist {
            return None;
        }
    }
}

fn block_cursor(hit: DVec3, bx: i32, by: i32, bz: i32) -> [u8; 3] {
    let f = |v: f64, base: i32| (((v - base as f64).clamp(0.0, 1.0)) * 16.0).round() as u8;
    [f(hit.x, bx), f(hit.y, by), f(hit.z, bz)]
}

/// Slab-method ray vs AABB. Returns the entry distance along `dir` (>= 0), or
/// None when the ray misses. `dir` need not be normalized but here it is.
fn ray_aabb(origin: DVec3, dir: DVec3, min: DVec3, max: DVec3) -> Option<f64> {
    let mut t_near = f64::NEG_INFINITY;
    let mut t_far = f64::INFINITY;
    for axis in 0..3 {
        let (o, d, lo, hi) = (origin[axis], dir[axis], min[axis], max[axis]);
        if d.abs() < 1.0e-8 {
            if o < lo || o > hi {
                return None;
            }
        } else {
            let mut t1 = (lo - o) / d;
            let mut t2 = (hi - o) / d;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            t_near = t_near.max(t1);
            t_far = t_far.min(t2);
            if t_near > t_far {
                return None;
            }
        }
    }
    if t_far < 0.0 {
        return None;
    }
    Some(t_near.max(0.0))
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

#[cfg(test)]
mod interaction_tests {
    use super::*;
    use recraft_core::{BlockState, EntityId, EntityKind};

    fn looking_along_x() -> GameState {
        // Empty world, camera at a block center looking toward +x (yaw -90).
        // Fractional coords mirror a real eye position (avoids the degenerate
        // case of a ray starting exactly on a voxel boundary).
        let mut gs = GameState::empty_for_server(1.0);
        gs.camera.position = Vec3::new(0.5, 0.5, 0.5);
        gs.camera.yaw = -90.0;
        gs.camera.pitch = 0.0;
        gs
    }

    #[test]
    fn raycast_block_hits_first_block_and_face() {
        let mut world = World::new();
        world.set_block(3, 0, 0, BlockState::STONE);
        let hit = raycast_block(
            &world,
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
            4.5,
        )
        .expect("ray should hit the block");
        assert_eq!((hit.x, hit.y, hit.z), (3, 0, 0));
        assert_eq!(hit.face, 4, "entered the west face moving +x");
        assert!((hit.distance - 3.0).abs() < 1e-9);
    }

    #[test]
    fn raycast_block_respects_reach() {
        let mut world = World::new();
        world.set_block(10, 0, 0, BlockState::STONE);
        assert!(
            raycast_block(&world, DVec3::ZERO, DVec3::new(1.0, 0.0, 0.0), 4.5).is_none(),
            "block beyond reach must not be targeted"
        );
    }

    #[test]
    fn ray_aabb_returns_entry_distance() {
        let t = ray_aabb(
            DVec3::ZERO,
            DVec3::new(1.0, 0.0, 0.0),
            DVec3::new(2.0, -1.0, -1.0),
            DVec3::new(3.0, 1.0, 1.0),
        )
        .unwrap();
        assert!((t - 2.0).abs() < 1e-9);
    }

    #[test]
    fn attack_entity_sends_use_entity_then_swing() {
        let mut gs = looking_along_x();
        gs.world.upsert_entity(EntityState::new_remote(
            EntityId(1),
            EntityKind::Mob(54),
            DVec3::new(2.0, 0.0, 0.5),
            0.0,
            0.0,
        ));
        let packets = gs.on_attack_press();
        assert!(
            matches!(
                packets.as_slice(),
                [
                    ServerboundPacket::UseEntity {
                        target: 1,
                        kind: UseEntityKind::Attack
                    },
                    ServerboundPacket::SwingArm,
                ]
            ),
            "got {packets:?}"
        );
    }

    #[test]
    fn survival_dig_starts_on_press_and_finishes_after_holding() {
        let mut gs = looking_along_x();
        gs.world.set_block(3, 0, 0, BlockState::STONE);
        // Press begins the dig (StartDestroy + swing), no FinishDestroy yet.
        let packets = gs.on_attack_press();
        assert!(
            matches!(
                packets.as_slice(),
                [
                    ServerboundPacket::PlayerDigging {
                        status: DiggingStatus::StartDestroy,
                        x: 3,
                        ..
                    },
                    ServerboundPacket::SwingArm,
                ]
            ),
            "got {packets:?}"
        );
        assert!(gs.breaking.is_some(), "dig should be in progress");
        // Holding for enough ticks finishes the dig with FinishDestroy.
        let mut finished = false;
        for _ in 0..200 {
            if gs.world.block_at(3, 0, 0).is_air() {
                break;
            }
            for packet in gs.on_attack_hold() {
                if matches!(
                    packet,
                    ServerboundPacket::PlayerDigging {
                        status: DiggingStatus::FinishDestroy,
                        x: 3,
                        ..
                    }
                ) {
                    finished = true;
                }
            }
        }
        assert!(finished, "holding should eventually send FinishDestroy");
        assert!(
            gs.breaking.is_none(),
            "dig should be cleared after finishing"
        );
    }

    #[test]
    fn use_block_sends_block_placement() {
        let mut gs = looking_along_x();
        gs.world.set_block(3, 0, 0, BlockState::STONE);
        let packets = gs.on_use();
        assert!(
            matches!(
                packets.as_slice(),
                [
                    ServerboundPacket::PlayerBlockPlacement { x: 3, face: 4, .. },
                    ServerboundPacket::SwingArm,
                ]
            ),
            "got {packets:?}"
        );
    }

    #[test]
    fn position_look_resets_velocity_and_enables_sending() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.joined_game = true;
        gs.player.velocity = DVec3::new(0.2, -3.0, 0.1);
        // Movement is withheld until the server sends our spawn position.
        assert!(!gs.can_send_movement_packets());
        gs.apply_play_packet(ClientboundPlayPacket::PlayerPositionLook {
            x: 1.0,
            y: 64.0,
            z: 2.0,
            yaw: 0.0,
            pitch: 0.0,
            flags: 0,
        });
        assert_eq!(gs.player.velocity, DVec3::ZERO, "velocity must reset");
        assert!(gs.can_send_movement_packets());
        assert!((gs.player.position.y - 64.0).abs() < 1.0e-9);
    }

    #[test]
    fn double_tap_jump_toggles_flight_and_queues_echo() {
        let mut gs = GameState::demo(1.0); // demo grants allow_flying
        // Press, release, press again within the 7-tick window.
        gs.input.jump = true;
        gs.tick(0.05);
        gs.input.jump = false;
        gs.tick(0.05);
        gs.input.jump = true;
        gs.tick(0.05);
        assert!(gs.capabilities.flying, "double-tap should enable flight");
        let packet = gs.take_abilities_packet().expect("C13 echo queued");
        assert!(matches!(
            packet,
            ServerboundPacket::PlayerAbilities { flying: true, .. }
        ));
        assert!(gs.take_abilities_packet().is_none(), "echo consumed once");
    }

    #[test]
    fn slow_taps_do_not_toggle_flight() {
        let mut gs = GameState::demo(1.0);
        gs.input.jump = true;
        gs.tick(0.05);
        gs.input.jump = false;
        for _ in 0..8 {
            gs.tick(0.05); // let the 7-tick window lapse
        }
        gs.input.jump = true;
        gs.tick(0.05);
        assert!(!gs.capabilities.flying, "slow second tap must not toggle");
        assert!(gs.take_abilities_packet().is_none());
    }

    #[test]
    fn touching_ground_disables_flight() {
        let mut gs = GameState::demo(1.0);
        gs.capabilities.flying = true;
        gs.player.position = DVec3::new(0.5, 1.0, 0.5); // resting on the floor
        gs.player.sync_aabb_to_position();
        gs.tick(0.05);
        assert!(!gs.capabilities.flying, "landing must stop flight");
        assert!(matches!(
            gs.take_abilities_packet(),
            Some(ServerboundPacket::PlayerAbilities { flying: false, .. })
        ));
    }

    #[test]
    fn abilities_packet_updates_capabilities() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.apply_play_packet(ClientboundPlayPacket::PlayerAbilities {
            invulnerable: true,
            flying: true,
            allow_flying: true,
            creative: true,
            fly_speed: 0.1,
            walk_speed: 0.2,
        });
        let caps = gs.capabilities;
        assert!(caps.invulnerable && caps.flying && caps.allow_flying && caps.creative);
        assert!((caps.fly_speed - 0.1).abs() < f32::EPSILON);
        assert!((caps.walk_speed - 0.2).abs() < f32::EPSILON);
        // Server-applied abilities never trigger a client echo by themselves.
        assert!(gs.take_abilities_packet().is_none());
    }

    #[test]
    fn nearer_entity_beats_block() {
        let mut gs = looking_along_x();
        gs.world.set_block(5, 0, 0, BlockState::STONE);
        gs.world.upsert_entity(EntityState::new_remote(
            EntityId(7),
            EntityKind::Mob(54),
            DVec3::new(2.0, 0.0, 0.5),
            0.0,
            0.0,
        ));
        assert!(matches!(
            gs.pick_target(),
            Some(InteractionTarget::Entity { id: 7, .. })
        ));
    }
}
