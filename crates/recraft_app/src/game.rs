use std::collections::HashSet;

use glam::{DVec3, Vec3};

#[derive(Debug, Clone, Copy)]
pub enum DemoKind {
    Landscape,
    ChunkStress,
    EntityStress,
    /// Large rolling terrain viewed from a vista — the realistic-world GPU
    /// benchmark (large coplanar surfaces that greedy meshing and occlusion
    /// culling actually act on, unlike the synthetic checkerboard).
    Terrain,
    /// A single block in an otherwise empty world: the minimal scene for
    /// measuring the fixed per-frame floor (clear + submit + present) — the FPS
    /// ceiling no fill-rate optimization can beat.
    SingleCube,
}
use recraft_core::{
    collision::{is_fence, is_pane, is_stairs},
    mc_math::wrap_degrees,
    resting_on_ground, BlockState, ChunkPos, EntityId, EntityKind, EntityState, PlayerInput,
    PlayerPhysics, RenderShape, SectionPos, World,
};
use recraft_protocol::v1_8_9::{
    chunk::{decode_chunk_column, ChunkColumnData},
    packets::{
        ClientboundPlayPacket, DiggingStatus, HeldItem, MetadataValue, ServerboundPacket, SlotItem,
        TitleAction, UseEntityKind,
    },
};
use recraft_render::{held_item_frame, Camera, ChestKind, EntityAnim, ModelMesh};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, PhysicalKey},
};

use crate::chat::{self, ChatState};
use crate::container::{max_stack, stackable, Container};
use crate::item_renderer::{DroppedItem, FallingBlock, PlayerHeldItem};
use crate::particle::ParticleSystem;
use crate::player_list::PlayerList;
use crate::scoreboard::Scoreboard;
use crate::settings::{GameAction, Keybinds};
use crate::sound::QueuedSound;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenOverlay {
    None,
    Water,
    Lava,
    Fire,
}

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

/// One tick's gameplay input intents, set just before [`GameState::tick`] so the
/// tick turns them into serverbound packets in vanilla `runTick` order — clicks
/// are resolved BEFORE the move, so a sprint-attack/sword-block slowdown lands on
/// the same flying packet the action does (what Grim's attack-slow / NoSlow
/// windows expect).
#[derive(Debug, Clone, Copy, Default)]
pub struct TickActions {
    pub slot_select: Option<i32>,
    pub slot_scroll: i32,
    pub attack_pressed: bool,
    pub use_pressed: bool,
    pub left_held: bool,
    pub right_held: bool,
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
    pub fn handle_key(&mut self, event: KeyEvent, keybinds: &Keybinds) {
        let pressed = event.state == ElementState::Pressed;
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };
        // Arrow-key view turning is a recraft extra (not a vanilla rebindable
        // control), so it stays on the fixed arrow keys.
        match code {
            KeyCode::ArrowLeft => return self.turn_left = pressed,
            KeyCode::ArrowRight => return self.turn_right = pressed,
            KeyCode::ArrowUp => return self.look_up = pressed,
            KeyCode::ArrowDown => return self.look_down = pressed,
            _ => {}
        }
        match keybinds.action_for(code) {
            Some(GameAction::Forward) => self.forward = pressed,
            Some(GameAction::Back) => self.backward = pressed,
            Some(GameAction::Left) => self.left = pressed,
            Some(GameAction::Right) => self.right = pressed,
            Some(GameAction::Jump) => self.jump = pressed,
            Some(GameAction::Sneak) => self.sneak = pressed,
            // Sprint is a toggle (not hold): each press of the sprint key flips
            // the intent. It is cleared by the wall-cancel and by sneaking.
            Some(GameAction::Sprint) if pressed => self.sprint = !self.sprint,
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
            // `sprint` is set by GameState::tick from the computed sprint state
            // (the onLivingUpdate logic), not from the raw input.
            ..PlayerInput::default()
        }
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

/// Vanilla sprint speed-boost modifier UUID
/// (662a6b8d-da3e-4c1c-8813-96ea6097278d). The server includes it in the
/// movement-speed attribute while we sprint; it is excluded from the synced
/// value because physics applies the 1.3 sprint multiplier itself.
const SPRINT_SPEED_BOOST_UUID: [u8; 16] = [
    0x66, 0x2a, 0x6b, 0x8d, 0xda, 0x3e, 0x4c, 0x1c, 0x88, 0x13, 0x96, 0xea, 0x60, 0x97, 0x27, 0x8d,
];

/// Vanilla `ModifiableAttributeInstance.computeValue`: base plus the additive
/// modifiers (op 0), then ×(1 + Σ op 1), then ×(1 + amount) per op 2 —
/// skipping the modifier with the `excluded` UUID.
fn effective_attribute_value(
    property: &recraft_protocol::v1_8_9::packets::EntityProperty,
    excluded: &[u8; 16],
) -> f64 {
    let modifiers = || property.modifiers.iter().filter(|m| &m.uuid != excluded);
    let mut d0 = property.base;
    for modifier in modifiers().filter(|m| m.operation == 0) {
        d0 += modifier.amount;
    }
    let mut d1 = d0;
    for modifier in modifiers().filter(|m| m.operation == 1) {
        d1 += d0 * modifier.amount;
    }
    for modifier in modifiers().filter(|m| m.operation == 2) {
        d1 *= 1.0 + modifier.amount;
    }
    d1.max(0.0)
}

/// Snapshot driving the first-person hand/item rendering for one frame
/// (vanilla ItemRenderer inputs at a given partialTicks).
#[derive(Debug, Clone)]
pub struct FirstPersonView {
    /// The item the hand currently shows (lags the selection by the equip dip).
    pub item: Option<SlotItem>,
    /// Vanilla `1 - equippedProgress`: 0 = fully raised, 1 = fully lowered.
    pub equip_progress: f32,
    pub swing_progress: f32,
    /// `rotateWithPlayerRotations` lag rotations, in degrees.
    pub arm_lag_pitch: f32,
    pub arm_lag_yaw: f32,
    /// The active use action (blocking, eating, drinking, bow draw) driving
    /// the first-person pose and the movement slowdown.
    pub use_action: ItemUseAction,
    /// How many ticks the item has been in use (includes partial tick for smooth animation).
    pub use_ticks: f32,
}

/// Vanilla `EnumAction`: the client-side use state for the held item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemUseAction {
    None,
    Block,
    Eat,
    Drink,
    Bow,
}

/// Standing eye height above the feet, in blocks (vanilla 1.8).
const STANDING_EYE_HEIGHT: f64 = 1.62;
/// How far the camera drops below the standing eye height while sneaking.
const SNEAK_EYE_DROP: f64 = 0.08;
/// Field-of-view added on top of the base FOV while sprinting, in degrees.
const SPRINT_FOV_BOOST: f32 = 10.0;
const BASE_FOV: f32 = 70.0;

const DEFAULT_TITLE_FADE_IN_TICKS: i32 = 10;
const DEFAULT_TITLE_STAY_TICKS: i32 = 70;
const DEFAULT_TITLE_FADE_OUT_TICKS: i32 = 20;

/// Ready-to-render title overlay snapshot.
#[derive(Debug, Clone, Copy)]
pub struct TitleOverlay<'a> {
    pub title: &'a str,
    pub subtitle: &'a str,
    pub alpha: f32,
}

#[derive(Debug, Clone)]
struct TitleState {
    title: String,
    subtitle: String,
    timer: i32,
    fade_in: i32,
    stay: i32,
    fade_out: i32,
}

impl Default for TitleState {
    fn default() -> Self {
        Self {
            title: String::new(),
            subtitle: String::new(),
            timer: 0,
            fade_in: DEFAULT_TITLE_FADE_IN_TICKS,
            stay: DEFAULT_TITLE_STAY_TICKS,
            fade_out: DEFAULT_TITLE_FADE_OUT_TICKS,
        }
    }
}

impl TitleState {
    fn overlay(&self, partial_ticks: f32) -> Option<TitleOverlay<'_>> {
        if self.timer <= 0 {
            return None;
        }
        let alpha = self.alpha(partial_ticks);
        if alpha <= 8.0 / 255.0 || (self.title.is_empty() && self.subtitle.is_empty()) {
            return None;
        }
        Some(TitleOverlay {
            title: &self.title,
            subtitle: &self.subtitle,
            alpha,
        })
    }

    fn alpha(&self, partial_ticks: f32) -> f32 {
        let partial = partial_ticks.clamp(0.0, 1.0);
        let remaining = self.timer as f32 - partial;
        let total = self.total_ticks() as f32;
        let mut alpha = 1.0;
        if self.fade_in > 0 && self.timer > self.fade_out + self.stay {
            let elapsed = total - remaining;
            alpha = elapsed / self.fade_in as f32;
        }
        if self.fade_out > 0 && self.timer <= self.fade_out {
            alpha = remaining / self.fade_out as f32;
        }
        alpha.clamp(0.0, 1.0)
    }

    fn total_ticks(&self) -> i32 {
        self.fade_in + self.stay + self.fade_out
    }

    fn clear(&mut self) {
        self.title.clear();
        self.subtitle.clear();
        self.timer = 0;
    }

    fn reset_times(&mut self) {
        self.fade_in = DEFAULT_TITLE_FADE_IN_TICKS;
        self.stay = DEFAULT_TITLE_STAY_TICKS;
        self.fade_out = DEFAULT_TITLE_FADE_OUT_TICKS;
    }

    fn apply(&mut self, action: TitleAction) {
        match action {
            TitleAction::Title { json } => {
                self.title = chat::flatten_chat_json(&json);
                self.timer = self.total_ticks();
            }
            TitleAction::Subtitle { json } => {
                self.subtitle = chat::flatten_chat_json(&json);
            }
            TitleAction::Times {
                fade_in,
                stay,
                fade_out,
            } => {
                if fade_in >= 0 {
                    self.fade_in = fade_in;
                }
                if stay >= 0 {
                    self.stay = stay;
                }
                if fade_out >= 0 {
                    self.fade_out = fade_out;
                }
                if self.timer > 0 {
                    self.timer = self.total_ticks();
                }
            }
            TitleAction::Clear => self.clear(),
            TitleAction::Reset => {
                self.clear();
                self.reset_times();
            }
        }
    }

    fn tick(&mut self) {
        if self.timer <= 0 {
            return;
        }
        self.timer -= 1;
        if self.timer <= 0 {
            self.title.clear();
            self.subtitle.clear();
        }
    }
}

pub struct GameState {
    pub world: World,
    pub input: InputState,
    pub camera: Camera,
    player: EntityState,
    previous_player_position: DVec3,
    physics: PlayerPhysics,
    has_sky_light: bool,
    /// Run a client-side block-light flood-fill on placement/break. Demos have no
    /// server lightmap, so emissive blocks must be lit locally; off in multiplayer
    /// (the server is authoritative for light there).
    local_lighting: bool,
    /// World time of day in ticks (0..24000), driving the day/night sky and
    /// lightmap. Advanced locally each tick and resynced by S03 TimeUpdate;
    /// starts at noon until the server reports otherwise.
    world_time: i64,
    /// Whether time advances locally (gamerule doDaylightCycle): cleared when
    /// the server sends a negative `time_of_day`.
    daylight_cycle: bool,
    /// World-time ticks advanced per game tick (1 = vanilla 20-min day; demos
    /// run faster so the day/night cycle is visible without waiting).
    time_rate: i64,
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
    /// Last server-reported food saturation (drives the hunger-bar jiggle).
    saturation: f32,
    /// Set once UpdateHealth has been received, so the first server health value
    /// (the spawn value) plays no hurt sound and seeds no heart highlight.
    health_received: bool,
    /// Vanilla `GuiIngame.updateCounter`: a free-running client-tick counter that
    /// seeds the heart-shake RNG and the heart/hunger blink timing.
    hud_update_counter: i32,
    /// Vanilla `GuiIngame.healthUpdateCounter`: the tick (in `hud_update_counter`
    /// units) until which the heart row draws its blinking highlight frame.
    health_update_counter: i64,
    /// Vanilla `GuiIngame.lastPlayerHealth` (`j`): the health snapshot drawn in the
    /// highlight frame while the row blinks after a health change.
    last_player_health: i32,
    /// Experience bar fill (0..1) and level, from SetExperience.
    xp_bar: f32,
    xp_level: i32,
    /// Set when health hit 0; cleared on respawn. The UI shows the death screen
    /// and the player must click respawn (vanilla holds a dead player frozen).
    is_dead: bool,
    /// The shared 45-slot player inventory (window 0): 0 craft output, 1-4
    /// crafting, 5-8 armor, 9-35 main, 36-44 hotbar. Server-opened windows alias
    /// its lower 36 slots, exactly like vanilla's `InventoryPlayer`.
    inventory: Vec<Option<SlotItem>>,
    /// The stack carried on the cursor while a window is open (vanilla
    /// `inventory.getItemStack()`, the slot -1 item).
    cursor_item: Option<SlotItem>,
    /// The currently open window: the player inventory ([`Container::player`],
    /// opened with E) or a server-opened container (chest/furnace/…). `None`
    /// means no window is open.
    open_container: Option<Container>,
    /// In-progress paint-drag deferred to mouse-release (vanilla sends the
    /// mode-5 sequence then): the button (0 left even-split, 1 right one-each,
    /// 2 middle fill) and the window slots painted so far.
    drag_active: bool,
    drag_button: i8,
    drag_slots: Vec<i16>,
    /// Set when the server opens (S2D) or force-closes (S2E) a window, so the
    /// host can push/pop the container screen. Drained by `take_window_*`.
    window_open_pending: bool,
    window_close_pending: bool,
    creative: bool,
    /// Account UUID per spawned remote entity (players carry it via SpawnPlayer),
    /// linking the entity to its tab-list roster entry for names and skins.
    entity_uuids: std::collections::HashMap<EntityId, [u8; 16]>,
    /// The item stack carried by each dropped-item entity (from EntityMetadata
    /// index 10), used to render the floating item.
    entity_items: std::collections::HashMap<EntityId, SlotItem>,
    /// Experience value carried by each XP-orb entity (S11 SpawnExperienceOrb),
    /// used to pick the orb's sprite cell.
    entity_xp: std::collections::HashMap<EntityId, i16>,
    /// Blockstate carried by each falling-block entity (SpawnObject kind 70,
    /// packed in its data int), used to render the falling cube with terrain
    /// textures.
    falling_blocks: std::collections::HashMap<EntityId, BlockState>,
    /// Equipment worn/held by other entities (S04 EntityEquipment). Indexed
    /// 0 = held, 1 = boots, 2 = leggings, 3 = chestplate, 4 = helmet.
    entity_equipment: std::collections::HashMap<EntityId, [Option<SlotItem>; 5]>,
    /// Passenger → vehicle mount relationships (S1B AttachEntity). Drives the
    /// rider render offset and the local player following its mount.
    vehicles: std::collections::HashMap<EntityId, EntityId>,
    dirty_chunks: HashSet<SectionPos>,
    /// Sections changed by local place/break prediction. They are submitted to
    /// the background mesher before ordinary dirty sections, but never rebuilt on
    /// the render thread.
    urgent_remesh: HashSet<SectionPos>,
    /// Block changes received for chunks that weren't loaded yet, replayed once
    /// the chunk arrives (otherwise spawn-platform blocks can be lost).
    pending_block_changes: std::collections::HashMap<ChunkPos, Vec<(i32, i32, i32, BlockState)>>,
    /// Per-chest lid-open amount (0 = closed .. 1 = fully open), keyed by world
    /// block position, eased toward its target each tick. Entries that reach 0
    /// (fully closed) are pruned. The open target is driven by S24 BlockAction
    /// (viewer count); see [`GameState::tick_chest_lids`].
    chest_lid_angles: std::collections::HashMap<[i32; 3], f32>,
    /// Per-chest lid-open TARGET (1 while a viewer has it open, 0 otherwise),
    /// set by the S24 BlockAction viewer count; `chest_lid_angles` eases toward it.
    chest_open_targets: std::collections::HashMap<[i32; 3], f32>,
    // Smoothed 0..1 view-state amounts, advanced once per physics tick so the
    // sneak camera dip and sprint FOV widen ease in/out instead of snapping.
    sneak_amount: f32,
    previous_sneak_amount: f32,
    sprint_amount: f32,
    previous_sprint_amount: f32,
    // Vanilla arm-swing state: tick counter through the 6-tick animation,
    // whether a swing is in progress, and the prev/current 0..1 progress for
    // partial-tick interpolation (EntityLivingBase.swingProgress).
    swing_progress_int: i32,
    is_swinging: bool,
    swing_progress: f32,
    prev_swing_progress: f32,
    // Vanilla ItemRenderer equip animation: the rendered item lags the
    // selection, dipping out of view (progress sinks below 0.1) before the
    // newly selected item rises back up.
    equipped_progress: f32,
    prev_equipped_progress: f32,
    rendered_item: Option<SlotItem>,
    equipped_slot: i32,
    // Vanilla renderArmPitch/renderArmYaw: a 0.5-lerp-per-tick smoothed copy
    // of the view rotation; the hand lags the camera by 10% of the gap.
    render_arm_pitch: f32,
    render_arm_yaw: f32,
    prev_render_arm_pitch: f32,
    prev_render_arm_yaw: f32,
    // Selected hotbar slot, 0..9.
    selected_slot: i32,
    /// The block currently being mined (survival), with accumulated progress.
    breaking: Option<BreakProgress>,
    /// Vanilla `itemInUse`: the active use action for the held item (blocking,
    /// eating, drinking, bow draw). Drives the first-person pose, the 0.2×
    /// movement slowdown, and gates the C07 release packet.
    use_action: ItemUseAction,
    use_item_ticks: i32,
    /// Vanilla `Minecraft.leftClickCounter`: a 10-tick lockout after a survival
    /// left-click that hits nothing — blocks new attacks/digs while it runs.
    left_click_counter: i32,
    /// Vanilla `Minecraft.rightClickDelayTimer`: the gap (4 ticks) between
    /// auto-repeated held right-clicks.
    right_click_delay_timer: i32,
    /// Vanilla `PlayerControllerMP.blockHitDelay`: 5-tick pause after a block
    /// breaks before the next dig may progress.
    block_hit_delay: i32,
    /// Vanilla `PlayerControllerMP.currentPlayerItem`: the hotbar slot last
    /// reported to the server. C09 is sent lazily (syncCurrentPlayItem) on the
    /// next interaction, not on the slot change itself.
    current_player_item: i32,
    /// This tick's input intents, consumed inside `tick` before the move.
    pending_actions: TickActions,
    /// Vanilla `isSprinting()`. Recomputed each tick by the `onLivingUpdate`
    /// sprint logic (start conditions then stop, in that order).
    sprinting: bool,
    /// Vanilla `sprintToggleTimer`: the 7-tick double-tap-W window.
    sprint_toggle_timer: i32,
    /// Set on the tick an attack cancels sprint (the "w-tap reset"); blocks
    /// the sprint-key-held re-enable for THIS tick so the server sees the
    /// StopSprinting before the next StartSprinting.
    sprint_reset_by_attack: bool,
    /// `movementInput.sneak` / `moveForward` from the previous tick (vanilla's
    /// `flag1` / `flag2`, read before `updatePlayerMoveState`).
    prev_sneak: bool,
    prev_move_forward: f32,
    /// Server-driven abilities (S39); `flying` is also toggled locally.
    capabilities: PlayerCapabilities,
    /// The movementSpeed attribute from S20 (sprint boost excluded); drives
    /// ground speed so speed potions / walk-speed plugins stay in sync.
    walk_speed_attribute: f32,
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
    /// Tab-list roster keyed by UUID (S38/S47), drives the tab overlay, player
    /// nametags and skin lookups.
    pub player_list: PlayerList,
    title: TitleState,
    /// Client-side particle effects (S2A SpawnParticle + local block breaks),
    /// simulated here and drawn as billboards by the renderer.
    particles: ParticleSystem,
    /// Sounds queued this frame by packets / local prediction, drained by the
    /// host (`main.rs`) into the audio backend so the game layer stays
    /// audio-free. World coordinates map 1:1 to render coordinates here.
    sound_queue: Vec<QueuedSound>,
}

/// In-progress survival block break: the target cell, the face the dig started
/// on, accumulated 0..1 progress and how much to add each 20 Hz tick.
#[derive(Debug, Clone)]
struct BreakProgress {
    x: i32,
    y: i32,
    z: i32,
    /// Vanilla `curBlockDamageMP`: accumulated 0..1 break progress.
    progress: f32,
    /// Per-tick increment (`getPlayerRelativeBlockHardness`).
    per_tick: f32,
    /// Vanilla `currentItemHittingBlock`: the held stack when the dig began.
    /// A change (hotbar switch) makes `isHittingPosition` fail → the dig
    /// restarts, exactly like vanilla.
    item: Option<SlotItem>,
}

/// Vanilla arm-swing length in ticks (getArmSwingAnimationEnd, no haste).
const ARM_SWING_END_TICKS: i32 = 6;

/// 1.8 sword item ids (wood/gold/stone/iron/diamond).
fn is_sword(id: i16) -> bool {
    matches!(id, 268 | 283 | 272 | 267 | 276)
}

/// Vanilla `Item.getItemUseAction` for 1.8.9 items.
fn item_use_action(id: i16, damage: i16) -> ItemUseAction {
    match id {
        268 | 272 | 267 | 276 | 283 => ItemUseAction::Block,
        261 => ItemUseAction::Bow,
        373 if damage & 0x4000 == 0 => ItemUseAction::Drink,
        335 => ItemUseAction::Drink,
        260 | 282 | 297 | 319 | 320 | 322 | 349 | 350 | 357 | 360 | 363 | 364 | 365 | 366
        | 367 | 375 | 391 | 392 | 393 | 394 | 396 | 400 | 412 | 413 | 423 | 424 => {
            ItemUseAction::Eat
        }
        _ => ItemUseAction::None,
    }
}

/// Whether a held item id is a placeable block (`ItemBlock`), used to decide if
/// a right-click on a block was consumed by placing (vanilla `onItemUse`). Block
/// items share the block id range 1..256.
fn is_block_item(id: i16) -> bool {
    (1..256).contains(&id)
}

impl GameState {
    pub fn demo(kind: DemoKind, aspect: f32) -> Self {
        let mut world = World::new();
        let spawn = match kind {
            DemoKind::Landscape => build_demo_landscape(&mut world),
            DemoKind::ChunkStress => build_demo_chunk_stress(&mut world),
            DemoKind::EntityStress => build_demo_entity_stress(&mut world),
            DemoKind::Terrain => build_demo_terrain(&mut world),
            DemoKind::SingleCube => build_demo_single_cube(&mut world),
        };
        // Demo worlds default every cell to full sky-light; cast it vertically so
        // caves/interiors are dark (and lit only by placed block sources).
        world.recompute_vertical_skylight();
        let mut state = Self::new(world, EntityId(0), spawn, aspect);
        state.capabilities.allow_flying = true;
        state.local_lighting = true;
        if matches!(kind, DemoKind::ChunkStress) {
            state.camera.pitch = 45.0;
        }
        if matches!(kind, DemoKind::SingleCube) {
            // Look straight ahead (+Z) at the lone block; hover so nothing drifts.
            state.camera.pitch = 8.0;
            state.capabilities.flying = true;
        }
        if matches!(kind, DemoKind::Terrain) {
            // A fixed vista over the whole landscape so the GPU load is stable
            // and comparable across mesher changes. Hover (flying) so gravity
            // never drifts the camera during the static benchmark.
            state.camera.pitch = 18.0;
            state.camera.yaw = 30.0;
            state.capabilities.flying = true;
        }
        // Demo worlds run the day/night cycle ~12× so it's visible in a minute or
        // two (a full vanilla day is 20 min), and stock the hotbar with light
        // sources so emissive lighting/shadows are easy to try at night.
        state.time_rate = 12;
        // glowstone, torch, sea lantern, jack-o-lantern, redstone lamp (lit), stone
        let hotbar = [89, 50, 169, 91, 124, 1];
        for (i, id) in hotbar.into_iter().enumerate() {
            state.inventory[36 + i] = Some(SlotItem::new(id, 64, 0));
        }
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
            local_lighting: false,
            world_time: 6000,
            daylight_cycle: true,
            time_rate: 1,
            joined_game: false,
            position_synced: false,
            pending_confirm: false,
            freeze_movement_after_teleport: false,
            needs_respawn: false,
            health: 20.0,
            food: 20,
            saturation: 5.0,
            health_received: false,
            hud_update_counter: 0,
            health_update_counter: 0,
            last_player_health: 20,
            xp_bar: 0.0,
            xp_level: 0,
            is_dead: false,
            inventory: vec![None; 45],
            cursor_item: None,
            open_container: None,
            drag_active: false,
            drag_button: 0,
            drag_slots: Vec::new(),
            window_open_pending: false,
            window_close_pending: false,
            creative: false,
            entity_uuids: std::collections::HashMap::new(),
            entity_items: std::collections::HashMap::new(),
            entity_xp: std::collections::HashMap::new(),
            falling_blocks: std::collections::HashMap::new(),
            entity_equipment: std::collections::HashMap::new(),
            vehicles: std::collections::HashMap::new(),
            dirty_chunks: HashSet::new(),
            urgent_remesh: HashSet::new(),
            pending_block_changes: std::collections::HashMap::new(),
            chest_lid_angles: std::collections::HashMap::new(),
            chest_open_targets: std::collections::HashMap::new(),
            sneak_amount: 0.0,
            previous_sneak_amount: 0.0,
            sprint_amount: 0.0,
            previous_sprint_amount: 0.0,
            swing_progress_int: 0,
            is_swinging: false,
            swing_progress: 0.0,
            prev_swing_progress: 0.0,
            equipped_progress: 0.0,
            prev_equipped_progress: 0.0,
            rendered_item: None,
            equipped_slot: 0,
            render_arm_pitch: 0.0,
            render_arm_yaw: 0.0,
            prev_render_arm_pitch: 0.0,
            prev_render_arm_yaw: 0.0,
            selected_slot: 0,
            breaking: None,
            use_action: ItemUseAction::None,
            use_item_ticks: 0,
            left_click_counter: 0,
            right_click_delay_timer: 0,
            block_hit_delay: 0,
            current_player_item: 0,
            pending_actions: TickActions::default(),
            sprinting: false,
            sprint_toggle_timer: 0,
            sprint_reset_by_attack: false,
            prev_sneak: false,
            prev_move_forward: 0.0,
            capabilities: PlayerCapabilities::default(),
            walk_speed_attribute: 0.1,
            fly_toggle_timer: 0,
            was_jump_down: false,
            abilities_dirty: false,
            chat: ChatState::default(),
            scoreboard: Scoreboard::default(),
            player_list: PlayerList::default(),
            title: TitleState::default(),
            particles: ParticleSystem::new(),
            sound_queue: Vec::new(),
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

    /// Whether the player is in creative mode (drives middle-click clone and
    /// the fill paint-drag in container windows).
    pub fn is_creative(&self) -> bool {
        self.creative
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

    /// Heart/hunger HUD animation state (vanilla `renderPlayerStats`): the live
    /// food saturation plus the tick counters that drive the heart-shake RNG and
    /// the heart-row blink/highlight.
    pub fn hud_vitals(&self) -> crate::gui::ingame::HudVitals {
        crate::gui::ingame::HudVitals {
            saturation: self.saturation,
            // Absorption and max-health are not plumbed: the server carries
            // absorption only in EntityProperties/metadata (not parsed for the
            // local player), and UpdateHealth has no absorption field. Default to
            // the vanilla base 20 HP / 0 absorption.
            max_health: 20.0,
            absorption: 0.0,
            update_counter: self.hud_update_counter,
            health_update_counter: self.health_update_counter,
            last_player_health: self.last_player_health,
        }
    }

    /// World time of day in ticks, interpolated by `tick_alpha` (0..1) for a
    /// smooth sky between ticks. Drives the renderer's day/night cycle.
    /// Handle a demo-world chat command (text after the leading `/`). Returns the
    /// feedback line to show in chat. Currently supports `/time`.
    pub fn run_demo_command(&mut self, cmd: &str) -> String {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        match parts.as_slice() {
            ["time", "set", v] => {
                let t = match *v {
                    "day" => Some(1000),
                    "noon" => Some(6000),
                    "night" => Some(13000),
                    "midnight" => Some(18000),
                    n => n.parse::<i64>().ok(),
                };
                match t {
                    Some(t) => {
                        self.world_time = t.rem_euclid(24000);
                        format!("Set the time to {}", self.world_time)
                    }
                    None => "Usage: /time set <day|noon|night|midnight|ticks>".to_owned(),
                }
            }
            ["time", "add", n] => match n.parse::<i64>() {
                Ok(n) => {
                    self.world_time = (self.world_time + n).rem_euclid(24000);
                    format!("Added {n} to the time (now {})", self.world_time)
                }
                Err(_) => "Usage: /time add <ticks>".to_owned(),
            },
            ["time", "rate", n] => match n.parse::<i64>() {
                Ok(n) => {
                    self.time_rate = n.max(0);
                    format!("Day/night rate set to {}x", self.time_rate)
                }
                Err(_) => "Usage: /time rate <ticks-per-tick>".to_owned(),
            },
            ["time", ..] => "Usage: /time set|add <…> or /time rate <n>".to_owned(),
            _ => format!("Unknown command: /{cmd}"),
        }
    }

    pub fn world_time(&self, tick_alpha: f32) -> f64 {
        self.world_time as f64
            + (if self.daylight_cycle { self.time_rate } else { 0 }) as f64 * tick_alpha as f64
    }

    /// Experience bar fill, 0..1.
    pub fn xp_bar(&self) -> f32 {
        self.xp_bar
    }

    pub fn xp_level(&self) -> i32 {
        self.xp_level
    }

    pub fn title_overlay(&self, partial_ticks: f32) -> Option<TitleOverlay<'_>> {
        self.title.overlay(partial_ticks)
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

    /// Total armor points from equipped armor (inventory slots 5-8).
    pub fn armor(&self) -> i32 {
        self.inventory[5..9]
            .iter()
            .filter_map(|s| s.as_ref())
            .map(|item| armor_points(item.id))
            .sum()
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

    /// The stack currently carried on the cursor (vanilla slot -1).
    pub fn cursor_item(&self) -> Option<&SlotItem> {
        self.cursor_item.as_ref()
    }

    /// The open window (player inventory or a server container), if any. The
    /// container screen reads its slot layout, kind and properties from this.
    pub fn open_container(&self) -> Option<&Container> {
        self.open_container.as_ref()
    }

    /// The downloaded-skin atlas row for the local player, resolved by matching
    /// `name` against the tab-list roster (the local player has no entry in the
    /// entity-uuid map). `None` falls back to the default player skin.
    pub fn local_skin_row(
        &self,
        name: &str,
        skin_rows: &std::collections::HashMap<[u8; 16], u32>,
    ) -> Option<u32> {
        let (uuid, _) = self.player_list.iter().find(|(_, info)| info.name == name)?;
        skin_rows.get(uuid).copied()
    }

    /// Whether a paint-drag is in progress (the screen defers the commit to the
    /// mouse release, falling back to a normal click when only one slot painted).
    pub fn container_drag_active(&self) -> bool {
        self.drag_active
    }

    /// Number of slots painted in the current drag.
    pub fn container_drag_len(&self) -> usize {
        self.drag_slots.len()
    }

    /// The active drag button (0 left even-split, 1 right one-each, 2 middle fill).
    pub fn container_drag_button(&self) -> i8 {
        self.drag_button
    }

    /// Drain the "server opened a window" / "server force-closed the window"
    /// signals so the host can push/pop the container screen.
    pub fn take_window_open(&mut self) -> bool {
        std::mem::take(&mut self.window_open_pending)
    }

    pub fn take_window_close(&mut self) -> bool {
        std::mem::take(&mut self.window_close_pending)
    }

    /// Drain the sounds queued this frame (by packets and local prediction) so
    /// the host can play them through the audio backend.
    pub fn take_sounds(&mut self) -> Vec<QueuedSound> {
        std::mem::take(&mut self.sound_queue)
    }

    /// Queue a positional sound event at a world position.
    fn queue_sound(&mut self, event: impl Into<String>, pos: Vec3, volume: f32, pitch: f32) {
        self.sound_queue.push(QueuedSound {
            event: event.into(),
            position: Some(pos),
            volume,
            pitch,
        });
    }

    // ─── Window interaction (vanilla Container.slotClick via container.rs) ─────

    /// Open the player inventory window (E key) — vanilla `ContainerPlayer`.
    pub fn open_player_inventory(&mut self) {
        self.open_container = Some(Container::player());
    }

    /// Run a click on the open window through the vanilla `Container.slotClick`
    /// port: it predicts locally and returns the matching C0E ClickWindow
    /// packet(s); the server confirms or corrects with SetSlot. `slot` is a
    /// window slot index, or -999 for outside the window.
    pub fn container_click(&mut self, slot: i16, button: i8, mode: i8) -> Vec<ServerboundPacket> {
        let creative = self.creative;
        let Some(container) = self.open_container.as_mut() else {
            return Vec::new();
        };
        container.window_click(
            slot,
            button,
            mode,
            &mut self.inventory,
            &mut self.cursor_item,
            creative,
        )
    }

    /// Close the open window: clear local drag/cursor state and return the
    /// CloseWindow packet (the server drops any cursor stack and re-syncs).
    pub fn container_close(&mut self) -> Option<ServerboundPacket> {
        self.container_drag_cancel();
        self.cursor_item = None;
        let container = self.open_container.take()?;
        Some(ServerboundPacket::CloseWindow {
            window_id: container.window_id,
        })
    }

    /// Begin accumulating a paint-drag (local only until [`container_drag_commit`]).
    pub fn container_drag_begin(&mut self, button: i8) {
        if self.cursor_item.is_some() {
            self.drag_active = true;
            self.drag_button = button;
            self.drag_slots.clear();
        }
    }

    /// Add a window slot to the active drag if it is a legal, not-yet-painted
    /// target with room (the Container re-checks on commit, as does the server).
    pub fn container_drag_add(&mut self, slot: i16) {
        if !self.drag_active || slot < 0 || self.drag_slots.contains(&slot) {
            return;
        }
        let Some(ref cursor) = self.cursor_item else {
            return;
        };
        let Some(container) = self.open_container.as_ref() else {
            return;
        };
        if slot as usize >= container.slots().len() {
            return;
        }
        // Even-split / one-each can't paint more slots than the cursor has items;
        // fill (middle, creative) has no such cap.
        if self.drag_button != 2 && self.drag_slots.len() as u32 >= cursor.count as u32 {
            return;
        }
        let valid = match container.slot_item(slot as usize, &self.inventory) {
            None => true,
            Some(it) => stackable(&it, &cursor) && (it.count) < max_stack(&cursor),
        };
        if valid {
            self.drag_slots.push(slot);
        }
    }

    /// Commit the accumulated drag: replay the vanilla mode-5 sequence (start,
    /// one add per painted slot, end) through the Container, which distributes
    /// the cursor stack on the end click and yields the ClickWindow packets.
    pub fn container_drag_commit(&mut self) -> Vec<ServerboundPacket> {
        if !self.drag_active {
            return Vec::new();
        }
        self.drag_active = false;
        let button = self.drag_button;
        let slots = std::mem::take(&mut self.drag_slots);
        if slots.len() <= 1 {
            // Not a real paint — the caller falls back to a normal click.
            return Vec::new();
        }
        let base = button << 2; // mode 0 split / 1 one-each / 2 fill in bits 2-3
        let mut packets = self.container_click(-999, base, 5);
        for slot in slots {
            packets.extend(self.container_click(slot, base | 1, 5));
        }
        packets.extend(self.container_click(-999, base | 2, 5));
        packets
    }

    /// Abandon the active drag without sending anything (used when a drag
    /// collapses to a single-slot normal click).
    pub fn container_drag_cancel(&mut self) {
        self.drag_active = false;
        self.drag_slots.clear();
    }

    pub fn loaded_chunk_count(&self) -> usize {
        self.world.chunk_count()
    }

    /// Player feet position in world coordinates (vanilla `posX/posY/posZ`),
    /// for the F3 debug overlay.
    pub fn player_position(&self) -> DVec3 {
        self.player.position
    }

    /// Whether the player is standing on the ground (F3 debug overlay).
    pub fn player_on_ground(&self) -> bool {
        self.player.on_ground
    }

    pub fn screen_overlay(&self) -> ScreenOverlay {
        let eye_y = self.player.position.y + STANDING_EYE_HEIGHT
            - SNEAK_EYE_DROP * self.sneak_amount as f64;
        let bx = self.player.position.x.floor() as i32;
        let by = eye_y.floor() as i32;
        let bz = self.player.position.z.floor() as i32;
        let block = self.world.block_at(bx, by, bz);
        if block.is_water() {
            ScreenOverlay::Water
        } else if block.is_lava() {
            ScreenOverlay::Lava
        } else if self.player.on_fire {
            ScreenOverlay::Fire
        } else {
            ScreenOverlay::None
        }
    }

    /// Stage this tick's input intents; [`tick`](Self::tick) consumes them.
    pub fn set_pending_actions(&mut self, actions: TickActions) {
        self.pending_actions = actions;
    }

    /// Turn this tick's input intents into serverbound packets, a 1:1 port of
    /// the mouse/keybind section of vanilla `Minecraft.runTick`. Called inside
    /// `tick` BEFORE the move so any state it changes (dig start, sword-block
    /// item-use, sprint reset on a hit) lands on this tick's flying packet,
    /// matching vanilla. The timers (left_click_counter / right_click_delay_timer)
    /// are decremented by the caller just before this, like runTick.
    fn process_tick_actions(&mut self) -> Vec<ServerboundPacket> {
        let a = std::mem::take(&mut self.pending_actions);
        let mut out = Vec::new();

        // Hotbar slot change. Vanilla sets `inventory.currentItem` directly here
        // and does NOT send C09 — that goes out lazily via syncCurrentPlayItem
        // inside the next click/dig/use.
        let prev_slot = self.selected_slot;
        if let Some(slot) = a.slot_select {
            self.selected_slot = slot.clamp(0, 8);
        } else if a.slot_scroll != 0 {
            self.selected_slot = (self.selected_slot + a.slot_scroll).rem_euclid(9);
        }
        if self.selected_slot != prev_slot && self.is_using_item() {
            self.on_stopped_using_item(&mut out);
        }

        // Clear use state if the held item was consumed/changed by the server.
        if self.is_using_item() {
            if self
                .held_item()
                .map_or(true, |it| item_use_action(it.id, it.damage) != self.use_action)
            {
                self.use_action = ItemUseAction::None;
                self.use_item_ticks = 0;
            } else {
                self.use_item_ticks += 1;
            }
        }

        // `if (isUsingItem()) { ... } else { ... }` — while an item is in use
        // (sword block), attack/use presses are swallowed; otherwise they drive
        // clickMouse / rightClickMouse.
        if self.is_using_item() {
            if !a.right_held {
                self.on_stopped_using_item(&mut out);
            }
            // attack/use/pick presses are drained (ignored) this tick.
        } else {
            if a.attack_pressed {
                self.click_mouse(&mut out);
            }
            if a.use_pressed {
                self.right_click_mouse(&mut out);
            }
            // pick-block: not implemented.
        }

        // Auto-repeat held right-click every `rightClickDelayTimer` ticks.
        if a.right_held && self.right_click_delay_timer == 0 && !self.is_using_item() {
            self.right_click_mouse(&mut out);
        }

        // Continuous left-click: advance the dig (or cancel it).
        self.send_click_block_to_controller(a.left_held, &mut out);

        out
    }

    /// Advance one 20 Hz simulation tick, returning the movement to report — or
    /// `None` on the tick right after a teleport, where vanilla emits only the
    /// teleport ack (already sent via [`take_position_confirm`]) and resumes
    /// movement next tick. The caller must not send a movement packet on `None`.
    pub fn tick(&mut self, dt: f32) -> Option<(Vec<ServerboundPacket>, MovementSnapshot)> {
        // Vanilla GuiIngame.updateTick: a free-running tick counter that drives the
        // heart-shake RNG and the heart/hunger blink timing.
        self.hud_update_counter = self.hud_update_counter.wrapping_add(1);
        self.title.tick();
        // Advance the day/night clock one tick (vanilla ticks world time forward
        // locally between server updates), unless daylight cycle is off.
        if self.daylight_cycle {
            self.world_time += self.time_rate;
        }
        // Advance particle effects once per tick (before any early return), so
        // smoke/flame age and move at the vanilla 20 Hz.
        self.particles.tick();
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

        self.update_arm_swing();
        self.update_equipped_item();
        self.update_render_arm();

        // Advance remote-entity interpolation: one lerp step per tick toward
        // the latest server target (vanilla newPosRotationIncrements). Dropped
        // items with no pending server correction run local EntityItem physics
        // instead, so a freshly-dropped item arcs immediately rather than
        // stalling in the air until the next position packet (T7).
        let player_id = self.player.id;
        for entity in self.world.entities_mut() {
            let item_simulating =
                entity.kind == EntityKind::Object(2) && entity.position_increments == 0;
            if entity.id != player_id && !item_simulating {
                entity.tick_interpolation();
            }
        }
        let simulating_items: Vec<EntityId> = self
            .world
            .entities()
            .filter(|e| {
                e.id != player_id
                    && e.kind == EntityKind::Object(2)
                    && e.position_increments == 0
            })
            .map(|e| e.id)
            .collect();
        for id in simulating_items {
            if let Some(mut item) = self.world.entity(id).cloned() {
                recraft_core::physics::tick_item(&self.world, &mut item);
                self.world.upsert_entity(item);
            }
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

        // Vanilla runTick: clickMouse/rightClickMouse run BEFORE onUpdate's move,
        // resolved against the current look and pre-move position. Refresh the
        // camera so interaction ray-casts use this tick's look, then turn the
        // intents into packets — the move below then reflects any sprint reset /
        // item-use slowdown they triggered. The click timers decrement first,
        // exactly like `runTick` (rightClickDelayTimer at the top, leftClickCounter
        // in the mouse section), so a fresh click this tick still sees them run.
        if self.right_click_delay_timer > 0 {
            self.right_click_delay_timer -= 1;
        }
        if self.left_click_counter > 0 {
            self.left_click_counter -= 1;
        }
        self.update_camera(1.0);
        let actions = self.process_tick_actions();

        // While mounted, the server drives the vehicle and the player rides
        // along: skip walking physics and snap to the vehicle (vanilla
        // `updateRidden`). The player can still look around. Dismount happens
        // server-side when the player sneaks (the sneak packet is already sent),
        // which sends an AttachEntity(-1) that clears this.
        if let Some(&vehicle_id) = self.vehicles.get(&self.player.id) {
            if let Some(vehicle) = self.world.entity(vehicle_id) {
                let (_, vehicle_height) = vehicle.size();
                self.player.position =
                    vehicle.position + DVec3::new(0.0, vehicle_height * 0.75, 0.0);
                self.player.velocity = DVec3::ZERO;
                self.player.on_ground = false;
                self.player.sync_aabb_to_position();
                self.sprinting = false;
                self.world.upsert_entity(self.player.clone());
                self.advance_view_state();
                self.update_camera(1.0);
                return Some((actions, self.movement_snapshot()));
            }
        }

        // Hold the player still (no physics) while:
        //  - the chunk under us hasn't arrived yet, so we don't fall through
        //    not-yet-generated terrain on join.
        let bx = self.player.position.x.floor() as i32;
        let bz = self.player.position.z.floor() as i32;
        // Sprint, a 1:1 port of `EntityPlayerSP.onLivingUpdate`. Runs before the
        // move so the reported flag matches the simulated motion. recraft's
        // sprint key is a toggle, so `self.input.sprint` stands in for
        // `keyBindSprint.isKeyDown()`; the double-tap-W path (sprintToggleTimer)
        // works regardless. (`sprintingTicksLeft` and the blindness potion are
        // not modelled — neither is set in the 1.8 client / recraft.)
        if self.sprint_toggle_timer > 0 {
            self.sprint_toggle_timer -= 1;
        }
        let flag1 = self.prev_sneak;
        let flag2 = self.prev_move_forward >= 0.8;
        // updatePlayerMoveState: this tick's moveForward (raw ±1, sneak ×0.3).
        let mut move_forward = (f32::from(self.input.forward) - f32::from(self.input.backward))
            * if self.input.sneak { 0.3 } else { 1.0 };
        let sprint_key_down = self.input.sprint;
        if self.is_using_item() {
            // Sword block slows the input to 0.2 (below the 0.8 sprint threshold).
            move_forward *= 0.2;
            self.sprint_toggle_timer = 0;
        }
        let flag3 = self.food > 6 || self.capabilities.allow_flying;
        // Double-tap-W start (on the ground, on a fresh forward press).
        // Suppress on the tick an attack just cancelled sprint so the server
        // sees StopSprinting before the re-enable (the "w-tap" gap).
        if !self.sprint_reset_by_attack
            && self.player.on_ground
            && !flag1
            && !flag2
            && move_forward >= 0.8
            && !self.sprinting
            && flag3
            && !self.is_using_item()
        {
            if self.sprint_toggle_timer <= 0 && !sprint_key_down {
                self.sprint_toggle_timer = 7;
            } else {
                self.sprinting = true;
            }
        }
        // Sprint-key-held start (no onGround requirement, exactly like vanilla).
        if !self.sprint_reset_by_attack
            && !self.sprinting
            && move_forward >= 0.8
            && flag3
            && !self.is_using_item()
            && sprint_key_down
        {
            self.sprinting = true;
        }
        // Stop: forward dropped below 0.8 (release / sneak / block), a wall, or low food.
        if self.sprinting
            && (move_forward < 0.8 || self.player.collided_horizontally || !flag3)
        {
            self.sprinting = false;
        }
        self.sprint_reset_by_attack = false;
        self.prev_sneak = self.input.sneak;
        self.prev_move_forward = move_forward;
        if self.world.is_block_column_loaded(bx, bz) {
            let mut input = self.input.player_input();
            input.sprint = self.sprinting;
            input.flying = self.capabilities.flying;
            input.fly_speed = self.capabilities.fly_speed;
            input.walk_speed = self.walk_speed_attribute;
            // Vanilla `EntityPlayerSP.onLivingUpdate`: using an item (sword
            // blocking) scales movement input to 0.2 — Grim's NoSlow expects it.
            if self.is_using_item() {
                input.forward *= 0.2;
                input.strafe *= 0.2;
            }
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
        Some((actions, self.movement_snapshot()))
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
        self.tick_chest_lids();
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

    /// Start the hand-swing animation (vanilla `swingItem`): a new swing only
    /// restarts when idle or past the half-way point, so swinging every tick
    /// while mining yields smooth full swings instead of resetting each tick.
    pub fn swing_arm(&mut self) {
        if !self.is_swinging
            || self.swing_progress_int >= ARM_SWING_END_TICKS / 2
            || self.swing_progress_int < 0
        {
            self.swing_progress_int = -1;
            self.is_swinging = true;
        }
    }

    /// Per-tick swing stepping (vanilla `updateArmSwingProgress`).
    fn update_arm_swing(&mut self) {
        self.prev_swing_progress = self.swing_progress;
        if self.is_swinging {
            self.swing_progress_int += 1;
            if self.swing_progress_int >= ARM_SWING_END_TICKS {
                self.swing_progress_int = 0;
                self.is_swinging = false;
            }
        } else {
            self.swing_progress_int = 0;
        }
        self.swing_progress = self.swing_progress_int as f32 / ARM_SWING_END_TICKS as f32;
    }

    /// Build the per-frame entity model geometry: a textured model per tracked
    /// entity (a skinned humanoid for players, a colored model for mobs and
    /// objects). Entities are drawn even while paused/dead so they stay
    /// visible behind menu overlays. The first-person hand/held item is
    /// appended by [`crate::item_renderer::ItemRenderer`]; the mining crack
    /// overlay is drawn by the renderer from `breaking_overlay()`.
    /// `tick_alpha` interpolates entity positions between simulation ticks
    /// (vanilla partialTicks) so movement stays smooth at any frame rate.
    pub fn build_entity_model(
        &self,
        mesh: &mut ModelMesh,
        tick_alpha: f32,
        brightness: f32,
        skin_rows: &std::collections::HashMap<[u8; 16], u32>,
        max_dist_sq: f64,
        old_animations: bool,
    ) {
        mesh.clear();
        let sun_b = recraft_render::sky::sun_brightness(self.world_time(tick_alpha));
        // Cull entities outside the view frustum up front: most mobs in a loaded
        // world are off-screen at any moment, and building each one's articulated
        // mesh is the dominant per-frame cost in entity-dense scenes.
        let frustum = self.camera.frustum();
        for entity in self.world.entities() {
            if entity.id == self.player.id {
                continue;
            }
            // Item entities (object type 2) are drawn as item geometry in the
            // separate world-item pass, not as a placeholder box. Experience
            // orbs render as billboards in their own per-frame pass.
            if entity.kind == EntityKind::Object(2) || entity.kind == EntityKind::ExperienceOrb {
                continue;
            }
            // Distance cull: distant mobs aren't worth the per-frame articulated
            // build (cull shorter than the terrain so weak machines skip the mob
            // crowd). Mirrored in entity_render_fingerprint so they don't churn
            // the cache either.
            if entity_dist_sq(entity, &self.camera, tick_alpha) > max_dist_sq {
                continue;
            }
            // Resolve the player's downloaded-skin row through its uuid, if any.
            let skin_row = (entity.kind == EntityKind::RemotePlayer)
                .then(|| self.entity_uuids.get(&entity.id))
                .flatten()
                .and_then(|uuid| skin_rows.get(uuid))
                .copied();
            // A passenger renders on top of its vehicle (vanilla mount offset),
            // not at its own server position.
            let feet = match self
                .vehicles
                .get(&entity.id)
                .and_then(|vehicle_id| self.world.entity(*vehicle_id))
            {
                Some(vehicle) => {
                    let (_, vehicle_height) = vehicle.size();
                    to_render_vec3(
                        vehicle.render_position(tick_alpha as f64)
                            + DVec3::new(0.0, vehicle_height * 0.75, 0.0),
                    )
                }
                None => to_render_vec3(entity.render_position(tick_alpha as f64)),
            };
            let (half_width, height) = entity.size();
            // Skip entities whose model box is fully outside the frustum. The box
            // is padded so model parts that overhang the hitbox (heads, arms,
            // spider legs) are never clipped at the screen edge.
            let pad = 0.5;
            let w = half_width as f32 + pad;
            let aabb_min = Vec3::new(feet.x - w, feet.y - pad, feet.z - w);
            let aabb_max = Vec3::new(feet.x + w, feet.y + height as f32 + pad, feet.z + w);
            if !frustum.intersects_aabb(aabb_min, aabb_max) {
                continue;
            }
            let body_yaw = entity.render_yaw(tick_alpha);
            let (limb_swing, limb_swing_amount) = entity.render_limb_swing(tick_alpha);
            // Net head yaw is clamped so a stale head target never spins the
            // head past a natural turn (vanilla mobs clamp head rotation too).
            let net_head_yaw =
                wrap_degrees(entity.render_head_yaw(tick_alpha) - body_yaw).clamp(-75.0, 75.0);
            let anim = EntityAnim {
                limb_swing,
                limb_swing_amount,
                net_head_yaw,
                head_pitch: entity.render_pitch(tick_alpha),
                swing_progress: entity.render_swing(tick_alpha),
                sneaking: entity.sneaking,
                old_animations,
            };
            let start = mesh.vertices.len();
            // Invisible entities (metadata flag 0x20) render no body model — but
            // a player's worn armor still shows (vanilla renders armor/held items
            // on invisible players), and a visible custom-name plate is drawn by
            // player_nametags.
            if !entity.invisible {
                mesh.push_entity(entity.kind, feet, body_yaw, &anim, skin_row);
            }
            // Armor overlay for players with equipped armor (shown even when the
            // player is invisible).
            if matches!(entity.kind, EntityKind::RemotePlayer) {
                if let Some(slots) = self.entity_equipment.get(&entity.id) {
                    let ids: [Option<i16>; 5] = [
                        None, // slot 0 (held) is not armor
                        slots[1].as_ref().map(|s| s.id),
                        slots[2].as_ref().map(|s| s.id),
                        slots[3].as_ref().map(|s| s.id),
                        slots[4].as_ref().map(|s| s.id),
                    ];
                    if ids[1].is_some() || ids[2].is_some() || ids[3].is_some() || ids[4].is_some() {
                        mesh.push_armor(&ids, &anim, feet, body_yaw);
                    }
                }
            }
            // Light the entity by the world lightmap at its body centre (vanilla
            // samples the lightmap per entity), so mobs darken at night/in caves
            // instead of glowing full-bright. Folded into the per-face shade
            // already baked in the vertex colour.
            let center = Vec3::new(feet.x, feet.y + height as f32 * 0.5, feet.z);
            let factor = entity_light(&self.world, center, sun_b, brightness);
            // Vanilla damage flash (RendererLivingEntity.setBrightness): while
            // hurtTime > 0 the model colour is lerped toward pure red at a
            // constant 0.3 strength (GL_INTERPOLATE with (1,0,0,0.3)) — held for
            // the whole hurt animation, not faded, then snapped off.
            let hurt = entity.hurt_time > 0;
            for v in &mut mesh.vertices[start..] {
                v.color[0] *= factor;
                v.color[1] *= factor;
                v.color[2] *= factor;
                if hurt {
                    v.color[0] = v.color[0] * 0.7 + 0.3;
                    v.color[1] *= 0.7;
                    v.color[2] *= 0.7;
                }
            }
        }
    }

    /// Append the chest block-entities near the camera to the entity model.
    /// Chests no longer mesh as terrain (their `render_shape` is `None`), so the
    /// dedicated [`recraft_render::ModelMesh::push_chest`] model is drawn here in
    /// the same model pass as mobs, sampling the chest entity textures. Loaded
    /// chunks within `max_dist_sq` of the camera are scanned for chest ids
    /// (54 normal, 130 ender, 146 trapped); each chest's lid uses its eased
    /// open amount (see [`Self::tick_chest_lids`]) and is lit by the world
    /// lightmap like the surrounding terrain.
    pub fn build_chest_models(
        &self,
        mesh: &mut ModelMesh,
        brightness: f32,
        tick_alpha: f32,
        max_dist_sq: f64,
    ) {
        let sun_b = recraft_render::sky::sun_brightness(self.world_time(tick_alpha));
        let frustum = self.camera.frustum();
        let cam = self.camera.position;
        let max_chunk_dist = (max_dist_sq.sqrt() / 16.0).ceil() as i32 + 1;
        let cam_cx = (cam.x.floor() as i32).div_euclid(16);
        let cam_cz = (cam.z.floor() as i32).div_euclid(16);

        for chunk in self.world.chunks() {
            let cpos = chunk.position;
            if (cpos.x - cam_cx).abs() > max_chunk_dist || (cpos.z - cam_cz).abs() > max_chunk_dist {
                continue;
            }
            for section in chunk.sections() {
                let base_y = section.y() * 16;
                for ly in 0..16u8 {
                    for lz in 0..16u8 {
                        for lx in 0..16u8 {
                            let block = section.get(lx, ly, lz);
                            let kind = match block.id {
                                54 => ChestKind::Normal,
                                130 => ChestKind::Ender,
                                146 => ChestKind::Trapped,
                                _ => continue,
                            };
                            let wx = cpos.x * 16 + lx as i32;
                            let wy = base_y + ly as i32;
                            let wz = cpos.z * 16 + lz as i32;
                            // Cull by distance (cell centre) then frustum.
                            let cx = wx as f64 + 0.5;
                            let cy = wy as f64 + 0.5;
                            let cz = wz as f64 + 0.5;
                            let d = (cx - cam.x as f64).powi(2)
                                + (cy - cam.y as f64).powi(2)
                                + (cz - cam.z as f64).powi(2);
                            if d > max_dist_sq {
                                continue;
                            }
                            let min = Vec3::new(wx as f32, wy as f32, wz as f32);
                            let max = min + Vec3::ONE;
                            if !frustum.intersects_aabb(min, max) {
                                continue;
                            }
                            let lid = self
                                .chest_lid_angles
                                .get(&[wx, wy, wz])
                                .copied()
                                .unwrap_or(0.0);
                            let start = mesh.vertices.len();
                            mesh.push_chest([wx, wy, wz], block.meta, lid, kind);
                            // Light the chest by the lightmap at its centre, like mobs.
                            let factor = entity_light(
                                &self.world,
                                Vec3::new(cx as f32, cy as f32, cz as f32),
                                sun_b,
                                brightness,
                            );
                            for v in &mut mesh.vertices[start..] {
                                v.color[0] *= factor;
                                v.color[1] *= factor;
                                v.color[2] *= factor;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Ease each tracked chest lid toward its target (1 = open while viewers > 0,
    /// 0 = closed) at vanilla's 0.1/tick, pruning fully-closed entries. The open
    /// target comes from the S24 BlockAction viewer count; until that packet is
    /// decoded no chest opens, so this currently only animates entries a future
    /// BlockAction handler inserts. Run once per simulation tick from
    /// [`Self::advance_view_state`].
    fn tick_chest_lids(&mut self) {
        let targets = &self.chest_open_targets;
        self.chest_lid_angles.retain(|pos, angle| {
            let target = targets.get(pos).copied().unwrap_or(0.0);
            // Vanilla TileEntityChest eases the lid at 0.1/tick toward the target.
            *angle += (target - *angle) * 0.1;
            // Keep entries that are open, opening, or still closing.
            *angle > 0.001 || target > 0.0
        });
        // Drop closed targets so the maps don't grow unbounded.
        self.chest_open_targets.retain(|_, t| *t > 0.0);
    }

    /// World-light factor (0..1) for a model drawn at `pos`, matching the chunk
    /// shader's day/night + block-light + brightness-gamma so entities and the
    /// first-person hand sit at the same brightness as the terrain around them.
    pub fn world_light_factor(&self, pos: Vec3, brightness: f32, tick_alpha: f32) -> f32 {
        let sun_b = recraft_render::sky::sun_brightness(self.world_time(tick_alpha));
        entity_light(&self.world, pos, sun_b, brightness)
    }

    /// The dropped items to render this frame (object type 2 with a known
    /// stack), each with its interpolated world position and bob/spin phase.
    pub fn dropped_items(&self, tick_alpha: f32) -> Vec<DroppedItem> {
        let mut items = Vec::new();
        for entity in self.world.entities() {
            if entity.kind != EntityKind::Object(2) {
                continue;
            }
            let Some(item) = self.entity_items.get(&entity.id) else {
                continue;
            };
            let pos = to_render_vec3(entity.render_position(tick_alpha as f64));
            let phase = entity.age as f32 + tick_alpha + (entity.id.0 as f32) * 0.5;
            let (block_l, sky_l) = self.world.light_at(
                pos.x.floor() as i32,
                pos.y.floor() as i32,
                pos.z.floor() as i32,
            );
            let light = [sky_l as f32 / 15.0, block_l as f32 / 15.0];
            items.push(DroppedItem {
                item: item.clone(),
                pos,
                phase,
                light,
            });
        }
        items
    }

    /// This frame's particle billboards (interpolated by `tick_alpha`), for the
    /// renderer to build into camera-facing quads.
    pub fn particle_billboards(&self, tick_alpha: f32) -> Vec<recraft_render::ParticleBillboard> {
        self.particles.billboards(tick_alpha)
    }

    /// This frame's experience-orb billboards (vanilla `RenderXPOrb`): a
    /// camera-facing quad sampling `experience_orb.png`, the sprite cell chosen
    /// by the orb's xp value, colour-cycling through the green/red rainbow over
    /// its age, drawn at half alpha and full brightness.
    pub fn xp_orbs(&self, tick_alpha: f32) -> Vec<recraft_render::ParticleBillboard> {
        let mut orbs = Vec::new();
        for entity in self.world.entities() {
            if entity.kind != EntityKind::ExperienceOrb {
                continue;
            }
            let xp = self.entity_xp.get(&entity.id).copied().unwrap_or(0);
            let cell = xp_orb_texture_cell(xp);
            let pos = to_render_vec3(entity.render_position(tick_alpha as f64));
            // Vanilla RenderXPOrb colour cycle: f = (age + partial) / 2,
            // r = (sin(f)+1)*0.5, g = 1.0, b = (sin(f+4.1887903)+1)*0.1, α 0.5.
            let f = (entity.age as f32 + tick_alpha) / 2.0;
            let color = [
                (f.sin() + 1.0) * 0.5,
                1.0,
                ((f + 4.188_790_3).sin() + 1.0) * 0.1,
                0.5,
            ];
            orbs.push(recraft_render::ParticleBillboard {
                world_pos: (pos + Vec3::new(0.0, 0.25, 0.0)).into(),
                size: 0.25, // ~0.5 wide quad
                uv: xp_orb_cell_uv(cell),
                color,
            });
        }
        orbs
    }

    /// This frame's falling-block cubes (SpawnObject kind 70): each carries its
    /// blockstate, interpolated world position and lightmap sample; the renderer
    /// builds a full terrain-textured cube at that position.
    pub fn falling_block_cubes(&self, tick_alpha: f32) -> Vec<FallingBlock> {
        let mut cubes = Vec::new();
        for entity in self.world.entities() {
            let Some(block) = self.falling_blocks.get(&entity.id) else {
                continue;
            };
            let pos = to_render_vec3(entity.render_position(tick_alpha as f64));
            let (block_l, sky_l) =
                self.world
                    .light_at(pos.x.floor() as i32, pos.y.floor() as i32, pos.z.floor() as i32);
            cubes.push(FallingBlock {
                block: *block,
                pos,
                light: [sky_l as f32 / 15.0, block_l as f32 / 15.0],
            });
        }
        cubes
    }

    /// This frame's projectile sprites: SpawnObject kinds mapped to an item id
    /// and rendered as 2D item-sprite billboards through the dropped-item path.
    /// Arrows are emitted as a thin elongated billboard (a full 3D model is
    /// deferred). Kinds already handled elsewhere (item=2, falling block=70,
    /// armor stand=78) are skipped.
    pub fn projectiles(&self, tick_alpha: f32) -> Vec<DroppedItem> {
        let mut sprites = Vec::new();
        for entity in self.world.entities() {
            let EntityKind::Object(kind) = entity.kind else {
                continue;
            };
            let Some(item_id) = projectile_item_id(kind) else {
                continue;
            };
            let pos = to_render_vec3(entity.render_position(tick_alpha as f64));
            let (block_l, sky_l) =
                self.world
                    .light_at(pos.x.floor() as i32, pos.y.floor() as i32, pos.z.floor() as i32);
            sprites.push(DroppedItem {
                item: SlotItem::new(item_id, 1, 0),
                pos,
                // No bob/spin for projectiles: a fixed phase keeps the sprite
                // steady (build_world_items lifts it 0.25 + a small bob).
                phase: 0.0,
                light: [sky_l as f32 / 15.0, block_l as f32 / 15.0],
            });
        }
        sprites
    }

    /// Client-derived boss bar (vanilla `BossStatus`): the nearest living wither
    /// (SpawnMob type 64) or ender dragon (type 63) within range, returning its
    /// display name and 0..1 health fraction. 1.8 has no bossbar packet, so this
    /// is reconstructed from the entity's tracked health metadata.
    pub fn boss_bar(&self) -> Option<(String, f32)> {
        let eye = DVec3::new(
            self.camera.position.x as f64,
            self.camera.position.y as f64,
            self.camera.position.z as f64,
        );
        let mut best: Option<(f64, &EntityState, f32, &str)> = None;
        for entity in self.world.entities() {
            let (max_health, default_name) = match entity.kind {
                EntityKind::Mob(64) => (300.0, "Wither"),
                EntityKind::Mob(63) => (200.0, "Ender Dragon"),
                _ => continue,
            };
            let Some(health) = entity.health.filter(|h| *h > 0.0) else {
                continue;
            };
            // Vanilla tracks the boss within ~80 blocks of the view.
            let dist = entity.position.distance(eye);
            if dist > 80.0 {
                continue;
            }
            if best.as_ref().is_none_or(|(d, ..)| dist < *d) {
                best = Some((dist, entity, health / max_health, default_name));
            }
        }
        let (_, entity, fraction, default_name) = best?;
        let name = entity
            .custom_name
            .clone()
            .unwrap_or_else(|| default_name.to_string());
        Some((name, fraction.clamp(0.0, 1.0)))
    }

    /// Held items of remote players, each with the arm's world-space reference
    /// frame so the item renderer can orient the model correctly in the hand.
    pub fn player_held_items(&self, tick_alpha: f32, old_animations: bool) -> Vec<PlayerHeldItem> {
        let mut items = Vec::new();
        for entity in self.world.entities() {
            if entity.kind != EntityKind::RemotePlayer || entity.id == self.player.id {
                continue;
            }
            let Some(slots) = self.entity_equipment.get(&entity.id) else {
                continue;
            };
            let Some(ref item) = slots[0] else {
                continue;
            };
            let feet = to_render_vec3(entity.render_position(tick_alpha as f64));
            let body_yaw = entity.render_yaw(tick_alpha);
            let (limb_swing, limb_swing_amount) = entity.render_limb_swing(tick_alpha);
            let net_head_yaw =
                wrap_degrees(entity.render_head_yaw(tick_alpha) - body_yaw).clamp(-75.0, 75.0);
            let anim = EntityAnim {
                limb_swing,
                limb_swing_amount,
                net_head_yaw,
                head_pitch: entity.render_pitch(tick_alpha),
                swing_progress: entity.render_swing(tick_alpha),
                sneaking: entity.sneaking,
                old_animations,
            };
            let frame = held_item_frame(feet, body_yaw, &anim);
            let (block_l, sky_l) = self.world.light_at(
                frame.hand.x.floor() as i32,
                frame.hand.y.floor() as i32,
                frame.hand.z.floor() as i32,
            );
            let light = [sky_l as f32 / 15.0, block_l as f32 / 15.0];
            items.push(PlayerHeldItem { item: item.clone(), frame, light });
        }
        items
    }

    /// A cheap fingerprint of everything the per-frame entity model + first-person
    /// hand + nametags are built from: camera, day/night, and each entity's
    /// interpolated transform, animation, hurt flash, equipment and resolved skin.
    /// When it matches the previous frame the caller skips the whole rebuild + GPU
    /// upload and the renderer keeps last frame's mesh — the win on weak CPUs in
    /// crowded-but-idle scenes. Block-light changes around a stationary entity are
    /// deliberately not tracked (day/night, the dominant variation, is via
    /// `sun_brightness`); such an entity's tint just lags until it next moves.
    pub fn entity_render_fingerprint(
        &self,
        tick_alpha: f32,
        brightness: f32,
        hud_visible: bool,
        skin_rows: &std::collections::HashMap<[u8; 16], u32>,
        max_dist_sq: f64,
    ) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| h = (h ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        // Quantize positions (≈1/256 block) and angles/anim (≈1/64) so the float
        // residual of a "still" player/entity — friction decays velocity toward,
        // but never exactly to, zero — doesn't churn the key every frame, while
        // real motion still crosses many steps and rebuilds. The grid is far below
        // a pixel at any view distance, so a skipped sub-step is invisible.
        let qp = |v: f64| (v * 256.0).round() as i64 as u64;
        let qa = |v: f32| (v * 64.0).round() as i64 as u64;
        // Camera drives frustum culling, the hand pose and nametag projection.
        let c = &self.camera;
        mix(qp(c.position.x as f64));
        mix(qp(c.position.y as f64));
        mix(qp(c.position.z as f64));
        mix(qa(c.yaw));
        mix(qa(c.pitch));
        // Day/night + brightness gamma fold into every entity's lighting.
        let sun_b = recraft_render::sky::sun_brightness(self.world_time(tick_alpha));
        mix((sun_b * 1024.0) as u64);
        mix(brightness.to_bits() as u64);
        mix(hud_visible as u64);
        // First-person hand/held-item pose.
        let fp = self.first_person_view(tick_alpha);
        mix(fp
            .item
            .as_ref()
            .map(|s| (s.id as u16 as u64) << 24 | (s.count as u64) << 16 | s.damage as u16 as u64)
            .unwrap_or(0));
        mix(qa(fp.equip_progress));
        mix(qa(fp.swing_progress));
        mix(qa(fp.arm_lag_pitch));
        mix(qa(fp.arm_lag_yaw));
        mix(fp.use_action as u64);
        // `use_ticks` carries the partial tick every frame, but only drives the
        // pose while an item is actually in use — folding it in when idle would
        // churn the key forever (the active case rebuilds per frame, as intended).
        if fp.use_action != ItemUseAction::None {
            mix(qa(fp.use_ticks));
        }
        // Every renderable entity's interpolated state (matches build_entity_model's
        // skip rules so the fingerprint changes exactly when its output would).
        for e in self.world.entities() {
            if e.id == self.player.id
                || e.kind == EntityKind::Object(2)
                || e.kind == EntityKind::ExperienceOrb
            {
                continue;
            }
            // Same distance cull as build_entity_model: distant mobs are neither
            // built nor folded into the key, so they don't force a rebuild.
            if entity_dist_sq(e, &self.camera, tick_alpha) > max_dist_sq {
                continue;
            }
            mix(e.id.0 as u32 as u64);
            let p = e.render_position(tick_alpha as f64);
            mix(qp(p.x));
            mix(qp(p.y));
            mix(qp(p.z));
            mix(qa(e.render_yaw(tick_alpha)));
            mix(qa(e.render_head_yaw(tick_alpha)));
            mix(qa(e.render_pitch(tick_alpha)));
            let (ls, lsa) = e.render_limb_swing(tick_alpha);
            mix(qa(ls));
            mix(qa(lsa));
            mix(qa(e.render_swing(tick_alpha)));
            mix(e.hurt_time as u64);
            mix((e.sneaking as u64) << 2 | (e.invisible as u64) << 1 | e.custom_name_visible as u64);
            if let Some(name) = &e.custom_name {
                for b in name.as_bytes() {
                    mix(*b as u64);
                }
            }
            if let Some(slots) = self.entity_equipment.get(&e.id) {
                for slot in slots {
                    mix(slot
                        .as_ref()
                        .map(|s| (s.id as u16 as u64) << 16 | s.damage as u16 as u64)
                        .unwrap_or(0));
                }
            }
            let skin_row = (e.kind == EntityKind::RemotePlayer)
                .then(|| self.entity_uuids.get(&e.id))
                .flatten()
                .and_then(|uuid| skin_rows.get(uuid))
                .copied();
            mix(skin_row.map(|r| r as u64 + 1).unwrap_or(0));
            // A passenger renders at its vehicle's position; fold the vehicle id so
            // a vehicle swap re-triggers even if the passenger's own state is still.
            if let Some(v) = self.vehicles.get(&e.id) {
                mix(v.0 as u32 as u64);
            }
        }
        // Animating chest lids (order-independent so the HashMap iteration order
        // doesn't churn the key): each open chest folds its position + quantized
        // lid angle. Closed chests aren't tracked, so this stays empty in idle
        // scenes and only forces rebuilds while a lid is actually moving.
        let mut chest_acc: u64 = 0;
        for (&[x, y, z], &angle) in &self.chest_lid_angles {
            chest_acc ^= (x as u32 as u64)
                ^ ((z as u32 as u64) << 21)
                ^ ((y as u64) << 42)
                ^ ((angle * 64.0) as u64).wrapping_mul(0x9e37_79b9);
        }
        mix(chest_acc);
        h
    }

    /// Nametag labels to draw this frame: the decorated name and the world
    /// anchor above each entity's head. Remote players take their name from the
    /// tab roster and are hidden behind terrain; entities with a visible custom
    /// name (armor-stand floating text, named mobs) show it through walls, like
    /// vanilla's `alwaysRenderNameTag`. Filtered by view distance; screen
    /// projection and the behind-camera cull happen at the draw site.
    pub fn player_nametags(&self, tick_alpha: f32) -> Vec<(String, Vec3)> {
        let eye = DVec3::new(
            self.camera.position.x as f64,
            self.camera.position.y as f64,
            self.camera.position.z as f64,
        );
        let mut tags = Vec::new();
        for entity in self.world.entities() {
            if entity.id == self.player.id {
                continue;
            }
            let pos = entity.render_position(tick_alpha as f64);
            let (_, height) = entity.size();
            // Distance cutoff (vanilla renders names within ~64 blocks).
            let head = pos + DVec3::new(0.0, height + 0.5, 0.0);
            if head.distance(eye) > 64.0 {
                continue;
            }
            // A visible custom name is shown for any entity and renders through
            // walls (no occlusion check), matching vanilla floating text.
            if entity.custom_name_visible {
                if let Some(name) = &entity.custom_name {
                    tags.push((name.clone(), to_render_vec3(head)));
                    continue;
                }
            }
            // Otherwise only other players get a plate, from the tab roster, and
            // only when not occluded by terrain.
            if entity.kind != EntityKind::RemotePlayer {
                continue;
            }
            let Some(uuid) = self.entity_uuids.get(&entity.id) else {
                continue;
            };
            let Some(info) = self.player_list.get(uuid) else {
                continue;
            };
            // Coarse occlusion: hide the name if a block sits between eye and head.
            let to_head = head - eye;
            let dist = to_head.length();
            if let Some(hit) = raycast_block(&self.world, eye, to_head / dist, dist) {
                if hit.distance < dist - 0.3 {
                    continue;
                }
            }
            let name = match &info.display_name {
                Some(json) => chat::flatten_chat_json(json),
                None => self.scoreboard.decorate_entry(&info.name),
            };
            tags.push((name, to_render_vec3(head)));
        }
        tags
    }

    /// Swing progress interpolated between ticks (vanilla `getSwingProgress`
    /// with its wrap-around handling).
    pub fn swing_progress(&self, partial_ticks: f32) -> f32 {
        let mut delta = self.swing_progress - self.prev_swing_progress;
        if delta < 0.0 {
            delta += 1.0;
        }
        self.prev_swing_progress + delta * partial_ticks.clamp(0.0, 1.0)
    }

    /// Vanilla `ItemRenderer.updateEquippedItem`: the equip progress chases
    /// 1 while the rendered item matches the selection and 0 otherwise; the
    /// rendered item only swaps once the hand has dipped below 0.1.
    fn update_equipped_item(&mut self) {
        self.prev_equipped_progress = self.equipped_progress;
        let current = self.held_item().cloned();
        let raised =
            self.equipped_slot == self.selected_slot && current == self.rendered_item;
        let target = if raised { 1.0 } else { 0.0 };
        let delta = (target - self.equipped_progress).clamp(-0.4, 0.4);
        self.equipped_progress += delta;
        if self.equipped_progress < 0.1 {
            self.rendered_item = current;
            self.equipped_slot = self.selected_slot;
        }
    }

    /// Vanilla `EntityPlayerSP` renderArmPitch/Yaw: half-lerp toward the view
    /// rotation each tick (the hand sways behind quick turns).
    fn update_render_arm(&mut self) {
        self.prev_render_arm_pitch = self.render_arm_pitch;
        self.prev_render_arm_yaw = self.render_arm_yaw;
        self.render_arm_pitch += (self.player.pitch - self.render_arm_pitch) * 0.5;
        self.render_arm_yaw += (self.player.yaw - self.render_arm_yaw) * 0.5;
    }

    /// Snapshot driving the first-person hand/item rendering for one frame.
    pub fn first_person_view(&self, partial_ticks: f32) -> FirstPersonView {
        let partial = partial_ticks.clamp(0.0, 1.0);
        let equip = lerp(self.prev_equipped_progress, self.equipped_progress, partial);
        let arm_pitch = lerp(self.prev_render_arm_pitch, self.render_arm_pitch, partial);
        let arm_yaw = lerp(self.prev_render_arm_yaw, self.render_arm_yaw, partial);
        FirstPersonView {
            item: self.rendered_item.clone(),
            equip_progress: 1.0 - equip,
            swing_progress: self.swing_progress(partial),
            arm_lag_pitch: (self.player.pitch - arm_pitch) * 0.1,
            arm_lag_yaw: (self.player.yaw - arm_yaw) * 0.1,
            use_action: self.use_action,
            use_ticks: self.use_item_ticks as f32 + partial,
        }
    }

    /// The item in the selected hotbar slot, if any.
    pub fn held_item(&self) -> Option<&SlotItem> {
        self.inventory
            .get(36 + self.selected_slot.clamp(0, 8) as usize)
            .and_then(|s| s.as_ref())
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

    fn is_using_item(&self) -> bool {
        self.use_action != ItemUseAction::None
    }

    /// Vanilla `EntityPlayerSP.swingItem`: start the local swing animation and
    /// send the C0A animation packet.
    fn swing_item(&mut self, out: &mut Vec<ServerboundPacket>) {
        self.swing_arm();
        out.push(ServerboundPacket::SwingArm);
    }

    /// Vanilla `PlayerControllerMP.syncCurrentPlayItem`: send C09 HeldItemChange
    /// lazily, only when the selected slot differs from what the server was last
    /// told (called at the start of every action method).
    fn sync_current_play_item(&mut self, out: &mut Vec<ServerboundPacket>) {
        if self.selected_slot != self.current_player_item {
            self.current_player_item = self.selected_slot;
            out.push(ServerboundPacket::HeldItemChange {
                slot: self.selected_slot as i16,
            });
        }
    }

    /// Vanilla `Minecraft.clickMouse`: swing, then attack the targeted entity,
    /// start digging the targeted block, or (on a survival miss) arm the
    /// 10-tick left-click lockout.
    fn click_mouse(&mut self, out: &mut Vec<ServerboundPacket>) {
        if self.left_click_counter > 0 {
            return;
        }
        self.swing_item(out);
        match self.pick_target() {
            Some(InteractionTarget::Entity { id, .. }) => self.attack_entity(id, out),
            Some(InteractionTarget::Block { x, y, z, face, .. }) => {
                self.click_block(x, y, z, face, out)
            }
            None => {
                if !self.creative {
                    self.left_click_counter = 10;
                }
            }
        }
    }

    /// Vanilla `PlayerControllerMP.attackEntity` + `attackTargetEntityWithCurrentItem`:
    /// sync held item, send C02 ATTACK, and apply the sprint hit (halve
    /// horizontal motion + cancel sprint — the w-tap reset; no knockback enchant
    /// here so it fires exactly when sprinting).
    fn attack_entity(&mut self, id: i32, out: &mut Vec<ServerboundPacket>) {
        self.sync_current_play_item(out);
        out.push(ServerboundPacket::UseEntity {
            target: id,
            kind: UseEntityKind::Attack,
        });
        if self.sprinting {
            self.player.velocity.x *= 0.6;
            self.player.velocity.z *= 0.6;
            self.sprinting = false;
            self.sprint_reset_by_attack = true;
        }
    }

    /// Vanilla `PlayerControllerMP.clickBlock`: begin (or restart) a dig. In
    /// creative the block breaks immediately; in survival a START is sent (after
    /// aborting any previous dig on the NEW face) and the hit state is armed
    /// unless the block breaks instantly.
    fn click_block(&mut self, x: i32, y: i32, z: i32, face: u8, out: &mut Vec<ServerboundPacket>) {
        if self.creative {
            out.push(ServerboundPacket::PlayerDigging {
                status: DiggingStatus::StartDestroy,
                x,
                y,
                z,
                face,
            });
            // clickBlockCreative → onPlayerDestroyBlock (no packet); a held sword
            // is the one creative case that does NOT break the block.
            if !self.held_item().is_some_and(|it| is_sword(it.id)) {
                self.predict_break(x, y, z);
            }
            self.block_hit_delay = 5;
            return;
        }
        if !self.is_hitting_position(x, y, z) {
            if let Some(ref b) = self.breaking {
                // Abort the previous dig: vanilla sends the OLD block with the
                // NEW face here (resetBlockRemoving's face-DOWN is a separate path).
                out.push(ServerboundPacket::PlayerDigging {
                    status: DiggingStatus::CancelDestroy,
                    x: b.x,
                    y: b.y,
                    z: b.z,
                    face,
                });
            }
            out.push(ServerboundPacket::PlayerDigging {
                status: DiggingStatus::StartDestroy,
                x,
                y,
                z,
                face,
            });
            let ticks = block_break_ticks(self.world.block_at(x, y, z));
            if ticks <= 1.0 {
                // Instant break (hardness ~0): START alone destroys it.
                self.breaking = None;
                self.predict_break(x, y, z);
            } else {
                self.breaking = Some(BreakProgress {
                    x,
                    y,
                    z,
                    progress: 0.0,
                    per_tick: 1.0 / ticks,
                    item: self.held_item().cloned(),
                });
            }
        }
    }

    /// Vanilla `PlayerControllerMP.isHittingPosition`: the same cell AND the same
    /// held item as when the dig began (a hotbar switch fails this → restart).
    fn is_hitting_position(&self, x: i32, y: i32, z: i32) -> bool {
        self.breaking
            .as_ref()
            .is_some_and(|b| b.x == x && b.y == y && b.z == z && b.item.as_ref() == self.held_item())
    }

    /// Vanilla `PlayerControllerMP.onPlayerDamageBlock`: advance the held dig.
    /// Returns whether the dig is "live" this tick (so the caller swings).
    fn on_player_damage_block(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        face: u8,
        out: &mut Vec<ServerboundPacket>,
    ) -> bool {
        self.sync_current_play_item(out);
        if self.block_hit_delay > 0 {
            self.block_hit_delay -= 1;
            return true;
        }
        if self.creative {
            self.block_hit_delay = 5;
            out.push(ServerboundPacket::PlayerDigging {
                status: DiggingStatus::StartDestroy,
                x,
                y,
                z,
                face,
            });
            if !self.held_item().is_some_and(|it| is_sword(it.id)) {
                self.predict_break(x, y, z);
            }
            return true;
        }
        if self.is_hitting_position(x, y, z) {
            if self.world.block_at(x, y, z).is_air() {
                self.breaking = None;
                return false;
            }
            let done = {
                let b = self.breaking.as_mut().expect("hitting position");
                b.progress += b.per_tick;
                b.progress >= 1.0
            };
            if done {
                self.breaking = None;
                out.push(ServerboundPacket::PlayerDigging {
                    status: DiggingStatus::FinishDestroy,
                    x,
                    y,
                    z,
                    face,
                });
                self.predict_break(x, y, z);
                self.block_hit_delay = 5;
            }
            true
        } else {
            self.click_block(x, y, z, face, out);
            true
        }
    }

    /// Vanilla `PlayerControllerMP.resetBlockRemoving`: abort an in-progress dig
    /// with `EnumFacing.DOWN` (face 0).
    fn reset_block_removing(&mut self, out: &mut Vec<ServerboundPacket>) {
        if let Some(b) = self.breaking.take() {
            out.push(ServerboundPacket::PlayerDigging {
                status: DiggingStatus::CancelDestroy,
                x: b.x,
                y: b.y,
                z: b.z,
                face: 0,
            });
        }
    }

    /// Vanilla `Minecraft.sendClickBlockToController`: called every tick with
    /// whether the attack key is held. Advances the dig (and swings) while held
    /// over a block; otherwise resets the block-removing state.
    fn send_click_block_to_controller(
        &mut self,
        left_click: bool,
        out: &mut Vec<ServerboundPacket>,
    ) {
        if !left_click {
            self.left_click_counter = 0;
        }
        if self.left_click_counter <= 0 && !self.is_using_item() {
            let block = if left_click {
                match self.pick_target() {
                    Some(InteractionTarget::Block { x, y, z, face, .. })
                        if !self.world.block_at(x, y, z).is_air() =>
                    {
                        Some((x, y, z, face))
                    }
                    _ => None,
                }
            } else {
                None
            };
            if let Some((x, y, z, face)) = block {
                if self.on_player_damage_block(x, y, z, face, out) {
                    self.swing_item(out);
                }
            } else {
                self.reset_block_removing(out);
            }
        }
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

    /// Locally clear a just-broken block so the crosshair stops targeting it
    /// (prevents an immediate re-dig before the server's BlockChange arrives).
    fn predict_break(&mut self, x: i32, y: i32, z: i32) {
        let old = self.world.block_at(x, y, z);
        if self
            .world
            .set_block_if_chunk_loaded(x, y, z, BlockState::AIR)
        {
            self.mark_block_dirty_urgent(x, y, z);
            self.relight_after_edit(x, y, z, old);
            // On a real break (not air): a debris puff (vanilla
            // addBlockDestroyEffects) and the block's dig sound (vol 1, pitch 0.8).
            if !old.is_air() {
                self.particles.spawn_block_break(x, y, z);
                let pos = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                self.queue_sound(dig_sound_for_block(old.id), pos, 1.0, 0.8);
            }
        }
    }

    /// Offline/demo block-light update: flood-fill block light around an edit and
    /// queue every affected section for an urgent re-mesh. No-op in multiplayer.
    fn relight_after_edit(&mut self, x: i32, y: i32, z: i32, old: BlockState) {
        if !self.local_lighting {
            return;
        }
        let block_light = self.world.update_block_light(x, y, z, old);
        // Also recompute the column's sky-light so roofing over darkens the space
        // below (and digging a shaft lets daylight back in).
        let sky_light = self.world.update_sky_light(x, y, z, old);
        self.dirty_chunks.extend(block_light.iter().copied());
        self.dirty_chunks.extend(sky_light.iter().copied());
        self.urgent_remesh.extend(block_light);
        self.urgent_remesh.extend(sky_light);
    }

    /// Vanilla `ItemBlock.canPlaceBlockOnSide` → `World.canBlockBePlaced`:
    /// resolve the oriented state and target cell for a placement, returning
    /// `None` when it isn't allowed — we can't orient the block, the target cell
    /// isn't replaceable, or the placed block's collision box would intersect the
    /// player (`checkNoEntityCollision`, so you can't box yourself in at your
    /// feet). Used both to gate the C08 packet and to drive the local prediction.
    fn resolve_placement(
        &self,
        x: i32,
        y: i32,
        z: i32,
        face: u8,
        cursor_y: u8,
        item: &SlotItem,
    ) -> Option<(BlockState, i32, i32, i32)> {
        let state = placement_block_state(item, face, self.player.yaw, cursor_y)?;
        let (dx, dy, dz) = face_offset(face);
        let (px, py, pz) = (x + dx, y + dy, z + dz);
        if !is_replaceable(self.world.block_at(px, py, pz)) {
            return None;
        }
        let pa = self.player.aabb;
        for b in state.collision_boxes().as_slice() {
            let min = DVec3::new(
                px as f64 + b.min[0],
                py as f64 + b.min[1],
                pz as f64 + b.min[2],
            );
            let max = DVec3::new(
                px as f64 + b.max[0],
                py as f64 + b.max[1],
                pz as f64 + b.max[2],
            );
            if pa.max.x > min.x
                && pa.min.x < max.x
                && pa.max.y > min.y
                && pa.min.y < max.y
                && pa.max.z > min.z
                && pa.min.z < max.z
            {
                return None;
            }
        }
        Some((state, px, py, pz))
    }

    /// Mirror vanilla's client-side placement: the instant we send a block
    /// placement, set the block locally so it shows without waiting for — or
    /// depending on — a server BlockChange. Some servers never echo the player's
    /// own placement, so without this the block would never appear.
    ///
    /// Predicts only when the outcome is unambiguous: a real block item, a
    /// target that isn't being right-click-activated, a replaceable destination,
    /// and a state we can orient correctly (so we never leave a phantom block a
    /// no-echo server won't correct). Anything else just sends the packet and
    /// waits for the server.
    fn predict_placement(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        face: u8,
        cursor_y: u8,
        held: Option<&SlotItem>,
    ) -> bool {
        let Some(item) = held else { return false };
        let Some((state, px, py, pz)) = self.resolve_placement(x, y, z, face, cursor_y, item)
        else {
            return false;
        };
        let old = self.world.block_at(px, py, pz);
        if self.world.set_block_if_chunk_loaded(px, py, pz, state) {
            self.mark_block_dirty_urgent(px, py, pz);
            self.relight_after_edit(px, py, pz, old);
            // Vanilla plays the block's step sound on placement (the
            // `getPlaceSound` family; we reuse the dig event at pitch 0.8).
            let pos = Vec3::new(px as f32 + 0.5, py as f32 + 0.5, pz as f32 + 0.5);
            self.queue_sound(dig_sound_for_block(state.id), pos, 1.0, 0.8);
            true
        } else {
            false
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

    /// Vanilla `Minecraft.rightClickMouse`: gated by `!getIsHittingBlock()` (no
    /// place/use while a dig is open). Tries the targeted entity, then a block
    /// place/activate; if nothing consumes the click, uses the held item — for a
    /// sword that raises it to block.
    #[allow(clippy::collapsible_match)] // the inner `if` has side effects; a guard would be worse
    fn right_click_mouse(&mut self, out: &mut Vec<ServerboundPacket>) {
        if self.breaking.is_some() {
            return;
        }
        self.right_click_delay_timer = 4;
        let mut flag = true;
        match self.pick_target() {
            Some(InteractionTarget::Entity { id, cursor }) => {
                // isPlayerRightClickingOnEntity (InteractAt) then
                // interactWithEntitySendPacket (Interact); both return false for
                // a player target, so `flag` stays true and we fall through to
                // sendUseItem (a sword still blocks while aiming at a player).
                self.sync_current_play_item(out);
                out.push(ServerboundPacket::UseEntity {
                    target: id,
                    kind: UseEntityKind::InteractAt {
                        x: cursor[0],
                        y: cursor[1],
                        z: cursor[2],
                    },
                });
                self.sync_current_play_item(out);
                out.push(ServerboundPacket::UseEntity {
                    target: id,
                    kind: UseEntityKind::Interact,
                });
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
                if self.on_player_right_click(x, y, z, face, cursor_x, cursor_y, cursor_z, out) {
                    flag = false;
                    self.swing_item(out);
                }
            }
            None => {}
        }
        if flag && self.held_item().is_some() {
            self.send_use_item(out);
        }
    }

    /// Vanilla `PlayerControllerMP.onPlayerRightClick`: sync the held item, send
    /// the C08 block placement, and return whether the click was consumed —
    /// `true` if the block activated or a block item placed, `false` for a
    /// sword/tool/empty hand (the caller then falls through to sendUseItem).
    #[allow(clippy::too_many_arguments)]
    fn on_player_right_click(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        face: u8,
        cursor_x: u8,
        cursor_y: u8,
        cursor_z: u8,
        out: &mut Vec<ServerboundPacket>,
    ) -> bool {
        self.sync_current_play_item(out);
        let held = self.held_item().cloned();
        // onBlockActivated: an interactable block opens unless sneaking with an item.
        let activated =
            (!self.input.sneak || held.is_none()) && is_interactable(self.world.block_at(x, y, z));
        // Vanilla gates the block placement on `canPlaceBlockOnSide`: if a block
        // item can't actually be placed here (target not replaceable, or the block
        // would intersect the player), onPlayerRightClick returns *before* sending
        // C08 — the caller then falls through to sendUseItem. Without this gate we
        // place at our own feet and box ourselves in.
        if !activated {
            if let Some(item) = held.as_ref() {
                if is_block_item(item.id)
                    && self.resolve_placement(x, y, z, face, cursor_y, item).is_none()
                {
                    return false;
                }
            }
        }
        out.push(ServerboundPacket::PlayerBlockPlacement {
            x,
            y,
            z,
            face,
            held_item: held.as_ref().map(|it| HeldItem {
                id: it.id,
                count: it.count,
                damage: it.damage,
            }),
            cursor_x,
            cursor_y,
            cursor_z,
        });
        if activated {
            return true;
        }
        // onItemUse: a block item places (and is consumed); other items don't.
        match &held {
            Some(item) if is_block_item(item.id) => {
                self.predict_placement(x, y, z, face, cursor_y, held.as_ref())
            }
            _ => false,
        }
    }

    /// Vanilla `PlayerControllerMP.sendUseItem`: send the in-air C08 placement
    /// (position -1,-1,-1 / face 255 carrying the held stack) and enter the
    /// item's use action — a sword raises to block.
    fn send_use_item(&mut self, out: &mut Vec<ServerboundPacket>) {
        self.sync_current_play_item(out);
        let held = self.held_item();
        out.push(ServerboundPacket::PlayerBlockPlacement {
            x: -1,
            y: -1,
            z: -1,
            face: 255,
            held_item: held.map(|it| HeldItem {
                id: it.id,
                count: it.count,
                damage: it.damage,
            }),
            cursor_x: 0,
            cursor_y: 0,
            cursor_z: 0,
        });
        if let Some(it) = held {
            let action = item_use_action(it.id, it.damage);
            if action != ItemUseAction::None {
                self.use_action = action;
                self.use_item_ticks = 0;
            }
        }
    }

    /// Vanilla `PlayerControllerMP.onStoppedUsingItem`: send C07 RELEASE_USE_ITEM
    /// (after syncing the held item) and clear the use state (sword lowers).
    fn on_stopped_using_item(&mut self, out: &mut Vec<ServerboundPacket>) {
        self.sync_current_play_item(out);
        out.push(ServerboundPacket::PlayerDigging {
            status: DiggingStatus::ReleaseUseItem,
            x: 0,
            y: 0,
            z: 0,
            face: 0,
        });
        self.use_action = ItemUseAction::None;
        self.use_item_ticks = 0;
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
            ClientboundPlayPacket::UpdateHealth {
                health,
                food,
                food_saturation,
            } => {
                // Vanilla seeds the heart-row highlight from the local player's
                // hurtResistantTime, which the client does not simulate here. The
                // server's UpdateHealth is the change signal instead: on a health
                // change, blink the row (20 ticks on damage, 10 on heal) and record
                // the pre-change health drawn in the highlight frame (vanilla `j`).
                // The very first value is the spawn health, so it seeds no blink.
                // The hurt *sound* is played from the EntityStatus(2) path (vanilla
                // EntityPlayer.handleStatusUpdate), not from UpdateHealth.
                if self.health_received {
                    let old = self.health.ceil() as i32;
                    let new = health.ceil() as i32;
                    if let Some(window) = crate::gui::ingame::health_blink_window(old, new) {
                        self.last_player_health = old;
                        self.health_update_counter = (self.hud_update_counter + window) as i64;
                    }
                } else {
                    self.last_player_health = health.ceil() as i32;
                }
                self.health_received = true;
                self.health = health;
                self.food = food;
                self.saturation = food_saturation;
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
            ClientboundPlayPacket::SpawnParticle {
                particle_id,
                x,
                y,
                z,
                offset_x,
                offset_y,
                offset_z,
                speed,
                count,
                args,
            } => {
                self.particles.spawn(
                    particle_id,
                    Vec3::new(x, y, z),
                    Vec3::new(offset_x, offset_y, offset_z),
                    speed,
                    count,
                    &args,
                );
                false
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
                uuid,
                x,
                y,
                z,
                yaw,
                pitch,
            } => {
                self.spawn_remote_entity(entity_id, EntityKind::RemotePlayer, x, y, z, yaw, pitch);
                self.entity_uuids.insert(EntityId(entity_id), uuid);
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
                ..
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
                yaw,
                pitch,
                data,
                ..
            } => {
                self.spawn_remote_entity(
                    entity_id,
                    EntityKind::Object(kind as u8),
                    x,
                    y,
                    z,
                    yaw,
                    pitch,
                );
                // Falling block (kind 70): the blockstate is packed into the
                // data int (low 12 bits id, next 4 bits meta).
                if kind == 70 {
                    let id = (data & 0xfff) as u16;
                    let meta = ((data >> 12) & 0xf) as u8;
                    self.falling_blocks
                        .insert(EntityId(entity_id), BlockState::new(id, meta));
                }
                false
            }
            ClientboundPlayPacket::SpawnExperienceOrb {
                entity_id,
                x,
                y,
                z,
                count,
            } => {
                self.spawn_remote_entity(
                    entity_id,
                    EntityKind::ExperienceOrb,
                    x,
                    y,
                    z,
                    0.0,
                    0.0,
                );
                self.entity_xp.insert(EntityId(entity_id), count);
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
                    let target = entity.server_position;
                    entity.set_server_target(target, yaw, pitch);
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
                    entity.set_server_target(DVec3::new(x, y, z), yaw, pitch);
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
            ClientboundPlayPacket::OpenWindow {
                window_id,
                inventory_type,
                title,
                slots,
                ..
            } => {
                // Replace any prior window and ask the host to push the screen.
                let title = chat::flatten_chat_json(&title);
                self.open_container = Some(Container::open(
                    window_id,
                    &inventory_type,
                    title,
                    slots as usize,
                ));
                self.container_drag_cancel();
                self.window_open_pending = true;
                false
            }
            ClientboundPlayPacket::CloseWindowS { window_id } => {
                // Server force-close: drop the window and signal the host to pop
                // the screen (vanilla closes without echoing a C0D back).
                if self.open_container.as_ref().is_some_and(|c| c.window_id == window_id) {
                    self.open_container = None;
                    self.cursor_item = None;
                    self.container_drag_cancel();
                    self.window_close_pending = true;
                }
                false
            }
            ClientboundPlayPacket::WindowProperty {
                window_id,
                property,
                value,
            } => {
                if let Some(container) = self.open_container.as_mut() {
                    if container.window_id == window_id {
                        container.set_property(property, value);
                    }
                }
                false
            }
            ClientboundPlayPacket::SetSlot {
                window_id,
                slot,
                item,
            } => {
                // Window -1 slot -1 is the cursor stack (vanilla `setItemStack`).
                if window_id == -1 && slot == -1 {
                    self.cursor_item = item;
                } else if let Some(container) = self
                    .open_container
                    .as_mut()
                    .filter(|c| c.window_id as i8 == window_id)
                {
                    container.apply_set_slot(slot, item, &mut self.inventory);
                } else if window_id == 0 && (0..self.inventory.len() as i16).contains(&slot) {
                    // No matching window open: the server still syncs the player
                    // inventory directly (item pickups, hotbar updates, …).
                    self.inventory[slot as usize] = item;
                }
                false
            }
            ClientboundPlayPacket::WindowItems { window_id, items } => {
                if let Some(container) = self
                    .open_container
                    .as_mut()
                    .filter(|c| c.window_id == window_id)
                {
                    container.apply_window_items(items, &mut self.inventory);
                } else if window_id == 0 {
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
                    let id = EntityId(id);
                    self.world.remove_entity(id);
                    self.entity_uuids.remove(&id);
                    self.entity_items.remove(&id);
                    self.entity_xp.remove(&id);
                    self.falling_blocks.remove(&id);
                    self.entity_equipment.remove(&id);
                    // Drop both directions of any mount relationship.
                    self.vehicles.remove(&id);
                    self.vehicles.retain(|_, vehicle| *vehicle != id);
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
            ClientboundPlayPacket::EntityProperties {
                entity_id,
                properties,
            } => {
                // Only the local player's movement speed feeds the prediction;
                // other entities' attributes aren't modeled.
                if entity_id == self.player.id.0 {
                    for property in &properties {
                        if property.key == "generic.movementSpeed" {
                            self.walk_speed_attribute =
                                effective_attribute_value(property, &SPRINT_SPEED_BOOST_UUID)
                                    as f32;
                        }
                    }
                }
                false
            }
            ClientboundPlayPacket::ChatMessage { json, position } => {
                if position == 2 {
                    self.chat.set_action_bar(chat::flatten_chat_json(&json));
                } else {
                    let segments = chat::parse_chat_components(&json);
                    let text: String = segments.iter().map(|s| s.text.as_str()).collect();
                    log::info!("[chat] {}", chat::strip_legacy_codes(&text));
                    self.chat.push_components(segments);
                }
                false
            }
            ClientboundPlayPacket::TabComplete { matches } => {
                self.chat.set_completions(matches);
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
            ClientboundPlayPacket::Title { action } => {
                self.title.apply(action);
                false
            }
            ClientboundPlayPacket::Disconnect { reason_json } => {
                log::warn!("server disconnected: {reason_json}");
                false
            }
            ClientboundPlayPacket::PlayerListItem { entries } => {
                self.player_list.apply(entries);
                false
            }
            ClientboundPlayPacket::PlayerListHeaderFooter { header, footer } => {
                self.player_list.set_header_footer(
                    chat::flatten_chat_json(&header),
                    chat::flatten_chat_json(&footer),
                );
                false
            }
            ClientboundPlayPacket::EntityHeadLook {
                entity_id,
                head_yaw,
            } => {
                if let Some(entity) = self.remote_entity_mut(entity_id) {
                    entity.set_head_yaw(head_yaw);
                }
                false
            }
            ClientboundPlayPacket::AttachEntity {
                entity_id,
                vehicle_id,
                leash,
            } => {
                // Leashes (leash = true) don't move the entity; only rides do.
                if !leash {
                    if vehicle_id == -1 {
                        self.vehicles.remove(&EntityId(entity_id));
                    } else {
                        self.vehicles
                            .insert(EntityId(entity_id), EntityId(vehicle_id));
                    }
                }
                false
            }
            ClientboundPlayPacket::EntityAnimation {
                entity_id,
                animation,
            } => {
                match animation {
                    0 => {
                        if let Some(entity) = self.remote_entity_mut(entity_id) {
                            entity.start_swing();
                        }
                    }
                    1 => {
                        if let Some(entity) = self.remote_entity_mut(entity_id) {
                            entity.start_hurt();
                        }
                    }
                    _ => {}
                }
                false
            }
            ClientboundPlayPacket::EntityStatus {
                entity_id,
                status,
            } => {
                if status == 2 {
                    if let Some(entity) = self.world.entity_mut(EntityId(entity_id)) {
                        entity.start_hurt();
                    }
                    // The local player's own hurt animation also plays the hurt
                    // sound (vanilla EntityPlayer.handleStatusUpdate → playHurtSound).
                    // UpdateHealth covers damage that changes health, but EntityStatus
                    // fires on every hit (incl. absorbed/blocked), so play it here too.
                    if EntityId(entity_id) == self.player.id {
                        let pos = self.camera.position;
                        self.queue_sound("game.player.hurt", pos, 1.0, 1.0);
                    }
                }
                false
            }
            ClientboundPlayPacket::EntityMetadata {
                entity_id,
                metadata,
            } => {
                self.apply_entity_metadata(entity_id, &metadata);
                false
            }
            ClientboundPlayPacket::TimeUpdate { time_of_day, .. } => {
                // A negative time means a fixed sky (doDaylightCycle off); its
                // magnitude is the actual time of day.
                self.daylight_cycle = time_of_day >= 0;
                self.world_time = time_of_day.abs();
                false
            }
            ClientboundPlayPacket::EntityEquipment {
                entity_id,
                slot,
                item,
            } => {
                if (0..5).contains(&slot) {
                    let slots = self
                        .entity_equipment
                        .entry(EntityId(entity_id))
                        .or_insert_with(Default::default);
                    slots[slot as usize] = item;
                }
                false
            }
            ClientboundPlayPacket::CollectItem { .. } => false,
            ClientboundPlayPacket::SoundEffect {
                name,
                x,
                y,
                z,
                volume,
                pitch,
            } => {
                let pos = Vec3::new(x as f32, y as f32, z as f32);
                let rate = pitch as f32 / 63.5;
                self.queue_sound(name, pos, volume, rate);
                false
            }
            ClientboundPlayPacket::Effect {
                effect_id,
                x,
                y,
                z,
                data,
                ..
            } => {
                if let Some(event) = effect_event(effect_id, data) {
                    let pos = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                    self.queue_sound(event, pos, 1.0, 1.0);
                }
                false
            }
            ClientboundPlayPacket::BlockAction {
                x,
                y,
                z,
                action_id,
                action_param,
                block_type,
            } => {
                // Note block (block id 25): play the pitched note and puff a
                // coloured NOTE particle. Chest/piston actions (other block
                // types) carry no client-side sound/particle here and are
                // ignored (T17 will extend this).
                if block_type == 25 {
                    let note = action_param.min(24);
                    let pitch = 2.0_f32.powf((note as f32 - 12.0) / 12.0);
                    let instrument = NOTE_INSTRUMENTS
                        .get(action_id as usize)
                        .copied()
                        .unwrap_or("harp");
                    let center = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                    self.queue_sound(format!("note.{instrument}"), center, 3.0, pitch);
                    // The NOTE branch reads the rainbow colour from offset.x.
                    let above = Vec3::new(x as f32 + 0.5, y as f32 + 1.2, z as f32 + 0.5);
                    self.particles
                        .spawn(23, above, Vec3::new(note as f32 / 24.0, 0.0, 0.0), 0.0, 1, &[]);
                } else if matches!(block_type, 54 | 130 | 146) && action_id == 1 {
                    // Chest/ender/trapped lid: action_param is the viewer count.
                    // Drive the lid-open target and play the open/close sound on
                    // the 0↔viewers transition (vanilla random.chestopen/closed).
                    let pos = [x, y, z];
                    let was_open = self.chest_open_targets.get(&pos).copied().unwrap_or(0.0) > 0.0;
                    let now_open = action_param > 0;
                    if now_open {
                        self.chest_open_targets.insert(pos, 1.0);
                        self.chest_lid_angles.entry(pos).or_insert(0.0);
                    } else {
                        self.chest_open_targets.insert(pos, 0.0);
                    }
                    if now_open != was_open {
                        let center = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                        let event = if now_open { "random.chestopen" } else { "random.chestclosed" };
                        self.queue_sound(event.to_string(), center, 0.5, 1.0);
                    }
                }
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

    /// Apply an EntityMetadata update to a tracked entity. Index 0 is the shared
    /// entity flags byte (0x02 = crouching → sneak pose, 0x20 = invisible);
    /// index 2/3 are the custom name and its always-visible flag (armor-stand
    /// floating text, named mobs); index 10 is the dropped-item ItemStack.
    fn apply_entity_metadata(
        &mut self,
        entity_id: i32,
        metadata: &[recraft_protocol::v1_8_9::packets::MetadataEntry],
    ) {
        for entry in metadata {
            match (entry.index, &entry.value) {
                (0, MetadataValue::Byte(flags)) => {
                    let on_fire = flags & 0x01 != 0;
                    let sneaking = flags & 0x02 != 0;
                    let invisible = flags & 0x20 != 0;
                    if entity_id == self.player.id.0 {
                        self.player.on_fire = on_fire;
                    }
                    if let Some(entity) = self.remote_entity_mut(entity_id) {
                        entity.sneaking = sneaking;
                        entity.on_fire = on_fire;
                        entity.invisible = invisible;
                    }
                }
                (2, MetadataValue::Str(name)) => {
                    if let Some(entity) = self.remote_entity_mut(entity_id) {
                        entity.custom_name = (!name.is_empty()).then(|| name.clone());
                    }
                }
                (3, MetadataValue::Byte(visible)) => {
                    if let Some(entity) = self.remote_entity_mut(entity_id) {
                        entity.custom_name_visible = *visible != 0;
                    }
                }
                (6, MetadataValue::Float(health)) => {
                    // Living-entity health (drives the client-derived boss bar).
                    if let Some(entity) = self.remote_entity_mut(entity_id) {
                        entity.health = Some(*health);
                    }
                }
                (10, MetadataValue::Slot(Some(item))) => {
                    self.entity_items.insert(EntityId(entity_id), item.clone());
                }
                _ => {}
            }
        }
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
            // Vanilla accumulates relative moves on the server-side position
            // (serverPosX) and lerps toward it over 3 ticks — never snapping
            // the rendered position to the packet.
            let target = entity.server_position + DVec3::new(dx, dy, dz);
            let (yaw, pitch) = look.unwrap_or((entity.server_yaw, entity.server_pitch));
            entity.set_server_target(target, yaw, pitch);
        }
    }

    /// Drain up to `max` dirty sections, nearest to the player first, leaving the
    /// rest queued for following frames. Bounds the per-frame mesh-rebuild cost
    /// so a join-time burst of chunks doesn't stall rendering.
    pub fn take_dirty_chunks_budget(&mut self, max: usize) -> Vec<SectionPos> {
        if max == 0 || self.dirty_chunks.is_empty() {
            return Vec::new();
        }
        if self.dirty_chunks.len() <= max {
            return self.dirty_chunks.drain().collect();
        }
        let pcx = (self.player.position.x.floor() as i32).div_euclid(16);
        let pcy = (self.player.position.y.floor() as i32).div_euclid(16);
        let pcz = (self.player.position.z.floor() as i32).div_euclid(16);
        let mut all: Vec<SectionPos> = self.dirty_chunks.iter().copied().collect();
        all.sort_by_key(|p| {
            let dx = (p.x - pcx) as i64;
            let dy = (p.y - pcy) as i64;
            let dz = (p.z - pcz) as i64;
            dx * dx + dy * dy + dz * dz
        });
        all.truncate(max);
        for p in &all {
            self.dirty_chunks.remove(p);
        }
        all
    }

    /// Mark every loaded section dirty so the whole world re-meshes — used when a
    /// setting that changes geometry (e.g. Fast/Fancy leaves) is toggled. The
    /// per-frame dirty budget spreads the rebuild over a few frames, so the toggle
    /// doesn't stall; until each section is rebuilt it keeps its old geometry.
    pub fn mark_all_sections_dirty(&mut self) {
        let sections: Vec<SectionPos> = self
            .world
            .chunks()
            .flat_map(|chunk| {
                let pos = chunk.position;
                chunk
                    .sections()
                    .map(move |section| SectionPos::new(pos.x, section.y(), pos.z))
            })
            .collect();
        self.dirty_chunks.extend(sections);
    }

    /// Drain locally predicted sections so the renderer can queue them before
    /// ordinary dirty sections. They are removed from the regular queue to avoid
    /// submitting duplicate mesh jobs in the same frame.
    pub fn take_urgent_remesh(&mut self) -> Vec<SectionPos> {
        let chunks: Vec<_> = self.urgent_remesh.drain().collect();
        for pos in &chunks {
            self.dirty_chunks.remove(pos);
        }
        chunks
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
        self.mark_block_dirty(x, y, z);
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

    /// Mark a whole column (and its four horizontal neighbours) dirty at section
    /// granularity, used when a column is loaded or unloaded. A loaded column
    /// marks only its non-empty sections; an unloaded one marks all 16 so their
    /// GPU meshes get dropped.
    fn mark_chunk_dirty(&mut self, pos: ChunkPos) {
        for col in [
            pos,
            ChunkPos::new(pos.x - 1, pos.z),
            ChunkPos::new(pos.x + 1, pos.z),
            ChunkPos::new(pos.x, pos.z - 1),
            ChunkPos::new(pos.x, pos.z + 1),
        ] {
            let section_ys: Vec<i32> = match self.world.chunk(col) {
                Some(chunk) => chunk.sections().map(|section| section.y()).collect(),
                None => (0..16).collect(),
            };
            for y in section_ys {
                self.dirty_chunks.insert(SectionPos::new(col.x, y, col.z));
            }
        }
    }

    fn mark_block_dirty(&mut self, x: i32, y: i32, z: i32) {
        self.dirty_chunks.extend(Self::block_dirty_sections(x, y, z));
    }

    fn mark_block_dirty_urgent(&mut self, x: i32, y: i32, z: i32) {
        let sections = Self::block_dirty_sections(x, y, z);
        self.dirty_chunks.extend(sections.iter().copied());
        self.urgent_remesh.extend(sections);
    }

    /// The sections a single block edit dirties: the block's own section plus the
    /// neighbour section across any chunk/section face it touches (so border-face
    /// culling and cross-border smooth lighting stay correct). Out-of-range Y
    /// edits touch nothing.
    fn block_dirty_sections(x: i32, y: i32, z: i32) -> Vec<SectionPos> {
        let sy = y.div_euclid(16);
        if !(0..16).contains(&sy) {
            return Vec::new();
        }
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let ly = y.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut sections = vec![SectionPos::new(cx, sy, cz)];
        if lx == 0 {
            sections.push(SectionPos::new(cx - 1, sy, cz));
        } else if lx == 15 {
            sections.push(SectionPos::new(cx + 1, sy, cz));
        }
        if lz == 0 {
            sections.push(SectionPos::new(cx, sy, cz - 1));
        } else if lz == 15 {
            sections.push(SectionPos::new(cx, sy, cz + 1));
        }
        if ly == 0 && sy > 0 {
            sections.push(SectionPos::new(cx, sy - 1, cz));
        } else if ly == 15 && sy < 15 {
            sections.push(SectionPos::new(cx, sy + 1, cz));
        }
        sections
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

/// Map a hardcoded S28 Effect id (and its blockstate `data` for 2001) to a
/// `sounds.json` event, for the handful of common effects worth playing. `None`
/// for the many particle-only / unsupported effects.
/// Note-block instrument sound suffixes by S24 BlockAction `action_id`
/// (vanilla `BlockNote` order). The block under the note block selects this:
/// 0 harp, 1 bass drum, 2 snare, 3 click/hat, 4 bass attack.
const NOTE_INSTRUMENTS: [&str; 5] = ["harp", "bd", "snare", "hat", "bassattack"];

/// The `experience_orb.png` sprite cell for an orb worth `xp` experience
/// (vanilla `RenderXPOrb.getTextureByXP`): higher-value orbs use larger,
/// brighter cells. The sheet is 16px cells, 4 per row.
fn xp_orb_texture_cell(xp: i16) -> u32 {
    match xp {
        x if x >= 2477 => 10,
        x if x >= 1237 => 9,
        x if x >= 617 => 8,
        x if x >= 307 => 7,
        x if x >= 149 => 6,
        x if x >= 73 => 5,
        x if x >= 37 => 4,
        x if x >= 17 => 3,
        x if x >= 7 => 2,
        x if x >= 3 => 1,
        _ => 0,
    }
}

/// Corner UVs into the 64×64 `experience_orb.png` for sprite `cell` (16px cells,
/// 4 per row), in the `ParticleBillboard` corner order: bottom-right, top-right,
/// top-left, bottom-left.
fn xp_orb_cell_uv(cell: u32) -> [[f32; 2]; 4] {
    let col = (cell % 4) as f32;
    let row = (cell / 4) as f32;
    let u0 = col * 16.0 / 64.0;
    let u1 = u0 + 16.0 / 64.0;
    let v0 = row * 16.0 / 64.0;
    let v1 = v0 + 16.0 / 64.0;
    [[u1, v1], [u1, v0], [u0, v0], [u0, v1]]
}

/// The item id whose sprite stands in for a SpawnObject projectile `kind`
/// (vanilla projectile render textures), or `None` for kinds rendered
/// elsewhere or not handled. Arrow (60) maps to the arrow item (262).
fn projectile_item_id(kind: u8) -> Option<i16> {
    Some(match kind {
        60 => 262, // arrow (rendered as a thin sprite; 3D model deferred)
        61 => 332, // snowball
        62 => 344, // egg
        64 => 385, // small fireball / fire charge
        65 => 368, // ender pearl
        72 => 381, // eye of ender
        73 => 373, // splash potion
        75 => 384, // bottle o' enchanting
        76 => 401, // firework rocket
        _ => return None,
    })
}

fn effect_event(effect_id: i32, data: i32) -> Option<String> {
    Some(
        match effect_id {
            1000 => "random.click",
            1001 => "random.click",
            1002 => "random.bow",
            1003 => "random.door_open",
            1006 => "random.door_open", // wooden door toggle
            1004 => "random.fizz",
            1009 => "random.fizz", // fire extinguish
            // 2001: block break — the low byte of `data` is the broken block id.
            2001 => return Some(dig_sound_for_block((data & 0xff) as u16)),
            _ => return None,
        }
        .to_string(),
    )
}

/// Vanilla `Block.stepSound` → the `dig.<material>` event for a block id. Only
/// the common materials are mapped; anything else falls back to `dig.stone`.
fn dig_sound_for_block(id: u16) -> String {
    let material = match id {
        // wood: planks, logs, fences, doors, stairs, chests, crafting table…
        5 | 17 | 47 | 53 | 54 | 58 | 63 | 64 | 65 | 85 | 96 | 107 | 134 | 135 | 136 | 146 | 162
        | 163 | 164 | 183..=187 => "wood",
        // gravel
        13 => "gravel",
        // sand
        12 | 24 => "sand",
        // grass / dirt / farmland / leaves / mycelium
        2 | 3 | 18 | 31 | 60 | 110 | 161 => "grass",
        // glass / glowstone / ice
        20 | 79 | 89 | 95 | 102 => "glass",
        // wool / carpet
        35 | 171 => "cloth",
        _ => "stone",
    };
    format!("dig.{material}")
}

/// Flat world-light factor (0..1) at a world position: vanilla lightmap combine
/// (day/night-scaled skylight vs. block light) with a small floor, run through
/// the same brightness-gamma the chunk shader uses. Keeps models in step with
/// the terrain's brightness, including the Brightness option and time of day.
/// Squared distance from the camera eye to an entity's interpolated position,
/// for the per-frame entity distance cull (shared by the build and its cache key).
fn entity_dist_sq(entity: &EntityState, camera: &Camera, tick_alpha: f32) -> f64 {
    let p = entity.render_position(tick_alpha as f64);
    let dx = p.x - camera.position.x as f64;
    let dy = p.y - camera.position.y as f64;
    let dz = p.z - camera.position.z as f64;
    dx * dx + dy * dy + dz * dz
}

fn entity_light(world: &World, pos: Vec3, sun_brightness: f32, brightness: f32) -> f32 {
    let (block_l, sky_l) = world.light_at(
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    );
    let level = (sky_l as f32 / 15.0 * sun_brightness)
        .max(block_l as f32 / 15.0)
        .max(0.05);
    let gamma = 1.0 + (1.0 - brightness) * 1.5;
    level.powf(gamma)
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

/// The neighbour cell a placement lands in, for a clicked face (vanilla
/// EnumFacing offsets: 0=down,1=up,2=north,3=south,4=west,5=east).
fn face_offset(face: u8) -> (i32, i32, i32) {
    match face {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        _ => (1, 0, 0),
    }
}

/// Cells a block may be placed into (vanilla `Material.isReplaceable`): air,
/// liquids, and the small plants/fire/snow that placement overwrites.
fn is_replaceable(block: BlockState) -> bool {
    block.is_air()
        || block.is_water()
        || block.is_lava()
        || matches!(block.id, 31 | 32 | 37 | 38 | 39 | 40 | 51 | 78 | 106 | 175)
}

/// Blocks whose right-click activates them (so a non-sneaking placement is
/// suppressed): containers, doors/trapdoors/gates, redstone toggles, and
/// workstations.
fn is_interactable(block: BlockState) -> bool {
    matches!(
        block.id,
        23 | 25 | 26 | 54 | 58 | 61 | 62 | 64 | 69 | 71 | 77 | 84 | 92 | 93 | 94 | 96 | 107 | 116
            | 117 | 130 | 137 | 138 | 143 | 145 | 146 | 149 | 150 | 154 | 158 | 167
            | 183..=187
            | 193..=197
    )
}

/// The block state vanilla `onBlockPlaced` would yield for a held block item, or
/// None when the placement isn't safely predictable — a non-block item (tools,
/// food, or doors/beds whose item id differs from the block id), or an oriented
/// block whose placement context we don't model (torches, rails, …), which then
/// waits for the server rather than risk a wrong phantom.
fn placement_block_state(item: &SlotItem, face: u8, yaw: f32, cursor_y: u8) -> Option<BlockState> {
    let raw = item.id;
    if !(1..=255).contains(&raw) {
        return None;
    }
    let id = raw as u16;
    let dmg = item.damage as u8;
    // Vanilla half rule (slabs/stairs): top when clicked on the underside, or on
    // a side above the cursor midline (hitY > 0.5 ⟺ cursor_y > 8).
    let upper_half = face == 0 || (face != 1 && cursor_y > 8);

    if matches!(id, 44 | 126 | 182) {
        // Slab: variant in the low bits, top half in bit 8.
        return Some(BlockState::new(
            id,
            (dmg & 7) | if upper_half { 8 } else { 0 },
        ));
    }
    if matches!(id, 17 | 162 | 170) {
        // Rotated pillar (log / hay): axis from the clicked face.
        let axis = match face {
            0 | 1 => 0,
            2 | 3 => 8,
            _ => 4,
        };
        return Some(BlockState::new(id, (dmg & 3) | axis));
    }
    if is_stairs(id) {
        // Facing from the player; half from the cursor.
        return Some(BlockState::new(
            id,
            stair_facing_meta(yaw) | if upper_half { 4 } else { 0 },
        ));
    }

    // Blocks whose meta carries no placement orientation: full cubes, plus the
    // auto-connecting fence/pane/wall family (which mesh from their neighbours),
    // all render correctly straight from meta = damage.
    let block = BlockState::new(id, dmg);
    if block.render_shape() == RenderShape::Cube || is_fence(id) || is_pane(id) || id == 139 {
        return Some(block);
    }
    None
}

/// Vanilla stairs `meta & 3` for the player's horizontal facing
/// (`getHorizontalFacing` → E=0, W=1, S=2, N=3).
fn stair_facing_meta(yaw: f32) -> u8 {
    // getHorizontal index: 0=S, 1=W, 2=N, 3=E.
    let idx = (((yaw as f64) * 4.0 / 360.0 + 0.5).floor() as i32) & 3;
    match idx {
        0 => 2,
        1 => 1,
        2 => 3,
        _ => 0,
    }
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

/// Simple deterministic hash for procedural generation (no std rand dependency).
fn hash2d(x: i32, z: i32, seed: u32) -> u32 {
    let mut h = seed;
    h ^= x as u32;
    h = h.wrapping_mul(0x45d9f3b).wrapping_add(0x16f11fe5);
    h ^= z as u32;
    h = h.wrapping_mul(0x45d9f3b).wrapping_add(0x16f11fe5);
    h ^ (h >> 16)
}

/// Terrain height at (x, z) using overlapping sine waves.
fn terrain_height(x: i32, z: i32) -> i32 {
    let fx = x as f64;
    let fz = z as f64;
    let h = 62.0
        + 8.0 * (fx * 0.02).sin() * (fz * 0.03).cos()
        + 4.0 * (fx * 0.07 + 1.0).sin() * (fz * 0.05 + 2.0).cos()
        + 2.0 * (fx * 0.15 + 3.0).cos() * (fz * 0.12 + 1.0).sin();
    h as i32
}

/// Place a simple oak tree at (x, y, z).
fn place_tree(world: &mut World, x: i32, y: i32, z: i32) {
    let trunk = BlockState::new(17, 0);
    let leaves = BlockState::new(18, 0);
    let h = 4 + (hash2d(x, z, 777) % 3) as i32;
    for dy in 0..h {
        world.set_block(x, y + dy, z, trunk);
    }
    let top = y + h;
    for dx in -2..=2 {
        for dz in -2..=2 {
            for dy in -2..=1 {
                let dist = dx * dx + dy * dy + dz * dz;
                if dist <= 6 && !(dx == 0 && dz == 0 && dy < 0) {
                    let bx = x + dx;
                    let by = top + dy;
                    let bz = z + dz;
                    if world.block_at(bx, by, bz).is_air() {
                        world.set_block(bx, by, bz, leaves);
                    }
                }
            }
        }
    }
}

/// Landscape demo: hilly terrain, trees, a lake, scattered ores, animals.
/// Returns the spawn position.
fn build_demo_landscape(world: &mut World) -> DVec3 {
    let water_level = 60;
    let range = 8; // chunks -8..8 (17×17)

    for cx in -range..=range {
        for cz in -range..=range {
            for lx in 0..16 {
                for lz in 0..16 {
                    let x = cx * 16 + lx;
                    let z = cz * 16 + lz;
                    let h = terrain_height(x, z);
                    let surface = h.max(water_level);

                    // Bedrock
                    world.set_block(x, 0, z, BlockState::new(7, 0));

                    // Stone fill
                    for y in 1..h.saturating_sub(3) {
                        // Scatter ores
                        let r = hash2d(x, y * 37 + z, 42);
                        let block = if r % 80 == 0 {
                            BlockState::new(16, 0) // coal
                        } else if r % 120 == 0 {
                            BlockState::new(15, 0) // iron
                        } else {
                            BlockState::STONE
                        };
                        world.set_block(x, y, z, block);
                    }

                    // Dirt / sand layers
                    let near_water = h <= water_level + 2;
                    let fill = if near_water {
                        BlockState::new(12, 0) // sand
                    } else {
                        BlockState::DIRT
                    };
                    for y in h.saturating_sub(3).max(1)..h {
                        world.set_block(x, y, z, fill);
                    }

                    // Surface
                    if h > water_level {
                        let top = if near_water { fill } else { BlockState::GRASS };
                        world.set_block(x, h, z, top);
                    } else {
                        // Underwater: dirt/sand on bottom, water above
                        world.set_block(x, h, z, fill);
                        for y in (h + 1)..=water_level {
                            world.set_block(x, y, z, BlockState::new(9, 0)); // still water
                        }
                    }

                    // Sky light: full above surface
                    for y in (surface + 1)..=(surface + 16) {
                        world.set_light(x, y, z, 0, 15);
                    }
                    world.set_light(x, surface, z, 0, 15);
                }
            }
        }
    }

    // Trees on grass blocks
    let tree_range = range * 16;
    for cx in -range..=range {
        for cz in -range..=range {
            // ~2 trees per chunk
            for seed in [111u32, 222] {
                let r = hash2d(cx, cz, seed);
                let lx = (r % 14 + 1) as i32;
                let lz = ((r >> 8) % 14 + 1) as i32;
                let x = cx * 16 + lx;
                let z = cz * 16 + lz;
                if x.abs() > tree_range || z.abs() > tree_range {
                    continue;
                }
                let h = terrain_height(x, z);
                if h > water_level + 2 {
                    place_tree(world, x, h + 1, z);
                }
            }
        }
    }

    // Animals
    let mob_types = [90u8, 91, 92, 93]; // pig, sheep, cow, chicken
    let mut eid = 100;
    for i in 0..24 {
        let r = hash2d(i, i * 7 + 3, 555);
        let x = ((r % 80) as i32 - 40) as f64 + 0.5;
        let z = (((r >> 8) % 80) as i32 - 40) as f64 + 0.5;
        let h = terrain_height(x as i32, z as i32);
        if h > water_level {
            let kind = EntityKind::Mob(mob_types[i as usize % mob_types.len()]);
            world.upsert_entity(EntityState::new_remote(
                EntityId(eid),
                kind,
                DVec3::new(x, h as f64 + 1.0, z),
                (r % 360) as f32,
                0.0,
            ));
            eid += 1;
        }
    }

    let spawn_h = terrain_height(0, 0).max(water_level) + 1;
    DVec3::new(0.5, spawn_h as f64 + 1.0, 0.5)
}

/// Chunk stress test: large area filled with geometry-heavy patterns to
/// maximize visible faces, draw calls, and triangle count.
fn build_demo_chunk_stress(world: &mut World) -> DVec3 {
    let range = 10; // 21×21 chunks

    for cx in -range..=range {
        for cz in -range..=range {
            for lx in 0..16 {
                for lz in 0..16 {
                    let x = cx * 16 + lx;
                    let z = cz * 16 + lz;

                    // Ground
                    world.set_block(x, 0, z, BlockState::new(7, 0)); // bedrock

                    // Layers 1-48: dense 3D checkerboard (every block has 6 exposed faces)
                    for y in 1..=48 {
                        if (x + y + z) % 2 == 0 {
                            // Vary block types across layers to stress all render paths
                            let block = match y % 6 {
                                0 => BlockState::STONE,
                                1 => BlockState::new(4, 0),  // cobblestone
                                2 => BlockState::new(98, 0), // stone bricks
                                3 => BlockState::new(18, 0), // leaves (cutout)
                                4 => BlockState::new(20, 0), // glass (cutout)
                                _ => BlockState::new(95, (x.unsigned_abs() % 16) as u8), // stained glass (transparent)
                            };
                            world.set_block(x, y, z, block);
                        }
                    }

                    // Light
                    for y in 49..65 {
                        world.set_light(x, y, z, 0, 15);
                    }
                }
            }
        }
    }

    DVec3::new(0.5, 52.0, 0.5)
}

/// Dramatic-relief terrain height (range ~28..100): a realistic rolling
/// landscape with the large flat-ish coplanar surfaces that greedy meshing and
/// occlusion culling target — the opposite of the checkerboard stress scene.
fn terrain_height_tall(x: i32, z: i32) -> i32 {
    let fx = x as f64;
    let fz = z as f64;
    let h = 64.0
        + 22.0 * (fx * 0.012).sin() * (fz * 0.013).cos()
        + 10.0 * (fx * 0.04 + 1.0).sin() * (fz * 0.035 + 2.0).cos()
        + 4.0 * (fx * 0.10 + 3.0).cos() * (fz * 0.09 + 1.0).sin();
    h as i32
}

/// Realistic-terrain GPU benchmark: a large rolling landscape (25×25 chunks)
/// with hills, ores, water and trees, viewed from a fixed elevated vista. This
/// is the representative real-world render load — its broad coplanar grass /
/// stone / dirt surfaces are exactly what greedy meshing collapses, so before /
/// after triangle counts here reflect real gameplay rather than the synthetic
/// checkerboard.
/// Minimal scene: one block at the origin in an empty world. Used by the pass
/// benchmark's `no-all` config to read the fixed per-frame floor (clear + submit
/// + present), i.e. the maximum FPS the engine can reach with no fill work.
fn build_demo_single_cube(world: &mut World) -> DVec3 {
    world.set_block(0, 64, 0, BlockState::STONE);
    for y in 64..=80 {
        world.set_light(0, y, 0, 0, 15);
        world.set_light(0, y, 5, 0, 15);
    }
    // Stand a few blocks back on -Z; yaw 0 looks toward +Z, at the block.
    DVec3::new(0.5, 64.0, -5.5)
}

fn build_demo_terrain(world: &mut World) -> DVec3 {
    let water_level = 56;
    let range = 12; // 25×25 chunks

    for cx in -range..=range {
        for cz in -range..=range {
            for lx in 0..16 {
                for lz in 0..16 {
                    let x = cx * 16 + lx;
                    let z = cz * 16 + lz;
                    let h = terrain_height_tall(x, z);
                    let surface = h.max(water_level);

                    world.set_block(x, 0, z, BlockState::new(7, 0)); // bedrock

                    // Stone fill with scattered ores up to a few blocks below the
                    // surface.
                    for y in 1..h.saturating_sub(3) {
                        let r = hash2d(x, y * 37 + z, 42);
                        let block = if r % 80 == 0 {
                            BlockState::new(16, 0) // coal
                        } else if r % 120 == 0 {
                            BlockState::new(15, 0) // iron
                        } else {
                            BlockState::STONE
                        };
                        world.set_block(x, y, z, block);
                    }

                    // Dirt / sand subsurface.
                    let near_water = h <= water_level + 2;
                    let fill = if near_water {
                        BlockState::new(12, 0) // sand
                    } else {
                        BlockState::DIRT
                    };
                    for y in h.saturating_sub(3).max(1)..h {
                        world.set_block(x, y, z, fill);
                    }

                    // Surface block, or water column over the seabed.
                    if h > water_level {
                        let top = if near_water { fill } else { BlockState::GRASS };
                        world.set_block(x, h, z, top);
                    } else {
                        world.set_block(x, h, z, fill);
                        for y in (h + 1)..=water_level {
                            world.set_block(x, y, z, BlockState::new(9, 0)); // still water
                        }
                    }

                    // Full sky light above the surface.
                    for y in surface..=(surface + 16) {
                        world.set_light(x, y, z, 0, 15);
                    }
                }
            }
        }
    }

    // Trees on dry grass, ~2 per chunk.
    let tree_range = range * 16;
    for cx in -range..=range {
        for cz in -range..=range {
            for seed in [111u32, 222] {
                let r = hash2d(cx, cz, seed);
                let lx = (r % 14 + 1) as i32;
                let lz = ((r >> 8) % 14 + 1) as i32;
                let x = cx * 16 + lx;
                let z = cz * 16 + lz;
                if x.abs() > tree_range || z.abs() > tree_range {
                    continue;
                }
                let h = terrain_height_tall(x, z);
                if h > water_level + 2 {
                    place_tree(world, x, h + 1, z);
                }
            }
        }
    }

    // Elevated vista above the peaks so the whole landscape fills the frustum.
    DVec3::new(0.5, 104.0, 0.5)
}

/// Entity stress test: flat ground with hundreds of mob entities.
fn build_demo_entity_stress(world: &mut World) -> DVec3 {
    let range = 4; // 9×9 chunks

    // Flat grass ground
    for cx in -range..=range {
        for cz in -range..=range {
            for lx in 0..16 {
                for lz in 0..16 {
                    let x = cx * 16 + lx;
                    let z = cz * 16 + lz;
                    world.set_block(x, 0, z, BlockState::GRASS);
                    for y in 1..4 {
                        world.set_light(x, y, z, 0, 15);
                    }
                }
            }
        }
    }

    // Spawn 500 mobs in a grid
    let mob_types = [50u8, 51, 52, 54, 90, 91, 92, 93, 95, 120];
    let mut eid = 100;
    let grid = 23; // ~23×23 = 529 entities
    let spacing = 3.0;
    let offset = -(grid as f64) * spacing / 2.0;

    for ix in 0..grid {
        for iz in 0..grid {
            let x = offset + ix as f64 * spacing + 0.5;
            let z = offset + iz as f64 * spacing + 0.5;
            let kind_idx = (ix * grid + iz) % mob_types.len();
            let yaw = hash2d(ix as i32, iz as i32, 999) % 360;
            world.upsert_entity(EntityState::new_remote(
                EntityId(eid),
                EntityKind::Mob(mob_types[kind_idx]),
                DVec3::new(x, 1.0, z),
                yaw as f32,
                0.0,
            ));
            eid += 1;
        }
    }

    DVec3::new(0.5, 2.0, 0.5)
}

/// Armor defense points per item ID (vanilla 1.8.9).
fn armor_points(id: i16) -> i32 {
    match id {
        298 => 1, 299 => 3, 300 => 2, 301 => 1, // leather
        302 => 2, 303 => 5, 304 => 4, 305 => 1, // chainmail
        306 => 2, 307 => 6, 308 => 5, 309 => 2, // iron
        310 => 3, 311 => 8, 312 => 6, 313 => 3, // diamond
        314 => 2, 315 => 5, 316 => 3, 317 => 1, // gold
        _ => 0,
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

    fn item(id: i16, count: u8) -> SlotItem {
        SlotItem::new(id, count, 0)
    }

    /// Stage `a` and run the vanilla runTick input section (the real path).
    fn act(gs: &mut GameState, a: TickActions) -> Vec<ServerboundPacket> {
        gs.set_pending_actions(a);
        gs.process_tick_actions()
    }

    /// A left-click press (attack held).
    fn attack(gs: &mut GameState) -> Vec<ServerboundPacket> {
        act(
            gs,
            TickActions {
                attack_pressed: true,
                left_held: true,
                ..Default::default()
            },
        )
    }

    /// A right-click press (use held).
    fn use_item(gs: &mut GameState) -> Vec<ServerboundPacket> {
        act(
            gs,
            TickActions {
                use_pressed: true,
                right_held: true,
                ..Default::default()
            },
        )
    }

    #[test]
    fn left_click_picks_up_then_places_a_stack() {
        let mut g = GameState::empty_for_server(1.0);
        g.open_player_inventory();
        g.inventory[9] = Some(item(1, 64));
        g.container_click(9, 0, 0);
        assert_eq!(g.cursor_item, Some(item(1, 64)));
        assert_eq!(g.inventory[9], None);
        g.container_click(10, 0, 0);
        assert_eq!(g.cursor_item, None);
        assert_eq!(g.inventory[10], Some(item(1, 64)));
    }

    #[test]
    fn right_click_takes_the_ceil_half() {
        let mut g = GameState::empty_for_server(1.0);
        g.open_player_inventory();
        g.inventory[9] = Some(item(1, 9));
        g.container_click(9, 1, 0);
        assert_eq!(g.cursor_item, Some(item(1, 5)));
        assert_eq!(g.inventory[9], Some(item(1, 4)));
    }

    #[test]
    fn left_click_merges_up_to_the_max_stack() {
        let mut g = GameState::empty_for_server(1.0);
        g.open_player_inventory();
        g.inventory[9] = Some(item(1, 60));
        g.cursor_item = Some(item(1, 20));
        g.container_click(9, 0, 0);
        assert_eq!(g.inventory[9], Some(item(1, 64)));
        assert_eq!(g.cursor_item, Some(item(1, 16)));
    }

    #[test]
    fn different_items_swap_on_click() {
        let mut g = GameState::empty_for_server(1.0);
        g.open_player_inventory();
        g.inventory[9] = Some(item(1, 10));
        g.cursor_item = Some(item(5, 3));
        g.container_click(9, 0, 0);
        assert_eq!(g.inventory[9], Some(item(5, 3)));
        assert_eq!(g.cursor_item, Some(item(1, 10)));
    }

    #[test]
    fn shift_click_moves_a_hotbar_stack_into_the_main_inventory() {
        let mut g = GameState::empty_for_server(1.0);
        g.open_player_inventory();
        g.inventory[36] = Some(item(1, 10));
        let packets = g.container_click(36, 0, 1);
        assert_eq!(g.inventory[36], None);
        assert_eq!(g.inventory[9], Some(item(1, 10)));
        assert!(matches!(
            packets[0],
            ServerboundPacket::ClickWindow { mode: 1, slot: 36, .. }
        ));
    }

    #[test]
    fn q_drops_one_outside_click_drops_the_cursor() {
        let mut g = GameState::empty_for_server(1.0);
        g.open_player_inventory();
        g.inventory[9] = Some(item(1, 3));
        g.container_click(9, 0, 4); // Q
        assert_eq!(g.inventory[9], Some(item(1, 2)));
        // Outside-window click drops the cursor stack.
        g.cursor_item = Some(item(2, 5));
        g.container_click(-999, 0, 0);
        assert_eq!(g.cursor_item, None);
    }

    #[test]
    fn left_paint_drag_even_splits_across_painted_slots() {
        let mut g = GameState::empty_for_server(1.0);
        g.open_player_inventory();
        g.cursor_item = Some(item(1, 6));
        g.container_drag_begin(0);
        g.container_drag_add(9);
        g.container_drag_add(10);
        g.container_drag_add(11);
        let packets = g.container_drag_commit();
        assert_eq!(g.inventory[9], Some(item(1, 2)));
        assert_eq!(g.inventory[10], Some(item(1, 2)));
        assert_eq!(g.inventory[11], Some(item(1, 2)));
        assert_eq!(g.cursor_item, None);
        // start + one per slot + end, all mode 5.
        assert_eq!(packets.len(), 5);
        assert!(packets
            .iter()
            .all(|p| matches!(p, ServerboundPacket::ClickWindow { mode: 5, .. })));
    }

    #[test]
    fn mounting_follows_the_vehicle_then_detaching_clears_it() {
        let mut g = GameState::empty_for_server(1.0);
        // A vehicle entity (pig, id 1) at a known position.
        g.apply_play_packet(ClientboundPlayPacket::SpawnMob {
            entity_id: 1,
            kind: 90,
            x: 10.0,
            y: 64.0,
            z: -5.0,
            yaw: 0.0,
            pitch: 0.0,
            head_pitch: 0.0,
            metadata: Vec::new(),
        });
        g.apply_play_packet(ClientboundPlayPacket::AttachEntity {
            entity_id: 0, // the local player
            vehicle_id: 1,
            leash: false,
        });
        g.tick(0.05);
        // The rider snaps onto the vehicle (pig height 0.9 × 0.75 mount offset).
        assert!((g.player.position.x - 10.0).abs() < 1.0e-6);
        assert!((g.player.position.z + 5.0).abs() < 1.0e-6);
        assert!((g.player.position.y - (64.0 + 0.9 * 0.75)).abs() < 1.0e-6);

        // Detaching (vehicle_id -1) clears the mount so physics resumes.
        g.apply_play_packet(ClientboundPlayPacket::AttachEntity {
            entity_id: 0,
            vehicle_id: -1,
            leash: false,
        });
        assert!(!g.vehicles.contains_key(&EntityId(0)));
    }

    #[test]
    fn server_window_opens_routes_slots_and_closes() {
        let mut g = GameState::empty_for_server(1.0);
        // S2D opens a single chest (27 slots); the host is signalled to show it.
        g.apply_play_packet(ClientboundPlayPacket::OpenWindow {
            window_id: 5,
            inventory_type: "minecraft:chest".to_owned(),
            title: "{\"text\":\"Chest\"}".to_owned(),
            slots: 27,
            entity_id: None,
        });
        assert!(g.take_window_open());
        assert!(g.open_container().is_some());

        // S30 WindowItems replaces the whole window (27 chest + 36 player).
        g.apply_play_packet(ClientboundPlayPacket::WindowItems {
            window_id: 5,
            items: vec![None; 63],
        });
        // S2F SetSlot for a chest slot routes through the container.
        g.apply_play_packet(ClientboundPlayPacket::SetSlot {
            window_id: 5,
            slot: 0,
            item: Some(item(1, 10)),
        });
        assert_eq!(
            g.open_container().unwrap().slot_item(0, g.inventory_slots()),
            Some(item(1, 10))
        );

        // Shift-click moves the chest stack into the player inventory (reverse
        // fill → last hotbar slot, window slot 62 == inventory slot 44).
        let packets = g.container_click(0, 0, 1);
        assert!(matches!(
            packets[0],
            ServerboundPacket::ClickWindow { window_id: 5, slot: 0, mode: 1, .. }
        ));
        assert_eq!(g.open_container().unwrap().slot_item(0, g.inventory_slots()), None);
        assert_eq!(g.inventory[44], Some(item(1, 10)));

        // S2E force-close drops the window and signals the host to pop the screen.
        g.apply_play_packet(ClientboundPlayPacket::CloseWindowS { window_id: 5 });
        assert!(g.take_window_close());
        assert!(g.open_container().is_none());
    }

    #[test]
    fn number_key_swaps_with_the_hotbar_slot() {
        let mut g = GameState::empty_for_server(1.0);
        g.open_player_inventory();
        g.inventory[9] = Some(item(1, 5));
        g.inventory[36] = Some(item(2, 1));
        g.container_click(9, 0, 2); // press "1" over slot 9
        assert_eq!(g.inventory[9], Some(item(2, 1)));
        assert_eq!(g.inventory[36], Some(item(1, 5)));
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
    fn sword_right_click_air_starts_and_releases_blocking() {
        let mut gs = looking_along_x();
        // Diamond sword in the selected hotbar slot, aiming at empty air.
        gs.inventory[36] = Some(SlotItem::new(276, 1, 0));
        // Vanilla `sendUseItem`: C08 with the in-air position (-1,-1,-1)/face 255.
        let start = use_item(&mut gs);
        assert!(
            matches!(
                start.as_slice(),
                [ServerboundPacket::PlayerBlockPlacement {
                    x: -1,
                    y: -1,
                    z: -1,
                    face: 255,
                    ..
                }]
            ),
            "got {start:?}"
        );
        assert_eq!(gs.use_action, ItemUseAction::Block);
        // Holding keeps the item in use without re-sending anything.
        assert!(act(
            &mut gs,
            TickActions {
                right_held: true,
                ..Default::default()
            }
        )
        .is_empty());
        assert_eq!(gs.use_action, ItemUseAction::Block);
        // Releasing sends C07 RELEASE_USE_ITEM and clears the state.
        let stop = act(&mut gs, TickActions::default());
        assert!(
            matches!(
                stop.as_slice(),
                [ServerboundPacket::PlayerDigging {
                    status: DiggingStatus::ReleaseUseItem,
                    ..
                }]
            ),
            "got {stop:?}"
        );
        assert_eq!(gs.use_action, ItemUseAction::None);
    }

    #[test]
    fn non_sword_right_click_air_sends_use_but_does_not_block() {
        let mut gs = looking_along_x();
        gs.inventory[36] = Some(SlotItem::new(1, 1, 0));
        // Vanilla sendUseItem fires for any held item (the "right-click air"
        // C08), but only usable items enter the use state.
        let packets = use_item(&mut gs);
        assert!(
            matches!(
                packets.as_slice(),
                [ServerboundPacket::PlayerBlockPlacement { x: -1, face: 255, .. }]
            ),
            "got {packets:?}"
        );
        assert_eq!(gs.use_action, ItemUseAction::None);
    }

    #[test]
    fn blocking_cancels_sprint_through_the_tick() {
        let mut gs = looking_along_x();
        gs.player.on_ground = true;
        gs.sprinting = true;
        gs.input.sprint = true;
        gs.input.forward = true;
        // Hold a sword and keep blocking (right held) so the tick keeps the
        // item in use and slows movement below the sprint threshold.
        gs.inventory[36] = Some(SlotItem::new(276, 1, 0));
        gs.use_action = ItemUseAction::Block;
        gs.set_pending_actions(TickActions {
            right_held: true,
            ..Default::default()
        });
        let (_packets, movement) = gs.tick(0.05).expect("not a freeze tick");
        assert_eq!(gs.use_action, ItemUseAction::Block);
        assert!(!movement.sprinting, "blocking drops sprint within the tick");
    }

    #[test]
    fn sprint_key_with_forward_starts_and_releasing_forward_stops() {
        let mut gs = looking_along_x();
        gs.player.on_ground = true;
        gs.input.forward = true;
        gs.input.sprint = true; // toggle = keyBindSprint.isKeyDown()
        let (_p, m) = gs.tick(0.05).expect("tick");
        assert!(m.sprinting, "sprint key + forward starts sprinting");
        // Release forward: moveForward drops below 0.8 → stop.
        gs.input.forward = false;
        let (_p, m) = gs.tick(0.05).expect("tick");
        assert!(!m.sprinting, "releasing forward stops the sprint");
    }

    #[test]
    fn wall_collision_ends_the_tick_not_sprinting() {
        let mut gs = looking_along_x();
        gs.player.on_ground = true;
        gs.player.collided_horizontally = true;
        gs.input.forward = true;
        gs.input.sprint = true;
        // Vanilla starts (no collision check) then stops (collidedHorizontally)
        // in the same onLivingUpdate, so it never flickers on in the report.
        let (_p, m) = gs.tick(0.05).expect("tick");
        assert!(!m.sprinting, "pressing into a wall reports not-sprinting");
    }

    #[test]
    fn double_tap_forward_starts_sprint_without_the_sprint_key() {
        let mut gs = looking_along_x();
        gs.player.on_ground = true;
        gs.input.sprint = false; // no sprint key — only the double-tap can start
        // First fresh forward press: arms the 7-tick window, no sprint yet.
        gs.input.forward = true;
        let (_p, m1) = gs.tick(0.05).expect("tick");
        assert!(!m1.sprinting, "first tap only arms sprintToggleTimer");
        // Release, then re-press within the window → sprint starts.
        gs.input.forward = false;
        let _ = gs.tick(0.05).expect("tick");
        gs.input.forward = true;
        let (_p, m3) = gs.tick(0.05).expect("tick");
        assert!(m3.sprinting, "a second tap within 7 ticks starts the sprint");
    }

    #[test]
    fn sprint_attack_resets_sprint_and_halves_horizontal_motion() {
        let mut gs = looking_along_x();
        gs.world.upsert_entity(EntityState::new_remote(
            EntityId(1),
            EntityKind::Mob(54),
            DVec3::new(2.0, 0.0, 0.5),
            0.0,
            0.0,
        ));
        gs.sprinting = true;
        gs.player.velocity = DVec3::new(1.0, 0.0, 2.0);
        let _ = attack(&mut gs);
        assert!(!gs.sprinting, "a sprint hit cancels the sprint");
        assert!((gs.player.velocity.x - 0.6).abs() < 1e-9);
        assert!((gs.player.velocity.z - 1.2).abs() < 1e-9);
    }

    #[test]
    fn attack_entity_swings_then_sends_use_entity() {
        let mut gs = looking_along_x();
        gs.world.upsert_entity(EntityState::new_remote(
            EntityId(1),
            EntityKind::Mob(54),
            DVec3::new(2.0, 0.0, 0.5),
            0.0,
            0.0,
        ));
        let packets = attack(&mut gs);
        // Vanilla `clickMouse`: swingItem (C0A) precedes attackEntity (C02).
        assert!(
            matches!(
                packets.as_slice(),
                [
                    ServerboundPacket::SwingArm,
                    ServerboundPacket::UseEntity {
                        target: 1,
                        kind: UseEntityKind::Attack
                    },
                ]
            ),
            "got {packets:?}"
        );
    }

    #[test]
    fn survival_dig_starts_on_press_and_finishes_after_holding() {
        let mut gs = looking_along_x();
        gs.world.set_block(3, 0, 0, BlockState::STONE);
        // Vanilla press tick: clickMouse swings + sends START, then
        // sendClickBlockToController advances the dig and swings again (two C0A).
        let packets = attack(&mut gs);
        assert!(
            matches!(
                packets.as_slice(),
                [
                    ServerboundPacket::SwingArm,
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
        for _ in 0..400 {
            if gs.world.block_at(3, 0, 0).is_air() {
                break;
            }
            for packet in act(
                &mut gs,
                TickActions {
                    left_held: true,
                    ..Default::default()
                },
            ) {
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
        // Empty hand: vanilla sends the C08 to trigger onBlockActivated but does
        // NOT swing (onPlayerRightClick returns false with no item).
        let packets = use_item(&mut gs);
        assert!(
            matches!(
                packets.as_slice(),
                [ServerboundPacket::PlayerBlockPlacement { x: 3, face: 4, .. }]
            ),
            "got {packets:?}"
        );
    }

    #[test]
    fn use_block_predicts_placement_and_sends_held_item() {
        let mut gs = looking_along_x();
        gs.world.set_block(3, 0, 0, BlockState::STONE);
        gs.selected_slot = 0;
        gs.inventory[36] = Some(SlotItem::new(1, 64, 0)); // stone in hotbar slot 0
        assert!(gs.world.block_at(2, 0, 0).is_air());

        let packets = use_item(&mut gs);

        // The block shows immediately at the adjacent cell, without a server echo.
        assert_eq!(gs.world.block_at(2, 0, 0), BlockState::new(1, 0));
        // C08 now carries the held stack rather than an empty slot.
        assert!(
            matches!(
                packets.first(),
                Some(ServerboundPacket::PlayerBlockPlacement {
                    held_item: Some(HeldItem { id: 1, .. }),
                    ..
                })
            ),
            "got {packets:?}"
        );
    }

    #[test]
    fn server_cancel_reverts_a_predicted_placement() {
        let mut gs = looking_along_x();
        gs.world.set_block(3, 0, 0, BlockState::STONE);
        gs.selected_slot = 0;
        gs.inventory[36] = Some(SlotItem::new(1, 64, 0));
        let _ = use_item(&mut gs);
        assert_eq!(
            gs.world.block_at(2, 0, 0),
            BlockState::new(1, 0),
            "placement is predicted locally"
        );

        // The server rejects the placement and sends the original (air) back. The
        // prediction never locks the cell, so this authoritative update wins.
        let remesh = gs.apply_play_packet(ClientboundPlayPacket::BlockChange {
            x: 2,
            y: 0,
            z: 0,
            id: 0,
            meta: 0,
        });
        assert!(
            gs.world.block_at(2, 0, 0).is_air(),
            "a server cancel must revert the phantom block"
        );
        assert!(remesh, "the revert should re-mesh the chunk");
    }

    #[test]
    fn use_block_on_interactable_does_not_predict() {
        let mut gs = looking_along_x();
        gs.world.set_block(3, 0, 0, BlockState::new(54, 2)); // chest
        gs.selected_slot = 0;
        gs.inventory[36] = Some(SlotItem::new(1, 64, 0));
        let _ = use_item(&mut gs);
        assert!(
            gs.world.block_at(2, 0, 0).is_air(),
            "right-clicking a chest must not conjure a phantom block in front of it"
        );
    }

    #[test]
    fn cannot_place_a_block_inside_the_player() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.world.set_block(0, 0, 0, BlockState::STONE); // floor (also loads the chunk)
        let stone = SlotItem::new(1, 64, 0);
        // Standing on the floor (feet at y=1): clicking its top face would land a
        // block at our feet — its 0.6×1.8 box covers cell (0,1,0).
        gs.player.position = DVec3::new(0.5, 1.0, 0.5);
        gs.player.sync_aabb_to_position();
        assert!(
            !gs.predict_placement(0, 0, 0, 1, 8, Some(&stone)),
            "must not place a block where the player is standing"
        );
        assert!(gs.world.block_at(0, 1, 0).is_air());
        // Jump clear (feet at y=2) and the cell frees up — placement now predicts.
        gs.player.position = DVec3::new(0.5, 2.0, 0.5);
        gs.player.sync_aabb_to_position();
        assert!(gs.predict_placement(0, 0, 0, 1, 8, Some(&stone)));
        assert_eq!(gs.world.block_at(0, 1, 0), BlockState::new(1, 0));
    }

    #[test]
    fn placement_block_state_orients_common_blocks() {
        let stone = SlotItem::new(1, 1, 0);
        assert_eq!(
            placement_block_state(&stone, 1, 0.0, 8),
            Some(BlockState::new(1, 0))
        );
        // Slab on top of a block → bottom half; on the underside → top half.
        let slab = SlotItem::new(44, 1, 0);
        assert_eq!(
            placement_block_state(&slab, 1, 0.0, 8),
            Some(BlockState::new(44, 0))
        );
        assert_eq!(
            placement_block_state(&slab, 0, 0.0, 8),
            Some(BlockState::new(44, 8))
        );
        // Log placed against an x-facing face → x axis (meta bit 4).
        let log = SlotItem::new(17, 1, 0);
        assert_eq!(
            placement_block_state(&log, 4, 0.0, 8),
            Some(BlockState::new(17, 4))
        );
        // Non-block items and oriented blocks we don't model are not predicted.
        let pickaxe = SlotItem::new(278, 1, 0);
        assert!(placement_block_state(&pickaxe, 1, 0.0, 8).is_none());
        let torch = SlotItem::new(50, 1, 0);
        assert!(placement_block_state(&torch, 1, 0.0, 8).is_none());
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
        let mut gs = GameState::demo(DemoKind::Landscape, 1.0); // demo grants allow_flying
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
        let mut gs = GameState::demo(DemoKind::Landscape, 1.0);
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
        let mut gs = GameState::demo(DemoKind::Landscape, 1.0);
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
    fn title_packet_sets_text_and_fades_out() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.apply_play_packet(ClientboundPlayPacket::Title {
            action: TitleAction::Times {
                fade_in: 2,
                stay: 2,
                fade_out: 2,
            },
        });
        gs.apply_play_packet(ClientboundPlayPacket::Title {
            action: TitleAction::Subtitle {
                json: r#"{"text":"Sub"}"#.to_owned(),
            },
        });
        assert!(
            gs.title_overlay(0.0).is_none(),
            "subtitle alone does not start the timer"
        );

        gs.apply_play_packet(ClientboundPlayPacket::Title {
            action: TitleAction::Title {
                json: r#"{"text":"Main","color":"gold"}"#.to_owned(),
            },
        });
        assert!(
            gs.title_overlay(0.0).is_none(),
            "first fade-in frame starts fully transparent"
        );
        let overlay = gs
            .title_overlay(1.0)
            .expect("title fades in during the first tick");
        assert_eq!(overlay.title, "§6Main");
        assert_eq!(overlay.subtitle, "Sub");
        assert!((overlay.alpha - 0.5).abs() < 1.0e-6);

        gs.tick(0.05);
        gs.tick(0.05);
        assert!((gs.title_overlay(0.0).unwrap().alpha - 1.0).abs() < 1.0e-6);
        gs.tick(0.05);
        gs.tick(0.05);
        assert!((gs.title_overlay(0.0).unwrap().alpha - 1.0).abs() < 1.0e-6);
        gs.tick(0.05);
        assert!((gs.title_overlay(0.0).unwrap().alpha - 0.5).abs() < 1.0e-6);
        gs.tick(0.05);
        assert!(
            gs.title_overlay(0.0).is_none(),
            "title expires and clears itself"
        );
    }

    #[test]
    fn title_clear_and_reset_match_vanilla() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.apply_play_packet(ClientboundPlayPacket::Title {
            action: TitleAction::Times {
                fade_in: 1,
                stay: 1,
                fade_out: 1,
            },
        });
        gs.apply_play_packet(ClientboundPlayPacket::Title {
            action: TitleAction::Title {
                json: r#"{"text":"A"}"#.to_owned(),
            },
        });
        assert!(gs.title_overlay(1.0).is_some());
        gs.apply_play_packet(ClientboundPlayPacket::Title {
            action: TitleAction::Clear,
        });
        assert!(gs.title_overlay(0.0).is_none());

        gs.apply_play_packet(ClientboundPlayPacket::Title {
            action: TitleAction::Title {
                json: r#"{"text":"B"}"#.to_owned(),
            },
        });
        assert_eq!(gs.title.timer, 3, "clear keeps custom timings");
        gs.apply_play_packet(ClientboundPlayPacket::Title {
            action: TitleAction::Reset,
        });
        assert!(gs.title_overlay(0.0).is_none());
        gs.apply_play_packet(ClientboundPlayPacket::Title {
            action: TitleAction::Title {
                json: r#"{"text":"C"}"#.to_owned(),
            },
        });
        assert_eq!(
            gs.title.timer, 100,
            "reset restores vanilla 10/70/20 timings"
        );
    }

    #[test]
    fn movement_speed_attribute_applies_potions_but_skips_sprint_boost() {
        use recraft_protocol::v1_8_9::packets::{AttributeModifier, EntityProperty};
        let mut gs = GameState::empty_for_server(1.0);
        gs.player.id = EntityId(7);
        gs.apply_play_packet(ClientboundPlayPacket::EntityProperties {
            entity_id: 7,
            properties: vec![EntityProperty {
                key: "generic.movementSpeed".to_owned(),
                base: 0.1,
                modifiers: vec![
                    // Speed I potion: op 2, +20%.
                    AttributeModifier {
                        uuid: [1; 16],
                        amount: 0.2,
                        operation: 2,
                    },
                    // The sprint boost must be excluded (physics models it).
                    AttributeModifier {
                        uuid: SPRINT_SPEED_BOOST_UUID,
                        amount: 0.3,
                        operation: 2,
                    },
                ],
            }],
        });
        assert!((gs.walk_speed_attribute - 0.12).abs() < 1.0e-6);

        // Attributes for other entities don't touch the player's speed.
        gs.apply_play_packet(ClientboundPlayPacket::EntityProperties {
            entity_id: 99,
            properties: vec![EntityProperty {
                key: "generic.movementSpeed".to_owned(),
                base: 0.5,
                modifiers: Vec::new(),
            }],
        });
        assert!((gs.walk_speed_attribute - 0.12).abs() < 1.0e-6);
    }

    #[test]
    fn attribute_operations_follow_vanilla_compute_value() {
        use recraft_protocol::v1_8_9::packets::{AttributeModifier, EntityProperty};
        let modifier = |amount: f64, operation: u8| AttributeModifier {
            uuid: [amount.to_bits() as u8; 16],
            amount,
            operation,
        };
        let property = EntityProperty {
            key: "generic.movementSpeed".to_owned(),
            base: 0.1,
            modifiers: vec![
                modifier(0.05, 0), // d0 = 0.15
                modifier(0.5, 1),  // d1 = 0.15 * 1.5 = 0.225
                modifier(0.2, 2),  // 0.225 * 1.2 = 0.27
            ],
        };
        let value = effective_attribute_value(&property, &SPRINT_SPEED_BOOST_UUID);
        assert!((value - 0.27).abs() < 1.0e-9, "got {value}");
    }

    #[test]
    fn entity_movement_interpolates_instead_of_snapping() {
        let mut gs = GameState::demo(DemoKind::Landscape, 1.0);
        gs.world.upsert_entity(EntityState::new_remote(
            EntityId(9),
            EntityKind::Mob(54),
            DVec3::new(10.0, 1.0, 10.0),
            0.0,
            0.0,
        ));
        // Two relative moves accumulate on the server-side target.
        for _ in 0..2 {
            gs.apply_play_packet(ClientboundPlayPacket::EntityRelativeMove {
                entity_id: 9,
                dx: 1.5,
                dy: 0.0,
                dz: 0.0,
            });
        }
        let x = |gs: &GameState| {
            gs.world
                .entities()
                .find(|e| e.id == EntityId(9))
                .unwrap()
                .position
                .x
        };
        assert!((x(&gs) - 10.0).abs() < 1.0e-9, "packets must not snap");
        gs.tick(0.05);
        assert!((x(&gs) - 11.0).abs() < 1.0e-9, "1/3 of the way after a tick");
        gs.tick(0.05);
        gs.tick(0.05);
        assert!((x(&gs) - 13.0).abs() < 1.0e-9, "settled on the target");
    }

    #[test]
    fn invisible_armor_stand_hides_model_but_shows_floating_text() {
        use recraft_protocol::v1_8_9::packets::MetadataEntry;
        let mut g = GameState::empty_for_server(1.0);
        g.camera.position = Vec3::new(0.5, 81.0, 0.5);
        // An armor stand (object type 78) a few blocks from the player.
        g.apply_play_packet(ClientboundPlayPacket::SpawnObject {
            entity_id: 7,
            kind: 78,
            x: 3.0,
            y: 80.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            data: 0,
            velocity: None,
        });
        // Invisible (flag 0x20) + a visible custom name → floating text.
        g.apply_play_packet(ClientboundPlayPacket::EntityMetadata {
            entity_id: 7,
            metadata: vec![
                MetadataEntry { index: 0, value: MetadataValue::Byte(0x20) },
                MetadataEntry { index: 2, value: MetadataValue::Str("Hello".to_string()) },
                MetadataEntry { index: 3, value: MetadataValue::Byte(1) },
            ],
        });
        let stand = g.world.entity(EntityId(7)).unwrap();
        assert!(stand.invisible);
        assert_eq!(stand.custom_name.as_deref(), Some("Hello"));
        assert!(stand.custom_name_visible);

        // The invisible stand contributes no model geometry…
        let mut mesh = ModelMesh::new();
        let skins = std::collections::HashMap::new();
        g.build_entity_model(&mut mesh, 1.0, 1.0, &skins, f64::INFINITY, false);
        assert!(mesh.is_empty(), "invisible entity must not render a model");

        // …but its floating-text plate is still emitted.
        let tags = g.player_nametags(1.0);
        assert!(
            tags.iter().any(|(name, _)| name == "Hello"),
            "floating text must show for a named invisible stand"
        );
    }

    #[test]
    fn invisible_player_still_shows_worn_armor() {
        use recraft_protocol::v1_8_9::packets::MetadataEntry;
        let mut g = GameState::empty_for_server(1.0);
        // Camera overlaps the entity so it always clears the frustum cull.
        g.camera.position = Vec3::new(0.5, 81.0, 0.5);
        let skins = std::collections::HashMap::new();

        // An invisible player right next to the camera.
        g.apply_play_packet(ClientboundPlayPacket::SpawnPlayer {
            entity_id: 8,
            uuid: [0u8; 16],
            x: 0.5,
            y: 80.5,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
        });
        g.apply_play_packet(ClientboundPlayPacket::EntityMetadata {
            entity_id: 8,
            metadata: vec![MetadataEntry { index: 0, value: MetadataValue::Byte(0x20) }],
        });

        // Bare invisible player: nothing renders.
        let mut bare = ModelMesh::new();
        g.build_entity_model(&mut bare, 1.0, 1.0, &skins, f64::INFINITY, false);
        assert!(bare.is_empty(), "invisible player with no armor renders nothing");

        // Give it an iron helmet (slot 4, id 306): the worn armor still shows.
        g.apply_play_packet(ClientboundPlayPacket::EntityEquipment {
            entity_id: 8,
            slot: 4,
            item: Some(SlotItem::new(306, 1, 0)),
        });
        let mut armored = ModelMesh::new();
        g.build_entity_model(&mut armored, 1.0, 1.0, &skins, f64::INFINITY, false);
        assert!(!armored.is_empty(), "invisible player must still show worn armor");
    }

    #[test]
    fn continuous_swinging_cycles_instead_of_pinning_at_the_start() {
        let mut gs = GameState::demo(DemoKind::Landscape, 1.0);
        // Swinging every tick (hold-to-mine) must advance through the vanilla
        // half-cycle (restart allowed past the midpoint), not reset to zero
        // every tick.
        let mut max_progress: f32 = 0.0;
        let mut distinct = std::collections::BTreeSet::new();
        for _ in 0..12 {
            gs.swing_arm();
            gs.tick(0.05);
            let p = gs.swing_progress(1.0);
            max_progress = max_progress.max(p);
            distinct.insert((p * 1000.0) as i32);
        }
        // The cycle must reach at least the vanilla midpoint restart (the
        // wrap-around interpolation passes through 1.0 on the restart tick).
        assert!(
            (0.5..=1.0).contains(&max_progress),
            "swing never progressed: {max_progress}"
        );
        assert!(distinct.len() >= 3, "swing progress never animated");
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

    #[test]
    fn xp_orb_texture_cell_matches_vanilla_thresholds() {
        assert_eq!(xp_orb_texture_cell(0), 0);
        assert_eq!(xp_orb_texture_cell(2), 0);
        assert_eq!(xp_orb_texture_cell(3), 1);
        assert_eq!(xp_orb_texture_cell(6), 1);
        assert_eq!(xp_orb_texture_cell(7), 2);
        assert_eq!(xp_orb_texture_cell(16), 2);
        assert_eq!(xp_orb_texture_cell(17), 3);
        assert_eq!(xp_orb_texture_cell(37), 4);
        assert_eq!(xp_orb_texture_cell(73), 5);
        assert_eq!(xp_orb_texture_cell(149), 6);
        assert_eq!(xp_orb_texture_cell(307), 7);
        assert_eq!(xp_orb_texture_cell(617), 8);
        assert_eq!(xp_orb_texture_cell(1237), 9);
        assert_eq!(xp_orb_texture_cell(2477), 10);
        assert_eq!(xp_orb_texture_cell(i16::MAX), 10);
    }

    #[test]
    fn spawn_experience_orb_tracks_entity_and_xp() {
        let mut g = GameState::empty_for_server(1.0);
        g.apply_play_packet(ClientboundPlayPacket::SpawnExperienceOrb {
            entity_id: 7,
            x: 3.0,
            y: 64.0,
            z: -2.0,
            count: 150,
        });
        let orb = g.world.entity(EntityId(7)).expect("orb spawned");
        assert_eq!(orb.kind, EntityKind::ExperienceOrb);
        assert_eq!(g.entity_xp.get(&EntityId(7)).copied(), Some(150));
        // The renderer-facing list reports one billboard with cell-6 UVs.
        let orbs = g.xp_orbs(1.0);
        assert_eq!(orbs.len(), 1);
        // count 150 → cell 6; despawn drops the tracking.
        g.apply_play_packet(ClientboundPlayPacket::DestroyEntities { entity_ids: vec![7] });
        assert!(g.entity_xp.get(&EntityId(7)).is_none());
        assert!(g.xp_orbs(1.0).is_empty());
    }

    #[test]
    fn falling_block_decodes_id_and_meta_from_data() {
        let mut g = GameState::empty_for_server(1.0);
        // data = id | meta << 12: stone-bricks (98) meta 3.
        let data = 98 | (3 << 12);
        g.apply_play_packet(ClientboundPlayPacket::SpawnObject {
            entity_id: 7,
            kind: 70,
            x: 3.0,
            y: 80.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            data,
            velocity: None,
        });
        let block = g.falling_blocks.get(&EntityId(7)).copied().expect("tracked");
        assert_eq!(block, BlockState::new(98, 3));
        let cubes = g.falling_block_cubes(1.0);
        assert_eq!(cubes.len(), 1);
        assert_eq!(cubes[0].block, BlockState::new(98, 3));
    }

    #[test]
    fn projectile_kinds_map_to_item_sprites() {
        assert_eq!(projectile_item_id(60), Some(262)); // arrow
        assert_eq!(projectile_item_id(61), Some(332)); // snowball
        assert_eq!(projectile_item_id(65), Some(368)); // ender pearl
        // Falling block, item and armor stand are rendered elsewhere, not here.
        assert_eq!(projectile_item_id(70), None);
        assert_eq!(projectile_item_id(2), None);
        assert_eq!(projectile_item_id(78), None);

        // A spawned snowball surfaces in the projectile sprite list.
        let mut g = GameState::empty_for_server(1.0);
        g.apply_play_packet(ClientboundPlayPacket::SpawnObject {
            entity_id: 7,
            kind: 61,
            x: 3.0,
            y: 64.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            data: 0,
            velocity: None,
        });
        let sprites = g.projectiles(1.0);
        assert_eq!(sprites.len(), 1);
        assert_eq!(sprites[0].item.id, 332);
    }

    #[test]
    fn note_block_action_plays_pitched_note_and_spawns_particle() {
        let mut g = GameState::empty_for_server(1.0);
        // instrument 2 (snare), note 12 → pitch 2^((12-12)/12) = 1.0.
        g.apply_play_packet(ClientboundPlayPacket::BlockAction {
            x: 1,
            y: 65,
            z: -2,
            action_id: 2,
            action_param: 12,
            block_type: 25,
        });
        let sounds = g.take_sounds();
        assert_eq!(sounds.len(), 1);
        assert_eq!(sounds[0].event, "note.snare");
        assert!((sounds[0].pitch - 1.0).abs() < 1e-6);
        assert!((sounds[0].volume - 3.0).abs() < 1e-6);
        // note 24 → pitch 2^(12/12) = 2.0; note 0 → 2^(-1) = 0.5.
        for (note, expected) in [(24u8, 2.0_f32), (0, 0.5)] {
            let mut g = GameState::empty_for_server(1.0);
            g.apply_play_packet(ClientboundPlayPacket::BlockAction {
                x: 0,
                y: 0,
                z: 0,
                action_id: 0,
                action_param: note,
                block_type: 25,
            });
            let s = g.take_sounds();
            assert!((s[0].pitch - expected).abs() < 1e-6, "note {note}");
        }
    }

    #[test]
    fn chest_block_action_opens_the_lid_and_plays_the_sound() {
        let mut g = GameState::empty_for_server(1.0);
        // A chest open (block type 54, action 1, viewers=1) arms the lid target
        // and plays random.chestopen once.
        g.apply_play_packet(ClientboundPlayPacket::BlockAction {
            x: 0,
            y: 0,
            z: 0,
            action_id: 1,
            action_param: 1,
            block_type: 54,
        });
        let sounds = g.take_sounds();
        assert_eq!(sounds.len(), 1);
        assert_eq!(sounds[0].event, "random.chestopen");
        assert!(g.chest_open_targets.get(&[0, 0, 0]).copied().unwrap_or(0.0) > 0.0);
        // Closing (viewers=0) plays the close sound and clears the target.
        g.apply_play_packet(ClientboundPlayPacket::BlockAction {
            x: 0,
            y: 0,
            z: 0,
            action_id: 1,
            action_param: 0,
            block_type: 54,
        });
        let sounds = g.take_sounds();
        assert_eq!(sounds.len(), 1);
        assert_eq!(sounds[0].event, "random.chestclosed");
    }

    #[test]
    fn piston_block_action_is_ignored() {
        let mut g = GameState::empty_for_server(1.0);
        // A piston extend (block type 33) carries no client sound/lid here.
        g.apply_play_packet(ClientboundPlayPacket::BlockAction {
            x: 0,
            y: 0,
            z: 0,
            action_id: 0,
            action_param: 1,
            block_type: 33,
        });
        assert!(g.take_sounds().is_empty());
    }

    #[test]
    fn boss_bar_reports_nearest_wither_health_fraction() {
        use recraft_protocol::v1_8_9::packets::MetadataEntry;
        let mut g = GameState::empty_for_server(1.0);
        g.camera.position = Vec3::new(0.0, 70.0, 0.0);
        // No boss in range yet.
        assert!(g.boss_bar().is_none());
        // A wither (mob type 64) close by, at half its 300 max health.
        g.apply_play_packet(ClientboundPlayPacket::SpawnMob {
            entity_id: 7,
            kind: 64,
            x: 5.0,
            y: 70.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            head_pitch: 0.0,
            metadata: vec![],
        });
        // Health metadata (index 6, float) = 150 → fraction 0.5.
        g.apply_play_packet(ClientboundPlayPacket::EntityMetadata {
            entity_id: 7,
            metadata: vec![MetadataEntry { index: 6, value: MetadataValue::Float(150.0) }],
        });
        let (name, fraction) = g.boss_bar().expect("wither in range");
        assert_eq!(name, "Wither");
        assert!((fraction - 0.5).abs() < 1e-6);
        // A dead wither (0 health) drops off the bar.
        g.apply_play_packet(ClientboundPlayPacket::EntityMetadata {
            entity_id: 7,
            metadata: vec![MetadataEntry { index: 6, value: MetadataValue::Float(0.0) }],
        });
        assert!(g.boss_bar().is_none());
    }
}
