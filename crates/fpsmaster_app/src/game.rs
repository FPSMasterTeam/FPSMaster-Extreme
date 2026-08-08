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
use fpsmaster_core::{
    collision::{is_fence, is_pane, is_stairs},
    mc_math::wrap_degrees,
    movement_direction, resting_on_ground, BlockState, ChunkPos, EntityId, EntityKind, EntityState,
    PlayerInput, PlayerPhysics, RenderShape, SectionPos, World,
};
use fpsmaster_protocol::v1_8_9::{
    chunk::{decode_chunk_column, ChunkColumnData},
    packets::{
        ClientboundPlayPacket, DiggingStatus, HeldItem, MetadataValue, ServerboundPacket, SlotItem,
        TitleAction, UseEntityKind,
    },
};
use fpsmaster_render::{
    arm_attach, Camera, ChestKind, EntityAnim, ModelMesh, SignKind, SignTextDraw,
};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, PhysicalKey},
};

use crate::chat::{self, ChatState};
use crate::container::{max_stack, stackable, Container};
use crate::item_renderer::{is_enchanted, DroppedItem, FallingBlock, PlayerHeldItem};
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
    /// 1.7-style animations: lets a sword swing/attack fire while blocking
    /// (right-click held). Off keeps the 1.8 lock (the block swallows attacks).
    pub old_animations: bool,
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
        // Arrow-key view turning is a fpsmaster extra (not a vanilla rebindable
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
    property: &fpsmaster_protocol::v1_8_9::packets::EntityProperty,
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

/// One active potion effect on the local player (S1D EntityEffect). Only the
/// amplifier feeds the HUD (absorption hearts, the heart tint); duration is
/// kept for completeness but not ticked down — the server resends.
#[derive(Debug, Clone, Copy)]
struct ActiveEffect {
    amplifier: i8,
    /// Remaining ticks as last reported by the server. Stored for the complete
    /// effect model but not consumed yet (effects are not ticked down — the
    /// server resends them); kept for a future client-side expiry.
    #[allow(dead_code)]
    duration: i32,
}

// Potion effect ids that drive the health HUD (vanilla `Potion`).
const POTION_REGENERATION: u8 = 10;
const POTION_HUNGER: u8 = 17;
const POTION_POISON: u8 = 19;
const POTION_WITHER: u8 = 20;
const POTION_ABSORPTION: u8 = 22;

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
/// Extra chunk rings kept meshed beyond the render distance before the
/// render-distance safety net drops a column's GPU mesh, so chunks at the very
/// edge don't evict/re-mesh while the player jitters across a boundary.
const RESIDENCY_MARGIN: i32 = 2;

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

/// A background-music command produced by [`MusicTicker`] and drained by the
/// host (`main.rs`) into the [`SoundManager`], so the game layer never touches
/// the audio backend directly.
#[derive(Debug, Clone)]
pub enum MusicCommand {
    /// Start the named `sounds.json` event's music (one random variant), e.g.
    /// `"music.menu"` or `"music.game"`. Replaces any track already playing.
    Play(String),
    /// Stop the current music track (e.g. when the music type changes).
    Stop,
}

/// Vanilla `MusicTicker.MusicType`: the six music groups, each with its own
/// random inter-track delay range (in 20 Hz ticks) and `sounds.json` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MusicType {
    Menu,
    Game,
    Creative,
    Nether,
    End,
    EndBoss,
}

impl MusicType {
    /// The `sounds.json` event key for this music group.
    fn event(self) -> &'static str {
        match self {
            MusicType::Menu => "music.menu",
            MusicType::Game => "music.game",
            MusicType::Creative => "music.game.creative",
            MusicType::Nether => "music.game.nether",
            MusicType::End => "music.game.end",
            MusicType::EndBoss => "music.game.end.dragon",
        }
    }

    /// Vanilla `MusicType(minDelay, maxDelay)`: the inclusive delay range, in
    /// ticks, waited before the next track of this type plays.
    fn delay_range(self) -> (i32, i32) {
        match self {
            MusicType::Menu => (20, 600),
            MusicType::Game => (12000, 24000),
            MusicType::Creative => (12000, 24000),
            MusicType::Nether => (12000, 24000),
            MusicType::End => (12000, 24000),
            // The end-boss theme retriggers with no delay while the fight runs.
            MusicType::EndBoss => (0, 0),
        }
    }
}

/// Vanilla `MusicTicker` (MCP-919): plays one background track at a time, with a
/// random delay between tracks, switching groups when the music type changes
/// (menu vs. in-world / dimension / creative). It emits [`MusicCommand`]s the
/// host applies to the real [`SoundManager`], and polls a `playing` flag the
/// host reports back so it knows when a track has finished.
struct MusicTicker {
    /// The type of the track currently scheduled/playing, or `None` when idle
    /// between tracks (waiting out `delay_ticks`).
    current: Option<MusicType>,
    /// Ticks remaining before the next track plays (only meaningful while
    /// `current` is `None`). `i32::MAX` parks the ticker while a track plays.
    delay_ticks: i32,
    /// Whether the host reports a music track is currently audible. Mirrors
    /// vanilla `isSoundPlaying(currentMusic)`.
    playing: bool,
    /// Deterministic xorshift RNG for the inter-track delays (no `rand` dep).
    rng: u32,
    /// Queued commands for the host to apply this frame.
    commands: Vec<MusicCommand>,
}

impl MusicTicker {
    fn new() -> Self {
        Self {
            current: None,
            // Vanilla seeds `timeUntilNextMusic = 100`.
            delay_ticks: 100,
            playing: false,
            rng: 0x9e37_79b9,
            commands: Vec::new(),
        }
    }

    fn next_rng(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// A random value in `min..=max` (inclusive), like vanilla `rand.nextInt`.
    fn rand_range(&mut self, min: i32, max: i32) -> i32 {
        if max <= min {
            return min;
        }
        let span = (max - min + 1) as u32;
        min + (self.next_rng() % span) as i32
    }

    /// Advance one 20 Hz tick against the desired `music_type`, emitting play /
    /// stop commands. `self.playing` must be kept in sync by the host before the
    /// call (it reports whether the last-started track is still audible).
    ///
    /// Mirrors vanilla `MusicTicker.update`:
    ///  1. If a different type is now wanted, stop the current track and shorten
    ///     the delay to `random(0, minDelay/2)`.
    ///  2. If the current track has finished, clear it and pick a fresh delay
    ///     `min(random(min, max), delay)`.
    ///  3. When idle and the delay expires, play the wanted type's event.
    fn tick(&mut self, wanted: MusicType) {
        // (1) A change in music type stops the current track early.
        if let Some(cur) = self.current {
            if cur != wanted {
                self.commands.push(MusicCommand::Stop);
                self.playing = false;
                self.current = None;
                let (min, _) = wanted.delay_range();
                self.delay_ticks = self.rand_range(0, (min / 2).max(0));
            }
        }
        // (2) The current track finished on its own.
        if self.current.is_some() && !self.playing {
            self.current = None;
            let (min, max) = wanted.delay_range();
            let roll = self.rand_range(min, max);
            self.delay_ticks = roll.min(self.delay_ticks);
        }
        // (3) Idle: count down and start the next track when the delay expires.
        if self.current.is_none() {
            self.delay_ticks -= 1;
            if self.delay_ticks <= 0 {
                self.commands.push(MusicCommand::Play(wanted.event().to_string()));
                self.current = Some(wanted);
                self.playing = true;
                // Park until the host reports the track has finished; a change
                // of type or a `playing=false` report reschedules the delay.
                self.delay_ticks = i32::MAX;
            }
        }
    }
}

/// An entity-attached positioned sound (minecart, moving mob, …) synced by the
/// tick loop: its host-side moving-emitter id, the entity it follows, and the
/// base volume/pitch and loop flag it was spawned with.
struct EntitySound {
    /// The id passed to `SoundManager::attach_moving_sound` / `update_moving_sound`.
    sound_id: u64,
    entity: EntityId,
    base_volume: f32,
}

/// A host-side moving-emitter command drained by `main.rs` each frame.
#[derive(Debug, Clone)]
pub enum MovingSoundCommand {
    /// Spawn a looping/one-shot positioned emitter with the given id.
    Attach {
        id: u64,
        event: String,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        looping: bool,
    },
    /// Move/re-gain an existing emitter.
    Update {
        id: u64,
        pos: Vec3,
        volume: f32,
        pitch: f32,
    },
    /// Stop and forget an emitter (emitter died / left range).
    Stop { id: u64 },
}

pub struct GameState {
    pub world: World,
    pub input: InputState,
    pub camera: Camera,
    player: EntityState,
    previous_player_position: DVec3,
    physics: PlayerPhysics,
    has_sky_light: bool,
    /// Rain/thunder state and its vanilla ramp. See [`Weather`].
    weather: Weather,
    /// Lightning bolts currently being drawn; each lives a few ticks.
    lightning_bolts: Vec<LightningBolt>,
    /// Bumped per strike so two bolts at the same spot get different shapes.
    lightning_seq: u32,
    /// Vanilla `EntityRenderer.rainSoundCounter`: gates how often the ambient
    /// rain sound fires relative to how many splashes landed.
    rain_sound_counter: i32,
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
    /// The local player's `generic.maxHealth` attribute (vanilla 20), from S20
    /// EntityProperties; sets the heart-row count alongside absorption.
    max_health: f32,
    /// The local player's active potion effects, keyed by effect id (S1D
    /// EntityEffect / S1E RemoveEntityEffect). The server resends them, so they
    /// are kept until removed or until a respawn, without ticking down.
    effects: std::collections::HashMap<u8, ActiveEffect>,
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
    /// Each entity's interpolated render position from the PREVIOUS rendered
    /// frame, used to compute its per-frame world movement for the TAA/DLSS
    /// motion-vector pass. Refreshed by `snapshot_entity_render_pos` after each
    /// entity-model rebuild; absent entries (new spawns) yield zero motion.
    entity_prev_render_pos: std::collections::HashMap<EntityId, DVec3>,
    dirty_chunks: HashSet<SectionPos>,
    /// Sections changed by local place/break prediction. They are submitted to
    /// the background mesher before ordinary dirty sections, but never rebuilt on
    /// the render thread.
    urgent_remesh: HashSet<SectionPos>,
    /// Columns whose GPU meshes were dropped by the render-distance safety net
    /// (their block data is kept in `world`). Re-meshed when the player walks
    /// back into range; pruned when the server unloads them. Bounds GPU memory
    /// on servers that never send ChunkUnload. See `enforce_render_distance`.
    evicted_columns: HashSet<ChunkPos>,
    /// Player chunk at the last `enforce_render_distance` pass, so the scan runs
    /// only when crossing a chunk boundary.
    last_residency_chunk: Option<ChunkPos>,
    /// Set for a built-in single-player world: the terrain generator that
    /// streams chunks around the player (`stream_local_world`). `None` for demo
    /// worlds and server sessions, which get their chunks elsewhere.
    local_worldgen: Option<WorldGen>,
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
    /// Sign block-entity text, keyed by world block position: the four lines
    /// flattened from their chat-JSON (S33 UpdateSign).
    signs: std::collections::HashMap<[i32; 3], [String; 4]>,
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
    /// Extension silent-look override (vanilla-style "pre" rotation): when set,
    /// the *server-visible* yaw/pitch in each movement packet is this instead of
    /// the camera, while the rendered view stays put. Cleared by ClearSilentRotation.
    server_look: Option<(f32, f32)>,
    /// This tick's input intents, consumed inside `tick` before the move.
    pending_actions: TickActions,
    /// Vanilla `isSprinting()`. Recomputed each tick by the `onLivingUpdate`
    /// sprint logic (start conditions then stop, in that order).
    sprinting: bool,
    /// Vanilla `sprintToggleTimer`: the 7-tick double-tap-W window.
    sprint_toggle_timer: i32,
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
    /// Background-music state machine (menu vs. in-world). Emits `MusicCommand`s
    /// the host applies to the real `SoundManager`.
    music_ticker: MusicTicker,
    /// Current dimension id (0 = overworld, -1 = nether, 1 = end), for the music
    /// selection. Set on JoinGame/Respawn; `has_sky_light` covers lighting.
    dimension: i8,
    /// Vanilla mood-sound countdown (`ambientTickCountdown`): ticks until the
    /// next `ambient.cave.cave` attempt.
    mood_tick_countdown: i32,
    /// LCG state for the vanilla mood-sound block sampler.
    mood_update_lcg: u32,
    /// Vanilla `distanceWalkedOnStepModified`: accumulated horizontal distance
    /// (×0.6) that drives the local player's footstep interval.
    distance_walked_on_step: f32,
    /// Vanilla `nextStepDistance`: the distance threshold for the next footstep.
    next_step_distance: i32,
    /// The local player's `on_ground` last tick, for the fall-landing edge.
    prev_on_ground: bool,
    /// Vanilla `fallDistance`: blocks fallen since last on the ground, for the
    /// landing sound / fall-damage sound selection.
    fall_distance: f32,
    /// Entity-attached looping sounds (minecart, …), synced each tick.
    entity_sounds: Vec<EntitySound>,
    /// Next host-side moving-emitter id to hand out.
    next_moving_sound_id: u64,
    /// Moving-emitter commands drained by the host each frame.
    moving_sound_commands: Vec<MovingSoundCommand>,
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

/// Vanilla `EntityRenderer.hurtCameraEffect` roll, in radians, for the given
/// hurt timer and partial-tick alpha. `hurt_time` counts down from `MAX_HURT`
/// (10) to 0; the tilt follows `sin((f)⁴·π)·14°` and eases back to nothing.
/// `attackedAtYaw` is always 0 here (the local EntityStatus hurt carries no
/// direction), so it reduces to a pure screen roll.
fn hurt_camera_roll(hurt_time: u8, alpha: f32) -> f32 {
    const MAX_HURT: f32 = 10.0;
    let f = hurt_time as f32 - alpha;
    if f <= 0.0 {
        return 0.0;
    }
    let f = f / MAX_HURT;
    let f = (f * f * f * f * std::f32::consts::PI).sin();
    -(f * 14.0).to_radians()
}

/// Vanilla `RendererLivingEntity` death fall-over tilt, in radians, for a
/// `death_time` (0 while alive, counting up to 20) and partial-tick alpha:
/// `sqrt(min((death_time + alpha - 1)/20 · 1.6, 1)) · 90°`. 0 until the entity
/// has been dead at least a tick.
fn death_roll_radians(death_time: u8, alpha: f32) -> f32 {
    if death_time == 0 {
        return 0.0;
    }
    let f = ((death_time as f32 + alpha - 1.0) / 20.0 * 1.6).max(0.0).sqrt().min(1.0);
    f * 90.0_f32.to_radians()
}

/// Vanilla `RenderPlayer.setModelVisibilities` arm state for a remote player:
/// 0 = empty hand, 1 = holding an item, 3 = blocking (using a sword). Drives the
/// third-person held-arm / blocking pose (`ModelBiped.heldItemRight`).
fn held_item_right_state(item: Option<&SlotItem>, using_item: bool) -> u8 {
    match item {
        None => 0,
        Some(it) if using_item && item_use_action(it.id, it.damage) == ItemUseAction::Block => 3,
        Some(_) => 1,
    }
}

/// Whether a held item id is a placeable block (`ItemBlock`), used to decide if
/// a right-click on a block was consumed by placing (vanilla `onItemUse`). Block
/// items share the block id range 1..256.
/// Whether two yaws point meaningfully different ways (so a silent look needs
/// the strict-movement remap). Tolerates float noise / 360 wrap.
fn yaw_differs(a: f32, b: f32) -> bool {
    let d = wrap_degrees(a - b).abs();
    d > 0.01
}

/// Snap `target` onto the lattice `base + n*step` via the shortest angular turn.
/// `base` is a real (mouse-produced) rotation, so the result stays on the same
/// `origin + n*step` lattice the server already sees — keeping rotation deltas
/// integer multiples of `step`. `step <= 0` leaves the target unsnapped.
fn quantize_rotation(target: f32, base: f32, step: f32) -> f32 {
    if step <= 0.0 {
        return target;
    }
    let delta = wrap_degrees(target - base);
    base + (delta / step).round() * step
}

/// Sign of a movement input axis as -1/0/1 (vanilla `moveForward`/`moveStrafing`
/// are always one of these before scaling).
fn sign3(v: f32) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// Remap a movement input so it stays one of the 8 legal directions but, under
/// `silent_yaw`, points as close as possible to where the player intended to go
/// under `real_yaw`. Preserves the input magnitude (so the sneak/use-item speed
/// scaling is unchanged); idle stays idle. Both client and server then derive
/// the same legal input from the silent yaw, so prediction stays in sync.
fn remap_input_to_yaw(forward: f32, strafe: f32, real_yaw: f32, silent_yaw: f32) -> (f32, f32) {
    let (fp, sp) = (sign3(forward), sign3(strafe));
    if fp == 0 && sp == 0 {
        return (forward, strafe);
    }
    let magnitude = forward.abs().max(strafe.abs());
    let intended = movement_direction(fp as f32, sp as f32, real_yaw);
    let mut best = (fp, sp);
    let mut best_dot = f64::NEG_INFINITY;
    for f in [-1i32, 0, 1] {
        for s in [-1i32, 0, 1] {
            if f == 0 && s == 0 {
                continue;
            }
            let dir = movement_direction(f as f32, s as f32, silent_yaw);
            let dot = dir.x * intended.x + dir.z * intended.z;
            if dot > best_dot {
                best_dot = dot;
                best = (f, s);
            }
        }
    }
    (best.0 as f32 * magnitude, best.1 as f32 * magnitude)
}

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

    /// A built-in, server-less single-player world backed by [`WorldGen`]. The
    /// spawn area is generated up front so terrain is visible immediately;
    /// [`stream_local_world`](Self::stream_local_world) extends it as the player
    /// moves. Runs in creative (instant dig, a hotbar of blocks, flight) — the
    /// mode is deliberately move/dig/build only, with no server or survival loop.
    pub fn local_world(seed: i64, aspect: f32) -> Self {
        let generator = WorldGen::new(seed);
        let mut world = World::new();
        // Generate only a tiny spawn platform up front so the player has ground
        // to stand on immediately. Everything else is streamed in gradually by
        // `stream_local_world` and meshed on the background worker — exactly like
        // a server session. This is deliberately small: the caller meshes the
        // initial world synchronously via `upload_world`, and meshing a large
        // area in one blocking call wedges the main thread (and, in a debug
        // build, can hang the whole machine).
        const INIT_RADIUS: i32 = 1;
        for cz in -INIT_RADIUS..=INIT_RADIUS {
            for cx in -INIT_RADIUS..=INIT_RADIUS {
                // The spawn platform is meshed synchronously by `upload_world`
                // right after this, so the relit sections need no dirty marking.
                let _ = generator.generate_chunk(&mut world, cx, cz);
            }
        }
        let spawn_h = generator.height(0, 0);
        let spawn = DVec3::new(0.5, spawn_h as f64 + 1.0, 0.5);
        let mut state = Self::new(world, EntityId(0), spawn, aspect);
        state.local_worldgen = Some(generator);
        // Move/dig/build only: creative gives instant break, reach, and flight,
        // and skips the survival/hunger/damage loops the mode doesn't want.
        state.creative = true;
        state.capabilities.creative = true;
        state.capabilities.allow_flying = true;
        // Stock the hotbar with common placeable blocks so building works out of
        // the box: stone, dirt, grass, cobblestone, planks, log, leaves, glass,
        // glowstone.
        let hotbar = [1i16, 3, 2, 4, 5, 17, 18, 20, 89];
        for (i, id) in hotbar.into_iter().enumerate() {
            state.inventory[36 + i] = Some(SlotItem::new(id, 64, 0));
        }
        state
    }

    /// Single-player chunk streaming: generate any ungenerated columns within
    /// `render_distance` of the player and drop columns that drifted far past it
    /// (their block data — GPU meshes are freed via the returned sections).
    /// Newly generated columns are marked dirty so the mesher picks them up. A
    /// no-op unless this is a generated single-player world.
    pub fn stream_local_world(&mut self, render_distance: u32) -> Vec<SectionPos> {
        // `WorldGen` is `Copy`, so lifting it out releases the borrow on `self`
        // and lets us mutate `self.world` below.
        let Some(generator) = self.local_worldgen else {
            return Vec::new();
        };
        let px = (self.player.position.x.floor() as i32).div_euclid(16);
        let pz = (self.player.position.z.floor() as i32).div_euclid(16);
        let r = render_distance as i32;

        // Generate at most a few new columns per call, nearest-first, and only
        // while the mesh backlog is small. This is the critical backpressure: the
        // main loop drains `MESH_SUBMITS_PER_FRAME` (40) dirty sections/frame into
        // the background mesher, so generating faster than that just piles up
        // multi-MB chunk snapshots in the mesher queue until memory is exhausted.
        // Gate on the backlog and mark only the column's non-empty sections (~5,
        // not all 16) so what we add each frame stays within the drain rate.
        const GEN_BUDGET: usize = 12;
        const BACKLOG_LIMIT: usize = 80;
        if self.dirty_chunks.len() < BACKLOG_LIMIT {
            let mut missing: Vec<ChunkPos> = Vec::new();
            for dz in -r..=r {
                for dx in -r..=r {
                    let pos = ChunkPos::new(px + dx, pz + dz);
                    if self.world.chunk(pos).is_none() {
                        missing.push(pos);
                    }
                }
            }
            missing.sort_by_key(|p| (p.x - px).pow(2) + (p.z - pz).pow(2));
            for pos in missing.into_iter().take(GEN_BUDGET) {
                // Sky/block light from the new column bleeds into the neighbours
                // that already exist, so their meshes go stale too — mark exactly
                // the sections the flood touched.
                let relit = generator.generate_chunk(&mut self.world, pos.x, pos.z);
                self.dirty_chunks.extend(relit);
                // Only the sections that actually got blocks exist; marking the
                // empty air sections above would just churn the mesher for nothing.
                let ys: Vec<i32> = self
                    .world
                    .chunk(pos)
                    .map(|c| c.sections().map(|s| s.y()).collect())
                    .unwrap_or_default();
                for sy in ys {
                    self.dirty_chunks.insert(SectionPos::new(pos.x, sy, pos.z));
                }
            }
        }

        // Unload far columns to bound memory as the player explores; they are
        // regenerated deterministically on return. Kept just past the
        // render-distance GPU-eviction margin (RESIDENCY_MARGIN = 2) so block
        // data stays a close, bounded superset of what's on screen.
        let keep = r + 3;
        let far: Vec<ChunkPos> = self
            .world
            .chunks()
            .map(|c| c.position)
            .filter(|p| (p.x - px).abs() > keep || (p.z - pz).abs() > keep)
            .collect();
        let mut removed = Vec::new();
        for p in far {
            self.world.remove_chunk(p);
            self.dirty_chunks.retain(|s| s.x != p.x || s.z != p.z);
            for sy in 0..16 {
                removed.push(SectionPos::new(p.x, sy, p.z));
            }
        }
        removed
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
            weather: Weather::default(),
            lightning_bolts: Vec::new(),
            lightning_seq: 0,
            rain_sound_counter: 0,
            world_time: 6000,
            daylight_cycle: true,
            time_rate: 1,
            joined_game: false,
            position_synced: false,
            pending_confirm: false,
            freeze_movement_after_teleport: false,
            needs_respawn: false,
            health: 20.0,
            max_health: 20.0,
            effects: std::collections::HashMap::new(),
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
            entity_prev_render_pos: std::collections::HashMap::new(),
            dirty_chunks: HashSet::new(),
            urgent_remesh: HashSet::new(),
            evicted_columns: HashSet::new(),
            last_residency_chunk: None,
            local_worldgen: None,
            pending_block_changes: std::collections::HashMap::new(),
            chest_lid_angles: std::collections::HashMap::new(),
            chest_open_targets: std::collections::HashMap::new(),
            signs: std::collections::HashMap::new(),
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
            server_look: None,
            pending_actions: TickActions::default(),
            sprinting: false,
            sprint_toggle_timer: 0,
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
            music_ticker: MusicTicker::new(),
            dimension: 0,
            // Vanilla seeds `ambientTickCountdown = rand.nextInt(12000)` at world
            // load; a fixed-but-nonzero seed keeps the first cave sound minutes out.
            mood_tick_countdown: 0x1357 % 12000,
            mood_update_lcg: 0x1234_5678,
            distance_walked_on_step: 0.0,
            next_step_distance: 1,
            prev_on_ground: true,
            fall_distance: 0.0,
            entity_sounds: Vec::new(),
            next_moving_sound_id: 1,
            moving_sound_commands: Vec::new(),
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

    /// Whether the sneak key is currently held (extension read-view).
    pub fn player_sneaking(&self) -> bool {
        self.input.sneak
    }

    /// Whether the local player is sprinting (extension read-view).
    pub fn player_sprinting(&self) -> bool {
        self.sprinting
    }

    /// Heart/hunger HUD animation state (vanilla `renderPlayerStats`): the live
    /// food saturation plus the tick counters that drive the heart-shake RNG and
    /// the heart-row blink/highlight.
    pub fn hud_vitals(&self) -> crate::gui::ingame::HudVitals {
        // Absorption hearts come from the Absorption effect (vanilla
        // PotionAbsorption): 4 * (amplifier + 1) health points.
        let absorption = self
            .effects
            .get(&POTION_ABSORPTION)
            .map_or(0.0, |e| 4.0 * (e.amplifier as f32 + 1.0));
        crate::gui::ingame::HudVitals {
            saturation: self.saturation,
            max_health: self.max_health,
            absorption,
            update_counter: self.hud_update_counter,
            health_update_counter: self.health_update_counter,
            last_player_health: self.last_player_health,
            regen: self.effects.contains_key(&POTION_REGENERATION),
            hunger_effect: self.effects.contains_key(&POTION_HUNGER),
            poison: self.effects.contains_key(&POTION_POISON),
            wither: self.effects.contains_key(&POTION_WITHER),
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
            // Local weather control. A server drives weather over S2B, but demo
            // and single-player worlds have no server, so this is the only way to
            // see rain/snow/thunder there.
            ["weather", kind] => match *kind {
                "clear" => {
                    self.weather.set_force_snow(false);
                    self.weather.set_raining(false);
                    "Weather set to clear".to_owned()
                }
                "rain" => {
                    self.weather.set_force_snow(false);
                    self.weather.set_raining(true);
                    "Weather set to rain".to_owned()
                }
                "snow" => {
                    self.weather.set_force_snow(true);
                    "Weather set to snow".to_owned()
                }
                "thunder" => {
                    self.weather.set_thundering(true);
                    "Weather set to thunder".to_owned()
                }
                // Lightning normally arrives from the server as S2C
                // SpawnGlobalEntity, so without one there is no way to see a
                // bolt at all. Strike ahead of the player so it lands in view.
                "strike" => {
                    // Reuse the camera's own forward vector rather than
                    // re-deriving Minecraft's yaw convention here — flattened to
                    // the horizontal so looking up or down does not move the
                    // strike closer.
                    let eye = self.camera.position;
                    let dir = self.camera.direction();
                    let flat = glam::Vec2::new(dir.x, dir.z).normalize_or_zero();
                    let (x, z) = (
                        (eye.x + flat.x * 12.0).floor() as i32,
                        (eye.z + flat.y * 12.0).floor() as i32,
                    );
                    // An unloaded column has no surface to strike; scanning it
                    // would return 0 and drop the bolt at bedrock, far below the
                    // camera and invisible.
                    let y = if self.world.is_block_column_loaded(x, z) {
                        self.precipitation_height(x, z)
                    } else {
                        eye.y.floor() as i32
                    };
                    self.handle_lightning_bolt(x as f64 + 0.5, y as f64, z as f64 + 0.5);
                    format!("Lightning strikes at {x}, {y}, {z}")
                }
                _ => "Usage: /weather clear|rain|snow|thunder|strike".to_owned(),
            },
            ["weather", ..] => "Usage: /weather clear|rain|snow|thunder|strike".to_owned(),
            _ => format!("Unknown command: /{cmd}"),
        }
    }

    /// Current weather, for the renderer's sky/fog/curtain.
    pub fn weather(&self) -> &Weather {
        &self.weather
    }

    /// Columns of precipitation to draw around the camera this frame, in
    /// vanilla's `renderRainSnow` layout: a square of columns within `radius`,
    /// each running from its precipitation height up through the camera band.
    ///
    /// Returns nothing at all when it is not raining, so a clear sky costs one
    /// branch rather than a world scan.
    pub fn precipitation_columns(
        &self,
        tick_alpha: f32,
        radius: i32,
    ) -> Vec<fpsmaster_render::weather::PrecipColumn> {
        if self.weather.rain_strength(tick_alpha) <= 0.0 || !self.has_sky_light {
            return Vec::new();
        }
        let eye = self.camera.position;
        let (cx, cy, cz) = (
            eye.x.floor() as i32,
            eye.y.floor() as i32,
            eye.z.floor() as i32,
        );
        let mut columns = Vec::new();
        for z in (cz - radius)..=(cz + radius) {
            for x in (cx - radius)..=(cx + radius) {
                if !self.world.is_block_column_loaded(x, z) {
                    continue;
                }
                let ground = self.precipitation_height(x, z);
                let biome = self.world.biome_at(x, z);
                let Some(natural_snow) =
                    fpsmaster_render::weather::column_precipitation(biome, ground)
                else {
                    continue;
                };
                let snow = natural_snow || self.weather.force_snow();
                // Vanilla clamps the camera-centred band up to the ground, so the
                // curtain never draws below the surface.
                let y_min = (cy - radius).max(ground);
                let y_max = (cy + radius).max(ground);
                if y_min >= y_max {
                    continue;
                }
                let (block_l, sky_l) = self.world.light_at(x, y_min, z);
                columns.push(fpsmaster_render::weather::PrecipColumn {
                    x,
                    z,
                    y_min,
                    y_max,
                    snow,
                    light: [sky_l as f32 / 15.0, block_l as f32 / 15.0],
                });
            }
        }
        columns
    }

    /// Vanilla `EntityRenderer.addRainParticles`: splash droplets on whatever
    /// surface the rain is landing on, near the player.
    ///
    /// The count is `100 * strength²` per tick, so it ramps in quadratically
    /// with the storm and costs nothing in light drizzle. Vanilla additionally
    /// halves it on Fast graphics; here that is left to `ParticleSystem`'s
    /// `density`, which the host already drives from the graphics settings —
    /// applying it in both places would scale it twice.
    ///
    /// Only rain splashes: snow columns and dry biomes are skipped, and lava
    /// gets smoke instead of a droplet.
    fn spawn_rain_particles(&mut self) {
        let strength = self.weather.rain_strength(1.0);
        if strength <= 0.0 || !self.has_sky_light {
            return;
        }
        let count = (100.0 * strength * strength) as i32;
        if count <= 0 {
            return;
        }
        const SPREAD: i32 = 10;
        let mut landed = 0i32;
        let mut last_splash = None;
        let eye = self.camera.position;
        let (ex, ey, ez) = (
            eye.x.floor() as i32,
            eye.y.floor() as i32,
            eye.z.floor() as i32,
        );
        for i in 0..count {
            let h = hash2d(ex.wrapping_add(i), ez, self.hud_update_counter as u32 ^ 0x5BD1);
            // Vanilla picks the column as `nextInt(10) - nextInt(10)`: a
            // TRIANGULAR distribution peaked at the player, not a uniform square.
            // Sampling uniformly over ±10 (as this first did) spreads the same
            // number of splashes evenly over 441 columns instead of concentrating
            // them around your feet, so the rain reads as sparse and even.
            let dx = (h % SPREAD as u32) as i32 - ((h >> 5) % SPREAD as u32) as i32;
            let dz = ((h >> 10) % SPREAD as u32) as i32 - ((h >> 15) % SPREAD as u32) as i32;
            let (x, z) = (ex + dx, ez + dz);
            if !self.world.is_block_column_loaded(x, z) {
                continue;
            }
            let top = self.precipitation_height(x, z);
            // Vanilla only splashes within ±10 of the player's own height, so
            // rain on a distant mountain does not spray at your feet.
            if top > ey + SPREAD || top < ey - SPREAD {
                continue;
            }
            let biome = self.world.biome_at(x, z);
            if self.weather.force_snow()
                || fpsmaster_render::weather::column_precipitation(biome, top) != Some(false)
            {
                continue; // dry biome, or snowing — no splash
            }
            let below = self.world.block_at(x, top - 1, z);
            if below.is_air() {
                continue;
            }
            let jitter = |bits: u32| ((h >> bits) & 0xFF) as f32 / 255.0;
            let pos = Vec3::new(
                x as f32 + jitter(20),
                top as f32 + 0.1,
                z as f32 + jitter(24),
            );
            // Lava hisses instead of splashing (vanilla spawns SMOKE_NORMAL).
            if matches!(below.id, 10 | 11) {
                self.particles.spawn(11, pos, Vec3::ZERO, 0.0, 1, &[]);
            } else {
                // The surface it landed on is its floor, so it can die on contact.
                self.particles.spawn_rain_splash(pos, top as f32);
            }
            landed += 1;
            last_splash = Some(pos);
        }

        // Vanilla plays the ambient loop at one of the splash points, gated so it
        // fires more often the more rain is actually landing near you.
        if let Some(pos) = last_splash {
            self.rain_sound_counter += 1;
            if landed > 0 && (hash2d(landed, self.hud_update_counter as i32, 0x9E3D) % 3) < self.rain_sound_counter as u32 {
                self.rain_sound_counter = 0;
                // Rain heard from under a roof is quieter and pitched down.
                let (volume, pitch) = if pos.y > eye.y + 1.0 {
                    (0.1, 0.5)
                } else {
                    (0.2, 1.0)
                };
                self.queue_sound("ambient.weather.rain", pos, volume, pitch);
            }
        }
    }

    /// Free-running tick counter plus the frame fraction, driving the curtain's
    /// scroll phase only (vanilla's `rendererUpdateCount + partialTicks`).
    pub fn weather_animation_time(&self, tick_alpha: f32) -> f32 {
        self.hud_update_counter as f32 + tick_alpha
    }

    /// Vanilla `World.getPrecipitationHeight`: one above the topmost block that
    /// blocks movement or is a liquid — i.e. the surface rain lands on.
    ///
    /// Called for every column of the rain curtain every frame (441 of them at
    /// the Fancy radius) and again per splash particle, so the inner loop
    /// matters. Two things keep it cheap:
    ///
    /// * The owning chunk is resolved ONCE. Going through `World::block_at` per
    ///   cell re-hashes the chunk map for all ~180 steps of the descent, which
    ///   measured at 0.94 ms/frame — about 80k hash lookups.
    /// * The scan starts at the top of the highest ALLOCATED section rather than
    ///   y=255. Air above the terrain has no section at all, so those steps are
    ///   pure waste.
    ///
    /// Together: 0.94 ms/frame -> 0.02 ms.
    fn precipitation_height(&self, x: i32, z: i32) -> i32 {
        let pos = ChunkPos::new(x.div_euclid(16), z.div_euclid(16));
        let Some(chunk) = self.world.chunk(pos) else {
            return 0;
        };
        let Some(top_section) = chunk.sections().map(|s| s.y()).max() else {
            return 0;
        };
        let (lx, lz) = (x.rem_euclid(16) as u8, z.rem_euclid(16) as u8);
        let mut y = top_section * 16 + 15;
        while y >= 0 {
            if !chunk.get_block(lx, y, lz).is_air() {
                return y + 1;
            }
            y -= 1;
        }
        0
    }

    /// S2B ChangeGameState. Only the weather reasons are acted on here; the
    /// others (game-mode change, credits, demo prompts) are handled elsewhere or
    /// not at all, and are ignored rather than treated as errors.
    fn handle_change_game_state(&mut self, reason: u8, value: f32) -> bool {
        match reason {
            1 => self.weather.set_raining(false),
            2 => self.weather.set_raining(true),
            // 7/8 set the level outright instead of letting the client ramp.
            7 => self.weather.set_rain_level(value),
            8 => self.weather.set_thunder_level(value),
            _ => {}
        }
        false
    }

    /// S2C SpawnGlobalEntity with kind 1: a lightning bolt. Vanilla lights the
    /// whole sky for two ticks (`World.lastLightningBolt`) regardless of where
    /// the bolt struck.
    fn handle_lightning_bolt(&mut self, x: f64, y: f64, z: f64) {
        self.weather.flash();
        self.lightning_seq = self.lightning_seq.wrapping_add(1);
        let seed = hash2d(x.floor() as i32, z.floor() as i32, self.lightning_seq);
        self.lightning_bolts.push(LightningBolt::new(x, y, z, seed));
        self.queue_sound(
            "ambient.weather.thunder",
            Vec3::new(x as f32, y as f32, z as f32),
            10000.0,
            0.8,
        );
    }

    /// Bolts still being drawn, for the renderer.
    pub fn lightning_bolts(&self) -> &[LightningBolt] {
        &self.lightning_bolts
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

    /// The local player's abilities (extension read-view).
    pub fn capabilities(&self) -> PlayerCapabilities {
        self.capabilities
    }

    /// Active potion effects as `(id, amplifier, duration)` (extension read-view).
    pub fn active_effects(&self) -> Vec<(u8, i8, i32)> {
        self.effects
            .iter()
            .map(|(id, e)| (*id, e.amplifier, e.duration))
            .collect()
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

    /// Drain this frame's background-music commands (start/stop). The host
    /// applies them to the real `SoundManager` and reports the resulting play
    /// state back via [`set_music_playing`](Self::set_music_playing).
    pub fn take_music_commands(&mut self) -> Vec<MusicCommand> {
        std::mem::take(&mut self.music_ticker.commands)
    }

    /// Advance ONLY the background-music ticker for one client tick, without any
    /// world/physics simulation. The host calls this while no world is active
    /// (the title screen) so menu music plays there too — vanilla runs the
    /// `MusicTicker` from `Minecraft.runTick` regardless of world state. With no
    /// world joined, [`desired_music_type`](Self::desired_music_type) resolves
    /// to [`MusicType::Menu`]. Commands are drained via `take_music_commands`.
    pub fn tick_menu_music(&mut self) {
        let music_type = self.desired_music_type();
        self.music_ticker.tick(music_type);
    }

    /// Report whether a music track is currently audible (from
    /// `SoundManager::is_music_playing`). The `MusicTicker` uses this to detect
    /// when the current track has finished so it can schedule the next one.
    pub fn set_music_playing(&mut self, playing: bool) {
        // Only clear the flag: the ticker sets `playing = true` the tick it asks
        // the host to start a track, and the host's report may lag a frame while
        // the ogg decodes. A `false` report means the track has truly ended.
        if !playing {
            self.music_ticker.playing = false;
        }
    }

    /// Drain this frame's entity-attached moving-emitter commands (attach /
    /// update / stop). The host applies them to the real `SoundManager`.
    pub fn take_moving_sound_commands(&mut self) -> Vec<MovingSoundCommand> {
        std::mem::take(&mut self.moving_sound_commands)
    }

    /// Spawn host-side particles for an extension `SpawnParticle` command.
    /// `type_id` is the vanilla 1.8 S2A particle id; the ext path carries no
    /// trailing VarInt args.
    pub fn ext_spawn_particle(
        &mut self,
        type_id: i32,
        pos: Vec3,
        offset: Vec3,
        speed: f32,
        count: i32,
    ) {
        self.particles.spawn(type_id, pos, offset, speed, count, &[]);
    }

    /// Set the particle spawn-count multiplier (extension `particleDensity`).
    pub fn set_particle_density(&mut self, density: f32) {
        self.particles.set_density(density);
    }

    /// Queue a positional sound for an extension `PlaySound` command (drained by
    /// the host into the audio backend like every other queued sound).
    pub fn ext_play_sound(&mut self, event: String, pos: Vec3, volume: f32, pitch: f32) {
        self.sound_queue.push(QueuedSound {
            event,
            position: Some(pos),
            volume,
            pitch,
        });
    }

    // ─── Extension actions (mod -> host -> server) ───────────────────────────

    /// Place the held item against a block face (extension PlaceBlock). Runs the
    /// vanilla `onPlayerRightClick` C08 path with a caller-supplied target/cursor,
    /// then swings.
    pub fn ext_place_block(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        face: u8,
        cursor: [u8; 3],
    ) -> Vec<ServerboundPacket> {
        let mut out = Vec::new();
        self.on_player_right_click(x, y, z, face, cursor[0], cursor[1], cursor[2], &mut out);
        self.swing_item(&mut out);
        out
    }

    /// Attack an entity by id (extension AttackEntity): vanilla C02 ATTACK + swing.
    pub fn ext_attack_entity(&mut self, id: i32) -> Vec<ServerboundPacket> {
        let mut out = Vec::new();
        self.swing_item(&mut out);
        self.attack_entity(id, &mut out);
        out
    }

    /// Right-click / interact with an entity (extension InteractEntity). `at` is
    /// the hit point for the InteractAt variant (vanilla sends InteractAt then
    /// Interact).
    pub fn ext_interact_entity(&mut self, id: i32, at: Option<[f32; 3]>) -> Vec<ServerboundPacket> {
        let mut out = Vec::new();
        self.sync_current_play_item(&mut out);
        if let Some([x, y, z]) = at {
            out.push(ServerboundPacket::UseEntity {
                target: id,
                kind: UseEntityKind::InteractAt { x, y, z },
            });
            self.sync_current_play_item(&mut out);
        }
        out.push(ServerboundPacket::UseEntity {
            target: id,
            kind: UseEntityKind::Interact,
        });
        out
    }

    /// Use the held item with no target (extension UseItem): vanilla `sendUseItem`.
    pub fn ext_use_item(&mut self) -> Vec<ServerboundPacket> {
        let mut out = Vec::new();
        self.send_use_item(&mut out);
        out
    }

    /// Swing the arm (extension SwingArm): drive the animation and send C0A.
    pub fn ext_swing(&mut self) -> Vec<ServerboundPacket> {
        let mut out = Vec::new();
        self.swing_item(&mut out);
        out
    }

    /// Select a hotbar slot 0..8 (extension SelectSlot): update the local
    /// selection and send C09 HeldItemChange.
    pub fn ext_select_slot(&mut self, slot: i32) -> Vec<ServerboundPacket> {
        let slot = slot.clamp(0, 8);
        self.selected_slot = slot;
        self.current_player_item = slot;
        vec![ServerboundPacket::HeldItemChange { slot: slot as i16 }]
    }

    /// Set the player's look (extension SetRotation). `silent` keeps the camera
    /// where it is and only overrides the server-visible rotation on the next
    /// movement packet (vanilla-style "pre" rotation); otherwise it turns the
    /// camera too and clears any silent override. `step` is the mouse-rotation
    /// quantum (the sensitivity factor) the silent look is snapped to.
    pub fn ext_set_rotation(&mut self, yaw: f32, pitch: f32, silent: bool, step: f32) {
        if silent {
            // Snap the look to the player's mouse-rotation lattice (multiples of
            // `step`, measured from the real camera). Real rotations are
            // `origin + n*step` (mouseDelta * sensitivity), so keeping the silent
            // look on the same lattice keeps every server-visible rotation delta
            // an integer multiple of `step` — what Grim's rotation-GCD
            // (AimModulo360) check verifies. The residual aim error is ≤ step/2,
            // far inside the block-face tolerance.
            let qyaw = quantize_rotation(yaw, self.player.yaw, step);
            let qpitch = quantize_rotation(pitch.clamp(-89.0, 89.0), self.player.pitch, step)
                .clamp(-90.0, 90.0);
            self.server_look = Some((qyaw, qpitch));
        } else {
            self.server_look = None;
            self.debug_set_look(yaw, pitch);
        }
    }

    /// Clear a silent-look override (extension ClearSilentRotation): resume
    /// sending the real camera rotation.
    pub fn ext_clear_silent_rotation(&mut self) {
        self.server_look = None;
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

    /// Attach a looping positioned sound to an entity, tracked and synced each
    /// tick (position/gain follow the entity, stopped when it dies or leaves
    /// range). Returns the host-side emitter id. Idempotent per entity: a second
    /// call for the same entity is ignored so we never double-attach.
    fn attach_entity_sound(&mut self, entity: EntityId, event: &str, volume: f32, looping: bool) {
        if self.entity_sounds.iter().any(|s| s.entity == entity) {
            return;
        }
        // The entity must already be in the world (spawn packet processed) so we
        // have an initial position to bake the emitter's gain/pan from.
        let Some(pos) = self.world.entity(entity).map(|e| to_render_vec3(e.position)) else {
            return;
        };
        let sound_id = self.next_moving_sound_id;
        self.next_moving_sound_id += 1;
        self.entity_sounds.push(EntitySound {
            sound_id,
            entity,
            base_volume: volume,
        });
        self.moving_sound_commands.push(MovingSoundCommand::Attach {
            id: sound_id,
            event: event.to_string(),
            pos,
            // Start near-silent; `sync_entity_sounds` ramps it with speed next tick.
            volume: 0.001,
            pitch: 1.0,
            looping,
        });
    }

    /// Stop and forget an entity-attached sound (emitter died / despawned).
    fn stop_entity_sound(&mut self, entity: EntityId) {
        if let Some(pos) = self.entity_sounds.iter().position(|s| s.entity == entity) {
            let s = self.entity_sounds.remove(pos);
            self.moving_sound_commands
                .push(MovingSoundCommand::Stop { id: s.sound_id });
        }
    }

    /// Sync every entity-attached sound to its entity's current position and
    /// speed each tick: minecart-style volume ramps with horizontal speed
    /// (vanilla `MovingSoundMinecart`), and a vanished entity stops its sound.
    fn sync_entity_sounds(&mut self) {
        let mut dead: Vec<EntityId> = Vec::new();
        let mut updates: Vec<MovingSoundCommand> = Vec::new();
        for s in &mut self.entity_sounds {
            let Some(entity) = self.world.entity(s.entity) else {
                dead.push(s.entity);
                continue;
            };
            let pos = to_render_vec3(entity.position);
            // Vanilla minecart: volume ramps 0..0.7 with horizontal speed; a
            // stationary cart is (almost) silent. General moving emitters reuse
            // this so a parked entity fades out.
            let speed =
                (entity.velocity.x * entity.velocity.x + entity.velocity.z * entity.velocity.z)
                    .sqrt() as f32;
            let volume = if speed >= 0.01 {
                s.base_volume * (speed / 0.5).clamp(0.0, 1.0)
            } else {
                0.0
            };
            updates.push(MovingSoundCommand::Update {
                id: s.sound_id,
                pos,
                // Keep a hair above zero so the emitter isn't culled when parked;
                // the host still attenuates it by distance.
                volume: volume.max(0.001),
                pitch: 1.0,
            });
        }
        self.moving_sound_commands.extend(updates);
        for id in dead {
            self.stop_entity_sound(id);
        }
    }

    /// Attach a looping rolling-sound to every minecart we don't already track
    /// (vanilla `MovingSoundMinecart`, event `minecart.base`). Kept general: any
    /// entity kind that vanilla gives a moving sound can be added to the match.
    /// Sounds detach automatically in [`sync_entity_sounds`] when the entity
    /// despawns.
    fn refresh_entity_sound_targets(&mut self) {
        // Vanilla SpawnObject minecart types: 10 (rideable) and the furnace/
        // chest/etc. variants share the `minecart.base` rolling sound.
        let carts: Vec<EntityId> = self
            .world
            .entities()
            .filter(|e| matches!(e.kind, EntityKind::Object(10)))
            .filter(|e| !self.entity_sounds.iter().any(|s| s.entity == e.id))
            .map(|e| e.id)
            .collect();
        for id in carts {
            self.attach_entity_sound(id, "minecart.base", 0.7, true);
        }
    }

    /// Vanilla `World.playMoodSoundAndCheckLight` (MCP-919 World.java:2637): once
    /// the mood countdown hits zero, LCG-sample a block near the player and, if
    /// it is a dark air pocket a few blocks away, play `ambient.cave.cave` and
    /// rearm the 5–15 minute cooldown.
    fn tick_ambient_mood(&mut self) {
        // Sky-less dimensions still get cave sounds in vanilla, but the trigger
        // needs a loaded column around the player; skip until we've joined.
        if !self.joined_game {
            return;
        }
        if self.mood_tick_countdown > 0 {
            self.mood_tick_countdown -= 1;
            return;
        }
        // Fire: sample one candidate block in the player's chunk via the LCG.
        self.mood_update_lcg = self
            .mood_update_lcg
            .wrapping_mul(3)
            .wrapping_add(1013904223);
        let i = self.mood_update_lcg >> 2;
        let jx = (i & 15) as i32;
        let kz = ((i >> 8) & 15) as i32;
        let ly = ((i >> 16) & 255) as i32;

        let player = self.player.position;
        let chunk_x = (player.x.floor() as i32) >> 4;
        let chunk_z = (player.z.floor() as i32) >> 4;
        let wx = chunk_x * 16 + jx;
        let wz = chunk_z * 16 + kz;

        // Rearm even when the candidate fails the checks, matching vanilla (the
        // countdown is only re-rolled after a successful play; a failed sample
        // just leaves the countdown at 0 so the next tick tries again). Vanilla
        // keeps countdown at 0 and retries next tick until a block qualifies.
        if !self.world.is_block_column_loaded(wx, wz) {
            return;
        }
        let block = self.world.block_at(wx, ly, wz);
        if !block.is_air() {
            return;
        }
        let (block_light, sky_light) = self.world.light_at(wx, ly, wz);
        // Dark air: block light <= rand(0..8) AND no sky light (deep underground).
        let rand_threshold = (self.mood_update_lcg >> 20) % 8;
        if u32::from(block_light) > rand_threshold || sky_light > 0 {
            return;
        }
        // Must be >4 blocks from the player (squared > 16), so it's not underfoot.
        let sx = wx as f64 + 0.5;
        let sy = ly as f64 + 0.5;
        let sz = wz as f64 + 0.5;
        let dist_sq = (sx - player.x).powi(2)
            + (sy - (player.y + STANDING_EYE_HEIGHT)).powi(2)
            + (sz - player.z).powi(2);
        if dist_sq <= 16.0 {
            return;
        }
        // Play: volume 0.7, pitch 0.8 + rand*0.2 (range [0.8, 1.0)).
        let pitch = 0.8 + ((self.mood_update_lcg >> 8) & 0xffff) as f32 / 65536.0 * 0.2;
        self.queue_sound(
            "ambient.cave.cave",
            Vec3::new(sx as f32, sy as f32, sz as f32),
            0.7,
            pitch,
        );
        // Rearm: 6000..18000 ticks (5–15 minutes).
        self.mood_tick_countdown = 6000 + ((self.mood_update_lcg >> 24) % 12000) as i32;
    }

    /// The vanilla `Block.stepSound` event for a footstep on block `id`, mapping
    /// the `dig.<material>` families onto `step.<material>`. Snow layers, ladders
    /// and other special step surfaces fall back to their material's family.
    fn step_sound_for_block(id: u16) -> String {
        // Reuse the dig-sound material classification and swap the prefix; the
        // step.* and dig.* event families share material names in 1.8.9.
        let dig = dig_sound_for_block(id);
        let material = dig.strip_prefix("dig.").unwrap_or("stone");
        format!("step.{material}")
    }

    /// Vanilla client-side movement SFX for the local player, run each tick after
    /// the move: footsteps on the ground, swim splashes in water, and the fall
    /// landing / fall-damage sound on touchdown.
    fn tick_local_movement_sounds(&mut self) {
        let pos = self.player.position;
        let prev = self.previous_player_position;
        let on_ground = self.player.on_ground;

        // Horizontal distance walked this tick (vanilla accumulates dx,dz ×0.6,
        // excluding vertical motion).
        let dx = (pos.x - prev.x) as f32;
        let dz = (pos.z - prev.z) as f32;
        let horizontal = (dx * dx + dz * dz).sqrt();

        // Block at the player's feet (one below the standing position) and the
        // block occupied by the feet, for water detection.
        let fx = pos.x.floor() as i32;
        let fz = pos.z.floor() as i32;
        let feet_y = pos.y.floor() as i32;
        let below = self.world.block_at(fx, feet_y - 1, fz);
        let feet_block = self.world.block_at(fx, feet_y, fz);
        let in_water = feet_block.is_water();

        // Fall tracking: accumulate downward motion while airborne; on a
        // ground touchdown, play the landing/damage sound and reset.
        let dy = (pos.y - prev.y) as f32;
        if !on_ground && dy < 0.0 {
            self.fall_distance += -dy;
        }
        let landed = on_ground && !self.prev_on_ground;
        if landed {
            let fall = self.fall_distance;
            // Vanilla EntityLivingBase.fall: damage = ceil(fall - 3.0); the fall
            // sound only plays when that is positive (a hurt fall).
            let damage = (fall - 3.0).ceil();
            if damage > 0.0 {
                let listener = self.camera.position;
                let event = if damage > 4.0 {
                    "game.player.hurt.fall.big"
                } else {
                    "game.player.hurt.fall.small"
                };
                self.queue_sound(event, listener, 1.0, 1.0);
            }
            // Landing step (vanilla playStepSound at fall): quieter, lower pitch.
            if !in_water && !below.is_air() {
                let listener = self.camera.position;
                self.queue_sound(Self::step_sound_for_block(below.id), listener, 0.15, 0.75);
            }
            self.fall_distance = 0.0;
        }
        if on_ground {
            self.fall_distance = 0.0;
        }
        self.prev_on_ground = on_ground;

        // Footstep accumulation and trigger (vanilla Entity.doBlockCollisions /
        // playStepSound): only on the ground (or in water) and moving.
        self.distance_walked_on_step += horizontal * 0.6;
        if self.distance_walked_on_step > self.next_step_distance as f32 {
            self.next_step_distance += 1;
            let listener = self.camera.position;
            if in_water {
                // Swim/splash: volume from motion, random pitch.
                let motion = (dx * dx * 0.2 + dy * dy + dz * dz * 0.2).sqrt() * 0.35;
                let vol = motion.min(1.0);
                let r = self.mood_rand_float();
                let pitch = 1.0 + (r - 0.5) * 0.8; // 1.0 + rand(-0.4, 0.4)
                self.queue_sound("game.player.swim", listener, vol, pitch);
            } else if on_ground && !below.is_air() && !below.is_water() {
                // Regular footstep: block step sound (vanilla volume ×0.15).
                self.queue_sound(Self::step_sound_for_block(below.id), listener, 0.15, 1.0);
            }
        }
    }

    /// A deterministic 0..1 float from the mood LCG stream (footstep pitch etc.),
    /// so the local prediction needs no `rand` dependency.
    fn mood_rand_float(&mut self) -> f32 {
        self.mood_update_lcg = self
            .mood_update_lcg
            .wrapping_mul(1664525)
            .wrapping_add(1013904223);
        (self.mood_update_lcg >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Choose the desired music group for the current world state, matching
    /// vanilla `Minecraft.getAmbientMusicType`.
    fn desired_music_type(&self) -> MusicType {
        if !self.joined_game {
            return MusicType::Menu;
        }
        match self.dimension {
            -1 => MusicType::Nether,
            1 => {
                // Vanilla plays the dragon fight theme while a boss bar is up
                // (BossStatus active), else the ambient end theme.
                if self.boss_bar().is_some() {
                    MusicType::EndBoss
                } else {
                    MusicType::End
                }
            }
            _ => {
                if self.creative && self.capabilities.allow_flying {
                    MusicType::Creative
                } else {
                    MusicType::Game
                }
            }
        }
    }

    /// Queue a non-positional UI sound (vanilla `PositionedSoundRecord.create`,
    /// unattenuated, e.g. `gui.button.press`).
    pub fn queue_ui_sound(&mut self, event: impl Into<String>) {
        self.sound_queue.push(QueuedSound {
            event: event.into(),
            position: None,
            volume: 1.0,
            pitch: 1.0,
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

    /// Select villager trade `index` (vanilla `GuiMerchant` `MC|TrSel`): clamp +
    /// store the selection locally and return the plugin-message packet that asks
    /// the server to fill the trade slots with that recipe.
    pub fn merchant_select(&mut self, index: usize) -> Option<ServerboundPacket> {
        let container = self.open_container.as_mut()?;
        container.set_selected_trade(index);
        Some(ServerboundPacket::PluginMessage {
            channel: "MC|TrSel".to_string(),
            data: (container.selected_trade() as i32).to_be_bytes().to_vec(),
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

    /// Current world time of day in ticks (0..24000) — extension read-view.
    /// (The interpolated `world_time(alpha)` above is for the renderer's clock.)
    pub fn world_time_ticks(&self) -> i64 {
        self.world_time
    }

    /// Best-effort current dimension id for extensions. Only `has_sky_light` is
    /// persisted (set from `dimension == 0` on JoinGame/Respawn), so this returns
    /// 0 (overworld) with sky light and -1 (nether) otherwise; the End is not
    /// distinguished without a stored dimension field.
    pub fn dimension(&self) -> i32 {
        if self.has_sky_light {
            0
        } else {
            -1
        }
    }

    /// Debug harness (`--camera`): pin the player at a fixed hovering pose so a
    /// scripted run renders a deterministic viewpoint.
    pub fn debug_set_pose(&mut self, pos: DVec3, yaw: f32, pitch: f32) {
        self.player.position = pos;
        self.previous_player_position = pos;
        self.player.velocity = DVec3::ZERO;
        // The camera re-syncs from the player each tick, so pin the player look.
        self.player.yaw = yaw;
        self.player.pitch = pitch;
        self.camera.yaw = yaw;
        self.camera.pitch = pitch;
        self.capabilities.allow_flying = true;
        self.capabilities.flying = true;
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

    /// Local player velocity (vanilla `motionX/Y/Z`) — extension read-view.
    pub fn player_velocity(&self) -> DVec3 {
        self.player.velocity
    }

    /// Local player body yaw in degrees — extension read-view.
    pub fn player_yaw(&self) -> f32 {
        self.player.yaw
    }

    /// Local player pitch in degrees — extension read-view.
    pub fn player_pitch(&self) -> f32 {
        self.player.pitch
    }

    /// The local player's network entity id, so extension read-views can skip
    /// its (stale) row when iterating `world.entities()`.
    pub fn player_entity_id(&self) -> EntityId {
        self.player.id
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
                // Vanilla EntityPlayer.updateItemUse: while eating/drinking, the
                // chew/gulp sound fires every 4 ticks from tick 8 (itemInUseCount
                // <= 25 && % 4 == 0, counting down from 32). The local player's
                // playSound is client-only — the server never echoes it; only the
                // finishing `random.burp` comes back over S29. Emit at the eye
                // (listener) position; SoundManager picks the ogg variant.
                if matches!(self.use_action, ItemUseAction::Eat | ItemUseAction::Drink)
                    && self.use_item_ticks >= 8
                    && self.use_item_ticks % 4 == 0
                {
                    let h = hash2d(self.hud_update_counter as i32, self.use_item_ticks, 0x9e37);
                    let pos = self.camera.position;
                    if self.use_action == ItemUseAction::Drink {
                        let pitch = (h & 0xff) as f32 / 255.0 * 0.1 + 0.9;
                        self.queue_sound("random.drink", pos, 0.5, pitch);
                    } else {
                        let vol = if h & 1 == 0 { 0.5 } else { 1.0 };
                        let pitch = (((h >> 8) & 0xff) as f32 - ((h >> 16) & 0xff) as f32)
                            / 255.0
                            * 0.2
                            + 1.0;
                        self.queue_sound("random.eat", pos, vol, pitch);
                    }
                }
            }
        }

        // `if (isUsingItem()) { ... } else { ... }` — while an item is in use
        // (sword block), vanilla 1.8 swallows ALL clicks: no attack/dig/use
        // packet goes out (block-hitting was removed in 1.8), only the right-key
        // release stops the block. old_animations restores the 1.7 *visual* and
        // NOTHING else: a left-click starts the local arm swing (rendered layered
        // onto the block pose for the "swing + block" look) but sends no packet,
        // so the network stays vanilla-1.8 and Grim sees a plain block.
        if self.is_using_item() {
            if !a.right_held {
                self.on_stopped_using_item(&mut out);
            }
            if a.old_animations && a.attack_pressed {
                self.swing_arm();
            }
            // all click presses are drained (no packets) this tick.
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
    /// Run one 20 Hz tick (input → physics → state). Returns the serverbound
    /// interaction packets to send before the flying packet; the caller builds
    /// the movement snapshot itself via [`Self::movement_snapshot`] *after* the
    /// extension tick, so a mod's silent look rides this tick's flying packet.
    pub fn tick(&mut self, dt: f32) -> Option<Vec<ServerboundPacket>> {
        // Debug probe (`FPSMASTER_LIGHT_PROBE="x z"`): once per second, log the
        // block/skylight column at (x, z) as the client sees it.
        if self.hud_update_counter % 20 == 0 {
            if let Ok(spec) = std::env::var("FPSMASTER_LIGHT_PROBE") {
                let coords: Vec<i32> =
                    spec.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                for pair in coords.chunks(2) {
                    let [px, pz] = *pair else { continue };
                    if !self.world.is_block_column_loaded(px, pz) {
                        continue;
                    }
                    let mut col = String::new();
                    for y in (30..75).rev() {
                        let b = self.world.block_at(px, y, pz);
                        let (bl, sky) = self.world.light_at(px, y, pz);
                        let kind = if b.is_water() {
                            'W'
                        } else if b.is_air() {
                            '.'
                        } else {
                            '#'
                        };
                        col.push_str(&format!(" y{y}{kind}s{sky}b{bl}"));
                        if kind == '#' {
                            break;
                        }
                    }
                    log::warn!("[light-probe] col ({px},{pz}):{col}");
                }
            }
        }
        // Vanilla GuiIngame.updateTick: a free-running tick counter that drives the
        // heart-shake RNG and the heart/hunger blink timing.
        self.hud_update_counter = self.hud_update_counter.wrapping_add(1);
        self.title.tick();
        // Advance the day/night clock one tick (vanilla ticks world time forward
        // locally between server updates), unless daylight cycle is off.
        if self.daylight_cycle {
            self.world_time += self.time_rate;
        }
        self.weather.tick();
        self.spawn_rain_particles();
        // Age out finished bolts (vanilla's EntityLightningBolt removes itself).
        self.lightning_bolts.retain_mut(|bolt| {
            bolt.life = bolt.life.saturating_sub(1);
            bolt.life > 0
        });
        // Advance particle effects once per tick (before any early return), so
        // smoke/flame age and move at the vanilla 20 Hz.
        self.particles.tick();
        // Background music (vanilla MusicTicker): pick the group for the current
        // world state and advance the inter-track delay. Commands are drained by
        // the host, which reports the play state back via `set_music_playing`.
        let music_type = self.desired_music_type();
        self.music_ticker.tick(music_type);
        // Random cave/mood ambient sound (vanilla playMoodSoundAndCheckLight).
        self.tick_ambient_mood();
        // Sync entity-attached looping sounds (minecarts, …) to their emitters,
        // and attach one to a suitable moving entity if none is tracked yet.
        self.refresh_entity_sound_targets();
        self.sync_entity_sounds();
        // Count down the local player's hurt timer (vanilla EntityLivingBase
        // .onUpdate), fading the hurt-camera tilt over 10 ticks.
        self.player.hurt_time = self.player.hurt_time.saturating_sub(1);
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
        // Entities with no pending server correction run local physics this tick
        // (zero-latency motion): dropped items always, projectiles while they
        // still carry motion. Everything else lerps toward the server target.
        let player_id = self.player.id;
        let locally_simulated = |e: &EntityState| -> bool {
            if e.id == player_id || e.position_increments != 0 {
                return false;
            }
            match e.kind {
                EntityKind::Object(2) => true, // dropped item
                EntityKind::Object(kind) => {
                    projectile_physics(kind).is_some() && e.velocity.length_squared() > 1.0e-6
                }
                _ => false,
            }
        };
        for entity in self.world.entities_mut() {
            if entity.id != player_id && !locally_simulated(entity) {
                entity.tick_interpolation();
            }
        }
        let simulating: Vec<EntityId> = self
            .world
            .entities()
            .filter(|e| locally_simulated(e))
            .map(|e| e.id)
            .collect();
        for id in simulating {
            if let Some(mut e) = self.world.entity(id).cloned() {
                match e.kind {
                    EntityKind::Object(2) => fpsmaster_core::physics::tick_item(&self.world, &mut e),
                    EntityKind::Object(kind) => {
                        if let Some((gravity, drag, collide)) = projectile_physics(kind) {
                            fpsmaster_core::physics::tick_projectile(
                                &self.world,
                                &mut e,
                                gravity,
                                drag,
                                collide,
                            );
                        }
                    }
                    _ => {}
                }
                self.world.upsert_entity(e);
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
                return Some(actions);
            }
        }

        // Hold the player still (no physics) while:
        //  - the chunk under us hasn't arrived yet, so we don't fall through
        //    not-yet-generated terrain on join.
        let bx = self.player.position.x.floor() as i32;
        let bz = self.player.position.z.floor() as i32;
        // Sprint, a 1:1 port of `EntityPlayerSP.onLivingUpdate`. Runs before the
        // move so the reported flag matches the simulated motion. fpsmaster's
        // sprint key is a toggle, so `self.input.sprint` stands in for
        // `keyBindSprint.isKeyDown()`; the double-tap-W path (sprintToggleTimer)
        // works regardless. (`sprintingTicksLeft` and the blindness potion are
        // not modelled — neither is set in the 1.8 client / fpsmaster.)
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
        if self.player.on_ground
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
        // Runs the same tick an attack cancelled sprint, so a sprint-attack while
        // W + sprint are held re-enables sprint immediately — vanilla never sends
        // a StopSprinting in that case (final isSprinting() stays true, so
        // onUpdateWalkingPlayer emits no C0B); only the ×0.6 momentum hit lands.
        if !self.sprinting
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
            // Silent-look strict movement: when the server sees a different yaw
            // than the camera (a silent rotation), drive the move with that yaw
            // and snap the input to the legal direction nearest the player's
            // intent. A player can only move in 8 directions, so this keeps the
            // server's movement prediction in sync with the flying-packet yaw.
            if let Some((silent_yaw, _)) = self.server_look {
                if yaw_differs(silent_yaw, self.player.yaw) {
                    let (f, s) =
                        remap_input_to_yaw(input.forward, input.strafe, self.player.yaw, silent_yaw);
                    input.forward = f;
                    input.strafe = s;
                    input.move_yaw = Some(silent_yaw);
                }
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
        // Client-side movement SFX (footsteps, swim, fall landing) from this
        // tick's post-move position.
        self.tick_local_movement_sounds();
        self.world.upsert_entity(self.player.clone());
        self.advance_view_state();
        self.update_camera(1.0);
        Some(actions)
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
        self.camera.roll = hurt_camera_roll(self.player.hurt_time, alpha);
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
    #[allow(clippy::too_many_arguments)]
    pub fn build_entity_model(
        &self,
        mesh: &mut ModelMesh,
        glint: &mut ModelMesh,
        tick_alpha: f32,
        brightness: f32,
        skin_rows: &std::collections::HashMap<[u8; 16], u32>,
        max_dist_sq: f64,
        old_animations: bool,
    ) {
        mesh.clear();
        glint.clear();
        let sun_b = fpsmaster_render::sky::sun_brightness(
            self.world_time(tick_alpha),
            self.weather.rain_strength(tick_alpha),
            self.weather.thunder_strength(tick_alpha),
        );
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
            // Players lower the right arm to hold an item / raise it to block;
            // mobs keep their archetype arm pose (held_item_right = 0).
            let held_item_right = if matches!(entity.kind, EntityKind::RemotePlayer) {
                let held = self.entity_equipment.get(&entity.id).and_then(|s| s[0].as_ref());
                held_item_right_state(held, entity.using_item)
            } else {
                0
            };
            let anim = EntityAnim {
                limb_swing,
                limb_swing_amount,
                net_head_yaw,
                head_pitch: entity.render_pitch(tick_alpha),
                swing_progress: entity.render_swing(tick_alpha),
                sneaking: entity.sneaking,
                held_item_right,
                old_animations,
                death_roll: death_roll_radians(entity.death_time, tick_alpha),
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
                        // Enchanted armor pieces shimmer like enchanted items:
                        // emit those boxes into the glint mesh for the additive
                        // glint pass.
                        let enchanted: [bool; 5] = std::array::from_fn(|i| {
                            slots[i].as_ref().is_some_and(is_enchanted)
                        });
                        if enchanted[1] || enchanted[2] || enchanted[3] || enchanted[4] {
                            glint.push_armor_glint(&ids, &enchanted, &anim, feet, body_yaw);
                        }
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
            // `hurtTime > 0 || deathTime > 0` the model colour is lerped toward
            // pure red at a constant 0.3 strength (GL_INTERPOLATE with
            // (1,0,0,0.3)) — held for the whole hurt animation AND the whole
            // death animation, so the corpse stays red.
            let hurt = entity.hurt_time > 0 || entity.death_time > 0;
            for v in &mut mesh.vertices[start..] {
                v.color[0] *= factor[0];
                v.color[1] *= factor[1];
                v.color[2] *= factor[2];
                if hurt {
                    v.color[0] = v.color[0] * 0.7 + 0.3;
                    v.color[1] *= 0.7;
                    v.color[2] *= 0.7;
                }
            }
            // Tag this entity's vertices with its per-frame world movement (rigid
            // translation) for the motion-vector pass. Missing from the cache (a
            // fresh spawn) => zero motion, so it doesn't streak on its first frame.
            let cur_pos = entity.render_position(tick_alpha as f64);
            let prev_pos = self
                .entity_prev_render_pos
                .get(&entity.id)
                .copied()
                .unwrap_or(cur_pos);
            let d = to_render_vec3(cur_pos) - to_render_vec3(prev_pos);
            mesh.fill_motion([d.x, d.y, d.z, 0.0]);
        }
    }

    /// Build a player biped at `feet` / `yaw` for ray tracing ONLY (so the local player
    /// casts a shadow + shows in reflections / water). Never rasterized — fed only to the
    /// entity BLAS. Default pose (a shadow doesn't need limb-swing detail). Built from the
    /// camera each frame so it works in both the game and the render demo.
    pub fn build_player_body_at(
        &self,
        mesh: &mut ModelMesh,
        feet: Vec3,
        body_yaw: f32,
        look_yaw: f32,
        limb_swing: f32,
        limb_swing_amount: f32,
        tick_alpha: f32,
        brightness: f32,
    ) {
        mesh.clear();
        let sun_b = fpsmaster_render::sky::sun_brightness(
            self.world_time(tick_alpha),
            self.weather.rain_strength(tick_alpha),
            self.weather.thunder_strength(tick_alpha),
        );
        // Walk cycle (limb_swing) is driven by the caller from movement; swing/sneak come
        // from the player state; the head turns toward the look relative to the body yaw.
        let net_head_yaw = wrap_degrees(look_yaw - body_yaw).clamp(-75.0, 75.0);
        let anim = EntityAnim {
            limb_swing,
            limb_swing_amount,
            net_head_yaw,
            head_pitch: self.player.render_pitch(tick_alpha),
            swing_progress: self.player.render_swing(tick_alpha),
            sneaking: self.player.sneaking,
            held_item_right: 0,
            old_animations: false,
            death_roll: 0.0,
        };
        mesh.push_entity(EntityKind::RemotePlayer, feet, body_yaw, &anim, None);
        let center = Vec3::new(feet.x, feet.y + 0.9, feet.z);
        let factor = entity_light(&self.world, center, sun_b, brightness);
        for v in &mut mesh.vertices {
            v.color[0] *= factor[0];
            v.color[1] *= factor[1];
            v.color[2] *= factor[2];
        }
    }

    /// Snapshot every entity's current interpolated render position so the next
    /// frame can diff against it for motion vectors. Called after the entity
    /// model is rebuilt; `entity.render_position` is the same value the build
    /// used, so the diff is exactly one frame of movement.
    pub fn snapshot_entity_render_pos(&mut self, tick_alpha: f32) {
        let snap: Vec<(EntityId, DVec3)> = self
            .world
            .entities()
            .map(|e| (e.id, e.render_position(tick_alpha as f64)))
            .collect();
        self.entity_prev_render_pos.clear();
        self.entity_prev_render_pos.extend(snap);
    }

    /// Append the chest block-entities near the camera to the entity model.
    /// Chests no longer mesh as terrain (their `render_shape` is `None`), so the
    /// dedicated [`fpsmaster_render::ModelMesh::push_chest`] model is drawn here in
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
        let sun_b = fpsmaster_render::sky::sun_brightness(
            self.world_time(tick_alpha),
            self.weather.rain_strength(tick_alpha),
            self.weather.thunder_strength(tick_alpha),
        );
        let frustum = self.camera.frustum();
        let cam = self.camera.position;

        for (&[wx, wy, wz], block) in self.world.block_entities() {
            let kind = match block.id {
                54 => ChestKind::Normal,
                130 => ChestKind::Ender,
                146 => ChestKind::Trapped,
                _ => continue,
            };
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
            // Pairing: a chest pairs with a same-id chest on the axis
            // perpendicular to its facing (meta 2/3 face N/S → pair on X; 4/5
            // face E/W → pair on Z). Ender chests (130) are never double. The
            // canonical half (smaller X/Z, vanilla's adjacentChestXNeg/ZNeg ==
            // null) renders the large model; the other half is skipped. Single
            // chests keep push_chest.
            let axis = match (kind, block.meta) {
                (ChestKind::Ender, _) => None,
                (_, 2 | 3) => Some([1i32, 0, 0]),
                (_, 4 | 5) => Some([0i32, 0, 1]),
                _ => None,
            };
            let partner = axis.and_then(|d| {
                let pos = self.world.block_at(wx + d[0], wy + d[1], wz + d[2]);
                let neg = self.world.block_at(wx - d[0], wy - d[1], wz - d[2]);
                if neg.id == block.id {
                    // Neighbour on −axis renders the pair; skip.
                    Some(None)
                } else if pos.id == block.id {
                    // This is the canonical half; partner at +axis.
                    Some(Some([wx + d[0], wy + d[1], wz + d[2]]))
                } else {
                    None
                }
            });
            let start = mesh.vertices.len();
            match partner {
                // Other half of a pair — the canonical half draws it.
                Some(None) => continue,
                // Canonical half: one large model, lid shared (max).
                Some(Some(p)) => {
                    let partner_lid = self.chest_lid_angles.get(&p).copied().unwrap_or(0.0);
                    mesh.push_large_chest([wx, wy, wz], block.meta, lid.max(partner_lid), kind);
                }
                None => mesh.push_chest([wx, wy, wz], block.meta, lid, kind),
            }
            // Light the chest by the lightmap at its centre, like mobs.
            let factor = entity_light(
                &self.world,
                Vec3::new(cx as f32, cy as f32, cz as f32),
                sun_b,
                brightness,
            );
            for v in &mut mesh.vertices[start..] {
                v.color[0] *= factor[0];
                v.color[1] *= factor[1];
                v.color[2] *= factor[2];
            }
        }
    }

    /// Append the sign, enchanting-table-book and end-portal block-entities near
    /// the camera to the entity model, and return the sign-text draws for the
    /// renderer's world-space text pass. Mirrors [`Self::build_chest_models`]:
    /// the world's block-entity index is walked (not a per-frame voxel scan),
    /// each entry distance + frustum culled and lit by the world lightmap. The book hover
    /// is driven by the world time (the same source folded into the entity
    /// fingerprint, so the book's phase matches the rebuild cadence).
    pub fn build_block_entity_models(
        &self,
        mesh: &mut ModelMesh,
        brightness: f32,
        tick_alpha: f32,
        max_dist_sq: f64,
    ) -> Vec<SignTextDraw> {
        let time = self.world_time(tick_alpha) as f32;
        let sun_b = fpsmaster_render::sky::sun_brightness(
            self.world_time(tick_alpha),
            self.weather.rain_strength(tick_alpha),
            self.weather.thunder_strength(tick_alpha),
        );
        let frustum = self.camera.frustum();
        let cam = self.camera.position;
        let mut sign_texts = Vec::new();

        for (&[wx, wy, wz], block) in self.world.block_entities() {
            // Signs (63 standing, 68 wall), enchanting table (116) and end
            // portal (119); chests share the index but render in build_chest_models.
            if !matches!(block.id, 63 | 68 | 116 | 119) {
                continue;
            }
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
            let center = Vec3::new(cx as f32, cy as f32, cz as f32);
            let factor = entity_light(&self.world, center, sun_b, brightness);
            let start = mesh.vertices.len();
            let cell = [wx, wy, wz];
            match block.id {
                63 | 68 => {
                    let kind = if block.id == 63 {
                        SignKind::Standing
                    } else {
                        SignKind::Wall
                    };
                    mesh.push_sign(cell, block.meta, kind);
                    if let Some(lines) = self.signs.get(&cell) {
                        if lines.iter().any(|l| !l.is_empty()) {
                            let (c, right, up, hw, hh) =
                                ModelMesh::sign_text_basis(cell, block.meta, kind);
                            sign_texts.push(SignTextDraw {
                                lines: lines.clone(),
                                center: c,
                                right,
                                up,
                                half_width: hw,
                                half_height: hh,
                            });
                        }
                    }
                }
                116 => mesh.push_book(cell, time),
                119 => mesh.push_end_portal(cell),
                _ => unreachable!(),
            }
            for v in &mut mesh.vertices[start..] {
                v.color[0] *= factor[0];
                v.color[1] *= factor[1];
                v.color[2] *= factor[2];
            }
        }
        sign_texts
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

    /// World-light multiplier for a model drawn at `pos` — the same coloured
    /// vanilla lightmap the chunk shader applies, so entities and the
    /// first-person hand sit at the terrain's brightness AND tint (warm near a
    /// torch, blue at night) rather than just its overall level.
    pub fn world_light_factor(&self, pos: Vec3, brightness: f32, tick_alpha: f32) -> [f32; 3] {
        let sun_b = fpsmaster_render::sky::sun_brightness(
            self.world_time(tick_alpha),
            self.weather.rain_strength(tick_alpha),
            self.weather.thunder_strength(tick_alpha),
        );
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
    pub fn particle_billboards(&self, tick_alpha: f32) -> Vec<fpsmaster_render::ParticleBillboard> {
        self.particles.billboards(tick_alpha)
    }

    /// This frame's block-break debris (vanilla `EntityDiggingFX`), for the item
    /// renderer to billboard against the block atlas.
    pub fn block_debris(&self, tick_alpha: f32) -> Vec<crate::particle::BlockDebris> {
        self.particles.block_debris(tick_alpha)
    }

    /// This frame's experience-orb billboards (vanilla `RenderXPOrb`): a
    /// camera-facing quad sampling `experience_orb.png`, the sprite cell chosen
    /// by the orb's xp value, colour-cycling through the green/red rainbow over
    /// its age, drawn at half alpha and full brightness.
    pub fn xp_orbs(&self, tick_alpha: f32) -> Vec<fpsmaster_render::ParticleBillboard> {
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
            orbs.push(fpsmaster_render::ParticleBillboard {
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
                scale: 1.0,
                flash: 0.0,
            });
        }
        cubes
    }

    /// This frame's primed-TNT cubes (SpawnObject kind 50): a TNT block that
    /// swells and flashes white as its fuse counts down, a port of vanilla
    /// `RenderTNTPrimed`. The fuse is client-local (1.8 never syncs it): it
    /// starts at 80 and counts down with the entity's tracked age, so
    /// `fuse = 80 - age`.
    pub fn primed_tnt_cubes(&self, tick_alpha: f32) -> Vec<FallingBlock> {
        let mut cubes = Vec::new();
        for entity in self.world.entities() {
            if entity.kind != EntityKind::Object(50) {
                continue;
            }
            let fuse = (80i64 - entity.age as i64).max(0) as f32;
            // Vanilla `f = fuse - partialTicks + 1`.
            let f = fuse - tick_alpha + 1.0;
            // Last 10 fuse ticks: swell to ×1.3 with a quartic ease-in.
            let scale = if f < 10.0 {
                let t = (1.0 - f / 10.0).clamp(0.0, 1.0);
                1.0 + t * t * t * t * 0.3
            } else {
                1.0
            };
            // White flash on every other 5-tick fuse window; the overlay
            // strength grows as the fuse shortens (vanilla `(1 - f/100) * 0.8`).
            let flash = if (fuse as i64 / 5) % 2 == 0 {
                (1.0 - f / 100.0) * 0.8
            } else {
                0.0
            };
            let pos = to_render_vec3(entity.render_position(tick_alpha as f64));
            let (block_l, sky_l) =
                self.world
                    .light_at(pos.x.floor() as i32, pos.y.floor() as i32, pos.z.floor() as i32);
            cubes.push(FallingBlock {
                block: BlockState::new(46, 0),
                pos,
                light: [sky_l as f32 / 15.0, block_l as f32 / 15.0],
                scale,
                flash,
            });
        }
        cubes
    }

    /// This frame's projectile sprites: SpawnObject kinds mapped to an item id
    /// and rendered as 2D item-sprite billboards through the dropped-item path
    /// (snowball, egg, ender pearl, …). Kinds rendered elsewhere — the arrow
    /// (3D model in the entity pass), item=2, falling block=70, armor stand=78 —
    /// are skipped.
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
                held_item_right: held_item_right_state(Some(item), entity.using_item),
                old_animations,
                death_roll: death_roll_radians(entity.death_time, tick_alpha),
            };
            let attach = arm_attach(feet, body_yaw, &anim);
            // Sample the lightmap at the rendered hand (bottom-centre of the arm).
            let hand = attach.to_world(fpsmaster_render::ArmAttach::HAND_PX);
            let (block_l, sky_l) = self.world.light_at(
                hand.x.floor() as i32,
                hand.y.floor() as i32,
                hand.z.floor() as i32,
            );
            let light = [sky_l as f32 / 15.0, block_l as f32 / 15.0];
            items.push(PlayerHeldItem {
                item: item.clone(),
                attach,
                sneaking: entity.sneaking,
                light,
            });
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
        let sun_b = fpsmaster_render::sky::sun_brightness(
            self.world_time(tick_alpha),
            self.weather.rain_strength(tick_alpha),
            self.weather.thunder_strength(tick_alpha),
        );
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
            // Death animation advances every tick (and the roll per frame), so
            // fold it in or the cached model would freeze mid-fall.
            mix(qa(death_roll_radians(e.death_time, tick_alpha)));
            mix((e.using_item as u64) << 3 | (e.sneaking as u64) << 2 | (e.invisible as u64) << 1 | e.custom_name_visible as u64);
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
        // Sign text (order-independent): fold each sign's position and a hash of
        // its lines so a new/edited sign re-triggers the block-entity rebuild.
        let mut sign_acc: u64 = 0;
        for (&[x, y, z], lines) in &self.signs {
            let mut s = (x as u32 as u64) ^ ((z as u32 as u64) << 21) ^ ((y as u64) << 42);
            for line in lines {
                for b in line.as_bytes() {
                    s = (s ^ *b as u64).wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
            sign_acc ^= s;
        }
        mix(sign_acc);
        // Coarse book-hover timer (world time advances ~20/sec): quantized to a
        // few steps per second so the floating book animates without forcing a
        // per-frame rebuild. Idle scenes with a frozen sky still update slowly.
        mix((self.world_time(tick_alpha) / 4.0) as u64);
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
    /// sync held item, send C02 ATTACK (always, regardless of target), and apply
    /// the sprint hit (halve horizontal motion + cancel sprint; no knockback
    /// enchant here so it fires exactly when sprinting). The cancel runs before
    /// this tick's `onLivingUpdate` sprint recompute, which re-enables sprint the
    /// same tick if W + sprint are still held (vanilla held-key behaviour) — so
    /// the ×0.6 momentum is the only server-visible effect and no StopSprinting
    /// packet is sent.
    ///
    /// The sprint hit only lands when the target's CLIENT-side `attackEntityFrom`
    /// returns true. On a remote world a living mob (`EntityLivingBase`) short-
    /// circuits to `false` via `worldObj.isRemote`, so hitting a normal mob does
    /// NOT slow the attacker — only other players (`EntityOtherPlayerMP` → true)
    /// and non-living objects (minecart/boat/…) do. Grim gates attack-slow the
    /// same way (`PacketPlayerAttack`: `!isLivingEntity || PLAYER`), so slowing on
    /// a mob hit desyncs its prediction → Simulation. XP orbs (and dropped items)
    /// aren't attackable in vanilla (`canAttackWithItem` false), so never slow.
    fn attack_entity(&mut self, id: i32, out: &mut Vec<ServerboundPacket>) {
        self.sync_current_play_item(out);
        out.push(ServerboundPacket::UseEntity {
            target: id,
            kind: UseEntityKind::Attack,
        });
        let target_slows = matches!(
            self.world.entity(EntityId(id)).map(|e| e.kind),
            Some(EntityKind::RemotePlayer | EntityKind::Object(_))
        );
        if target_slows && self.sprinting {
            self.player.velocity.x *= 0.6;
            self.player.velocity.z *= 0.6;
            self.sprinting = false;
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
            let ticks = self.block_break_ticks(self.world.block_at(x, y, z));
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

    /// Vanilla `EntityPlayer.getBreakSpeed`: the held tool's dig speed against
    /// `block` — its raw strength times the efficiency enchant, haste / mining
    /// fatigue, and the in-air penalty. (Underwater Aqua-Affinity is not
    /// modelled.)
    fn break_speed(&self, block: BlockState) -> f32 {
        let item = self.held_item();
        let mut f = item.map_or(1.0, |it| tool_strength(it, block));
        // Efficiency adds (level² + 1), but only to a tool already effective here.
        if f > 1.0 {
            if let Some(level) = item.map(efficiency_level).filter(|l| *l > 0) {
                f += (level * level + 1) as f32;
            }
        }
        // Haste (potion id 3) speeds up; mining fatigue (id 4) cripples.
        if let Some(e) = self.effects.get(&3) {
            f *= 1.0 + (e.amplifier as f32 + 1.0) * 0.2;
        }
        if let Some(e) = self.effects.get(&4) {
            f *= match e.amplifier {
                0 => 0.3,
                1 => 0.09,
                2 => 0.0027,
                _ => 0.000_81,
            };
        }
        if !self.player.on_ground {
            f /= 5.0;
        }
        f.max(0.0)
    }

    /// Vanilla `EntityPlayer.canHarvestBlock`: true when the block needs no tool
    /// (its material is hand-harvestable) or the held pickaxe is the right class
    /// and tier. Drives the `/30` vs `/100` dig divisor (and, server-side, drops).
    fn can_harvest_block(&self, block: BlockState) -> bool {
        if !block_needs_tool(block.id) {
            return true;
        }
        matches!(
            self.held_item().map(|it| tool_props(it.id)),
            Some(Some((ToolClass::Pickaxe, _, level))) if level >= block_harvest_level(block.id)
        )
    }

    /// Number of 20 Hz ticks to break `block` in survival, matching vanilla's
    /// `Block.getPlayerRelativeBlockHardness` (`break_speed / hardness / divisor`,
    /// divisor 30 when harvestable else 100; the block breaks once the summed
    /// per-tick progress reaches 1). `INFINITY` for unbreakable blocks; 1 for an
    /// instant break. The server (Grim FastBreak) predicts this exactly, so it
    /// must match to keep digging legal.
    fn block_break_ticks(&self, block: BlockState) -> f32 {
        let hardness = block_hardness(block.id);
        if hardness < 0.0 {
            return f32::INFINITY; // unbreakable (bedrock, etc.)
        }
        let speed = self.break_speed(block);
        if speed <= 0.0 {
            return f32::INFINITY;
        }
        let divisor = if self.can_harvest_block(block) { 30.0 } else { 100.0 };
        // Per-tick progress; ≥1 means the click alone destroys it (hardness 0, or
        // a very fast tool).
        let per_tick = speed / hardness / divisor;
        if per_tick >= 1.0 {
            return 1.0;
        }
        (1.0 / per_tick).ceil().max(1.0)
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
                self.particles.spawn_block_debris(x, y, z, old);
                let pos = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                self.queue_sound(dig_sound_for_block(old.id), pos, 1.0, 0.8);
            }
        }
    }

    /// Client-side relight after a block edit: flood-fill block light, and (in
    /// dimensions with a sky) recompute the column's sky-light, queuing every
    /// affected section for an urgent re-mesh. Runs in both single-player and
    /// multiplayer: the 1.8 protocol doesn't re-send light for a single block
    /// change, so the client recomputes it locally (as vanilla does). Without
    /// this, freshly exposed faces keep the chunk's load-time light and render
    /// black until the chunk reloads.
    fn relight_after_edit(&mut self, x: i32, y: i32, z: i32, old: BlockState) {
        let block_light = self.world.update_block_light(x, y, z, old);
        self.dirty_chunks.extend(block_light.iter().copied());
        self.urgent_remesh.extend(block_light);
        // Sky-light only exists in the overworld; the nether/end have none, and
        // forcing open columns to 15 there would wrongly brighten the world.
        if self.has_sky_light {
            // Roofing over darkens the space below; a dug shaft lets daylight in.
            let sky_light = self.world.update_sky_light(x, y, z, old);
            self.dirty_chunks.extend(sky_light.iter().copied());
            self.urgent_remesh.extend(sky_light);
        }
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
            // No client-side place sound: vanilla's `ItemBlock.onItemUse` plays it
            // via `World.playSoundEffect`, but on the client that hits the empty
            // `RenderGlobal.playSound` (a no-op). The audible place sound comes
            // from the server's S29 SoundEffect (`WorldManager.playSound` →
            // `sendToAllNear`, which includes the placer). Playing it here too
            // would double it. (Block *breaking* differs: the server excludes the
            // breaker via `sendToAllNearExcept`, so that one is predicted locally.)
            true
        } else {
            false
        }
    }

    /// The block currently being mined and its 0..9 crack stage, for the HUD /
    /// breaking overlay.
    pub fn breaking_overlay(&self) -> Option<(i32, i32, i32, u8, BlockState)> {
        self.breaking.as_ref().map(|b| {
            let stage = (b.progress.clamp(0.0, 0.999) * 10.0) as u8;
            (b.x, b.y, b.z, stage, self.world.block_at(b.x, b.y, b.z))
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
            } => self.handle_join_game(entity_id, game_mode, dimension),
            ClientboundPlayPacket::UpdateHealth {
                health,
                food,
                food_saturation,
            } => self.handle_update_health(health, food, food_saturation),
            ClientboundPlayPacket::SetExperience { bar, level } => {
                self.xp_bar = bar.clamp(0.0, 1.0);
                self.xp_level = level;
                false
            }
            ClientboundPlayPacket::Respawn {
                dimension,
                game_mode,
                ..
            } => self.handle_respawn(dimension, game_mode),
            ClientboundPlayPacket::PlayerPositionLook {
                x,
                y,
                z,
                yaw,
                pitch,
                flags,
            } => self.handle_player_position_look(x, y, z, yaw, pitch, flags),
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
            } => self.handle_multi_block_change(chunk_x, chunk_z, changes),
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
            } => self.handle_spawn_particle(
                particle_id, x, y, z, offset_x, offset_y, offset_z, speed, count, args,
            ),
            ClientboundPlayPacket::ChunkBulk {
                sky_light_sent,
                chunks,
            } => self.handle_chunk_bulk(sky_light_sent, chunks),
            ClientboundPlayPacket::SpawnPlayer {
                entity_id,
                uuid,
                x,
                y,
                z,
                yaw,
                pitch,
            } => self.handle_spawn_player(entity_id, uuid, x, y, z, yaw, pitch),
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
                velocity,
            } => self.handle_spawn_object(entity_id, kind, x, y, z, yaw, pitch, data, velocity),
            ClientboundPlayPacket::SpawnExperienceOrb {
                entity_id,
                x,
                y,
                z,
                count,
            } => self.handle_spawn_experience_orb(entity_id, x, y, z, count),
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
            } => self.handle_entity_velocity(entity_id, vx, vy, vz),
            ClientboundPlayPacket::OpenWindow {
                window_id,
                inventory_type,
                title,
                slots,
                ..
            } => self.handle_open_window(window_id, inventory_type, title, slots),
            ClientboundPlayPacket::CloseWindowS { window_id } => {
                self.handle_close_window_s(window_id)
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
            } => self.handle_set_slot(window_id, slot, item),
            ClientboundPlayPacket::WindowItems { window_id, items } => {
                self.handle_window_items(window_id, items)
            }
            ClientboundPlayPacket::HeldItemChange { slot } => {
                // The server tells us which hotbar slot is selected.
                self.selected_slot = (slot as i32).clamp(0, 8);
                false
            }
            ClientboundPlayPacket::DestroyEntities { entity_ids } => {
                self.handle_destroy_entities(entity_ids)
            }
            ClientboundPlayPacket::PlayerAbilities {
                invulnerable,
                flying,
                allow_flying,
                creative,
                fly_speed,
                walk_speed,
            } => self.handle_player_abilities(
                invulnerable,
                flying,
                allow_flying,
                creative,
                fly_speed,
                walk_speed,
            ),
            ClientboundPlayPacket::EntityProperties {
                entity_id,
                properties,
            } => self.handle_entity_properties(entity_id, properties),
            ClientboundPlayPacket::EntityEffect {
                entity_id,
                effect_id,
                amplifier,
                duration,
                ..
            } => {
                // Track only the local player's effects (the HUD reads them for
                // absorption hearts, the regen heartbeat and the heart tint).
                if entity_id == self.player.id.0 {
                    self.effects.insert(
                        effect_id as u8,
                        ActiveEffect {
                            amplifier,
                            duration,
                        },
                    );
                }
                false
            }
            ClientboundPlayPacket::RemoveEntityEffect {
                entity_id,
                effect_id,
            } => {
                if entity_id == self.player.id.0 {
                    self.effects.remove(&(effect_id as u8));
                }
                false
            }
            ClientboundPlayPacket::ChatMessage { json, position } => {
                self.handle_chat_message(json, position)
            }
            ClientboundPlayPacket::TabComplete { matches } => {
                self.chat.set_completions(matches);
                false
            }
            ClientboundPlayPacket::UpdateSign { x, y, z, lines } => {
                self.handle_update_sign(x, y, z, lines)
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
                self.handle_player_list_header_footer(header, footer)
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
            } => self.handle_attach_entity(entity_id, vehicle_id, leash),
            ClientboundPlayPacket::EntityAnimation {
                entity_id,
                animation,
            } => self.handle_entity_animation(entity_id, animation),
            ClientboundPlayPacket::EntityStatus {
                entity_id,
                status,
            } => self.handle_entity_status(entity_id, status),
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
            } => self.handle_entity_equipment(entity_id, slot, item),
            ClientboundPlayPacket::CollectItem {
                collected_id,
                collector_id,
            } => self.handle_collect_item(collected_id, collector_id),
            ClientboundPlayPacket::SoundEffect {
                name,
                x,
                y,
                z,
                volume,
                pitch,
            } => self.handle_sound_effect(name, x, y, z, volume, pitch),
            ClientboundPlayPacket::ChangeGameState { reason, value } => {
                self.handle_change_game_state(reason, value)
            }
            ClientboundPlayPacket::SpawnGlobalEntity { kind, x, y, z, .. } => {
                // Vanilla only ever sends kind 1, a lightning bolt.
                if kind == 1 {
                    self.handle_lightning_bolt(x, y, z);
                }
                false
            }
            ClientboundPlayPacket::Effect {
                effect_id,
                x,
                y,
                z,
                data,
                ..
            } => self.handle_effect(effect_id, x, y, z, data),
            ClientboundPlayPacket::BlockAction {
                x,
                y,
                z,
                action_id,
                action_param,
                block_type,
            } => self.handle_block_action(x, y, z, action_id, action_param, block_type),
            // ConfirmTransaction is ponged on the network thread (vanilla replies
            // immediately), so the game loop never needs to act on it.
            ClientboundPlayPacket::PluginMessage { channel, data } => {
                // MC|TrList carries a villager's trade offers for the open window.
                if channel == "MC|TrList" {
                    if let Some((window_id, trades)) = crate::container::parse_trade_list(&data) {
                        if let Some(container) = self.open_container.as_mut() {
                            if container.window_id as i32 == window_id {
                                container.set_trades(trades);
                            }
                        }
                    }
                }
                false
            }
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
        metadata: &[fpsmaster_protocol::v1_8_9::packets::MetadataEntry],
    ) {
        for entry in metadata {
            match (entry.index, &entry.value) {
                (0, MetadataValue::Byte(flags)) => {
                    let on_fire = flags & 0x01 != 0;
                    let sneaking = flags & 0x02 != 0;
                    let using_item = flags & 0x10 != 0;
                    let invisible = flags & 0x20 != 0;
                    if entity_id == self.player.id.0 {
                        self.player.on_fire = on_fire;
                    }
                    if let Some(entity) = self.remote_entity_mut(entity_id) {
                        entity.sneaking = sneaking;
                        entity.using_item = using_item;
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
                    // Health reaching 0 is the other death signal besides
                    // EntityStatus 3 (some servers only send one), so kick off the
                    // death animation here too — `start_death` is idempotent.
                    if let Some(entity) = self.remote_entity_mut(entity_id) {
                        entity.health = Some(*health);
                        if *health <= 0.0 {
                            entity.start_death();
                        }
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

    /// Drain this frame's dirty sections: every GPU-mesh removal (unloaded
    /// column) plus up to `max` real (re)mesh sections nearest the player, the
    /// rest queued for following frames. Bounds the per-frame mesh-rebuild cost
    /// so a join-time burst of chunks doesn't stall rendering, while never
    /// deferring removals (which would leak their GPU mesh while exploring).
    pub fn take_dirty_chunks_budget(&mut self, max: usize) -> Vec<SectionPos> {
        if self.dirty_chunks.is_empty() {
            return Vec::new();
        }
        // Sections whose column is no longer loaded are GPU-mesh *removals*: the
        // server unloaded the chunk and `world` already dropped it, so applying
        // one only frees a buffer slot (no meshing). They sit at the far edge of
        // view, so the nearest-first budget below would starve them during
        // sustained exploration — the GPU mesh leaks until the player stops and
        // the dirty set finally drains, which is the out-of-memory path. Drain
        // every removal each frame; spend `max` only on real (re)mesh work.
        let removals: Vec<SectionPos> = self
            .dirty_chunks
            .iter()
            .copied()
            .filter(|p| self.world.chunk(p.chunk()).is_none())
            .collect();
        for p in &removals {
            self.dirty_chunks.remove(p);
        }
        let mut out = removals;

        if max == 0 || self.dirty_chunks.is_empty() {
            return out;
        }
        if self.dirty_chunks.len() <= max {
            out.extend(self.dirty_chunks.drain());
            return out;
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
        out.extend(all);
        out
    }

    /// Render-distance safety net: bound resident GPU mesh memory to the client's
    /// view, independent of whether the server ever unloads. Columns that drift
    /// beyond `render_distance + RESIDENCY_MARGIN` (Chebyshev, chunks) have their
    /// GPU meshes dropped — their block data stays in `world`, so re-entering the
    /// area re-meshes without a server round-trip and never shows a hole. Columns
    /// that come back into range are re-queued for meshing through the normal
    /// dirty budget. Runs only when the player crosses a chunk boundary. Returns
    /// the sections whose GPU mesh the caller should free.
    pub fn enforce_render_distance(&mut self, render_distance: u32) -> Vec<SectionPos> {
        let cx = (self.player.position.x.floor() as i32).div_euclid(16);
        let cz = (self.player.position.z.floor() as i32).div_euclid(16);
        let here = ChunkPos::new(cx, cz);
        if self.last_residency_chunk == Some(here) {
            return Vec::new();
        }
        self.last_residency_chunk = Some(here);
        let keep = render_distance as i32 + RESIDENCY_MARGIN;
        let chebyshev = |c: ChunkPos| (c.x - cx).abs().max((c.z - cz).abs());

        // Previously-evicted columns that returned to range (re-mesh) or that the
        // server has since unloaded (just forget them).
        let returning: Vec<ChunkPos> = self
            .evicted_columns
            .iter()
            .copied()
            .filter(|&c| chebyshev(c) <= keep || self.world.chunk(c).is_none())
            .collect();
        for c in returning {
            self.evicted_columns.remove(&c);
            if let Some(chunk) = self.world.chunk(c) {
                if chebyshev(c) <= keep {
                    let ys: Vec<i32> = chunk.sections().map(|s| s.y()).collect();
                    for y in ys {
                        self.dirty_chunks.insert(SectionPos::new(c.x, y, c.z));
                    }
                }
            }
        }

        // Drop the GPU meshes of in-world columns that drifted out of range.
        let mut removals = Vec::new();
        let to_evict: Vec<ChunkPos> = self
            .world
            .chunks()
            .map(|chunk| chunk.position)
            .filter(|&c| chebyshev(c) > keep && !self.evicted_columns.contains(&c))
            .collect();
        for c in to_evict {
            self.evicted_columns.insert(c);
            for y in 0..16 {
                removals.push(SectionPos::new(c.x, y, c.z));
            }
            // Cancel any queued (re)mesh work for a column we're dropping.
            self.dirty_chunks.retain(|s| s.x != c.x || s.z != c.z);
        }
        removals
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
        let old = self.world.block_at(x, y, z);
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
        self.relight_after_edit(x, y, z, old);
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
        if let Some(biomes) = &column.biomes {
            self.world.remove_chunk(pos);
            // Keep the biome array — the mesher colours grass/foliage/water from
            // it. `remove_chunk` cleared the column, so this re-creates it before
            // the sections land.
            self.world.set_biomes(x, z, biomes);
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

    /// Build the movement packet snapshot. Public so the host can rebuild it
    /// AFTER the extension tick runs (post-physics), capturing a mod's silent
    /// look + this tick's position together, so a placement's rotation matches
    /// the position the flying packet carries (Grim RotationPlace ray-traces both).
    pub fn movement_snapshot(&self) -> MovementSnapshot {
        // A silent-look override (extension `setRotation` with silent) rides the
        // movement packet so the server sees that rotation while the camera stays.
        let (yaw, pitch) = self
            .server_look
            .unwrap_or((self.player.yaw, self.player.pitch));
        MovementSnapshot {
            x: self.player.position.x,
            y: self.player.position.y,
            z: self.player.position.z,
            yaw,
            pitch,
            on_ground: self.player.on_ground,
            entity_id: self.player.id.0,
            sneaking: self.input.sneak,
            sprinting: self.sprinting,
        }
    }
}

/// Per-packet handlers extracted from `apply_play_packet`. Each returns the
/// same "terrain must re-mesh" bool its match arm produced; behaviour is
/// identical to the inlined arm bodies.
impl GameState {
    fn handle_join_game(&mut self, entity_id: i32, game_mode: u8, dimension: i8) -> bool {
        self.player.id = EntityId(entity_id);
        self.has_sky_light = dimension == 0;
        // Drives what an absent section reads as (see `Chunk::sky_light_fallback`).
        self.world.set_has_sky_light(self.has_sky_light);
        self.dimension = dimension;
        // Low 3 bits are the gamemode; bit 3 is the hardcore flag.
        self.creative = (game_mode & 0x7) == 1;
        self.joined_game = true;
        // Restart the music delay so a track can start shortly after spawn.
        self.music_ticker.delay_ticks = 100;
        self.world.upsert_entity(self.player.clone());
        false
    }

    fn handle_update_health(&mut self, health: f32, food: i32, food_saturation: f32) -> bool {
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

    fn handle_respawn(&mut self, dimension: i32, game_mode: u8) -> bool {
        // The server resends chunks plus a fresh PlayerPositionLook at the
        // respawn point; wait for that position before reporting movement
        // again so we don't send a stale (death-location) position.
        self.has_sky_light = dimension == 0;
        // Drives what an absent section reads as (see `Chunk::sky_light_fallback`).
        self.world.set_has_sky_light(self.has_sky_light);
        self.dimension = dimension as i8;
        self.creative = (game_mode & 0x7) == 1;
        self.needs_respawn = false;
        self.is_dead = false;
        self.health = 20.0;
        self.max_health = 20.0;
        self.effects.clear();
        self.position_synced = false;
        self.pending_confirm = false;
        self.player.velocity = DVec3::ZERO;
        log::info!("respawned into dimension {dimension}");
        false
    }

    fn handle_player_position_look(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        flags: i8,
    ) -> bool {
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

    fn handle_multi_block_change(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        changes: Vec<fpsmaster_protocol::v1_8_9::packets::BlockChangeRecord>,
    ) -> bool {
        let mut changed = false;
        for block in changes {
            let x = chunk_x * 16 + block.x as i32;
            let z = chunk_z * 16 + block.z as i32;
            changed |= self.apply_block_change(x, block.y as i32, z, block.id, block.meta);
        }
        changed
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_spawn_particle(
        &mut self,
        particle_id: i32,
        x: f32,
        y: f32,
        z: f32,
        offset_x: f32,
        offset_y: f32,
        offset_z: f32,
        speed: f32,
        count: i32,
        args: Vec<i32>,
    ) -> bool {
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

    fn handle_chunk_bulk(
        &mut self,
        sky_light_sent: bool,
        chunks: Vec<fpsmaster_protocol::v1_8_9::packets::BulkChunkData>,
    ) -> bool {
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

    #[allow(clippy::too_many_arguments)]
    fn handle_spawn_player(
        &mut self,
        entity_id: i32,
        uuid: [u8; 16],
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    ) -> bool {
        self.spawn_remote_entity(entity_id, EntityKind::RemotePlayer, x, y, z, yaw, pitch);
        self.entity_uuids.insert(EntityId(entity_id), uuid);
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_spawn_object(
        &mut self,
        entity_id: i32,
        kind: i8,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        data: i32,
        velocity: Option<(f64, f64, f64)>,
    ) -> bool {
        self.spawn_remote_entity(
            entity_id,
            EntityKind::Object(kind as u8),
            x,
            y,
            z,
            yaw,
            pitch,
        );
        // Seed the spawn velocity (present when data != 0, e.g. a thrown
        // projectile) so the client can simulate the flight locally
        // instead of waiting on server position packets.
        if let Some((vx, vy, vz)) = velocity {
            if let Some(entity) = self.remote_entity_mut(entity_id) {
                entity.velocity = DVec3::new(vx, vy, vz);
            }
        }
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

    fn handle_spawn_experience_orb(
        &mut self,
        entity_id: i32,
        x: f64,
        y: f64,
        z: f64,
        count: i16,
    ) -> bool {
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

    fn handle_entity_velocity(&mut self, entity_id: i32, vx: f64, vy: f64, vz: f64) -> bool {
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

    fn handle_open_window(
        &mut self,
        window_id: u8,
        inventory_type: String,
        title: String,
        slots: u8,
    ) -> bool {
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

    fn handle_close_window_s(&mut self, window_id: u8) -> bool {
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

    fn handle_set_slot(&mut self, window_id: i8, slot: i16, item: Option<SlotItem>) -> bool {
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

    fn handle_window_items(&mut self, window_id: u8, items: Vec<Option<SlotItem>>) -> bool {
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

    /// S0D CollectItem: play the pickup sound when the local player absorbs an
    /// item or XP orb (vanilla `EntityItem`/`EntityXPOrb` client sounds). Only
    /// the local player's pickups make a sound here.
    fn handle_collect_item(&mut self, collected_id: i32, collector_id: i32) -> bool {
        if collector_id != self.player.id.0 {
            return false;
        }
        let collected = EntityId(collected_id);
        // Play at the collected entity's position, or the player if it's gone.
        let pos = self
            .world
            .entity(collected)
            .map(|e| to_render_vec3(e.position))
            .unwrap_or(self.camera.position);
        let is_xp = self.entity_xp.contains_key(&collected)
            || matches!(
                self.world.entity(collected).map(|e| e.kind),
                Some(EntityKind::ExperienceOrb)
            );
        if is_xp {
            // random.orb, volume 0.1, pitch 0.5*(rand*0.7+1.8) -> ~[0.9, 1.15).
            let r = self.mood_rand_float();
            let pitch = 0.5 * (r * 0.7 + 1.8);
            self.queue_sound("random.orb", pos, 0.1, pitch);
        } else {
            // random.pop, volume 0.2, pitch (rand*0.7+1.0)*2.0 -> [2.0, 3.4).
            let r = self.mood_rand_float();
            let pitch = (r * 0.7 + 1.0) * 2.0;
            self.queue_sound("random.pop", pos, 0.2, pitch);
        }
        false
    }

    fn handle_destroy_entities(&mut self, entity_ids: Vec<i32>) -> bool {
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
            // Stop any entity-attached looping sound (minecart, …).
            self.stop_entity_sound(id);
        }
        false
    }

    fn handle_player_abilities(
        &mut self,
        invulnerable: bool,
        flying: bool,
        allow_flying: bool,
        creative: bool,
        fly_speed: f32,
        walk_speed: f32,
    ) -> bool {
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

    fn handle_entity_properties(
        &mut self,
        entity_id: i32,
        properties: Vec<fpsmaster_protocol::v1_8_9::packets::EntityProperty>,
    ) -> bool {
        // Only the local player's attributes feed the HUD/prediction;
        // other entities' attributes aren't modeled.
        if entity_id == self.player.id.0 {
            for property in &properties {
                match property.key.as_str() {
                    "generic.movementSpeed" => {
                        self.walk_speed_attribute =
                            effective_attribute_value(property, &SPRINT_SPEED_BOOST_UUID)
                                as f32;
                    }
                    "generic.maxHealth" => {
                        self.max_health =
                            effective_attribute_value(property, &[0u8; 16]) as f32;
                    }
                    _ => {}
                }
            }
        }
        false
    }

    fn handle_chat_message(&mut self, json: String, position: i8) -> bool {
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

    fn handle_update_sign(&mut self, x: i32, y: i32, z: i32, lines: [String; 4]) -> bool {
        // Store the four lines flattened from chat-JSON, keyed by block
        // position; the sign block-entity renderer draws them in world.
        let text = lines.map(|line| chat::flatten_chat_json(&line));
        self.signs.insert([x, y, z], text);
        false
    }

    fn handle_player_list_header_footer(&mut self, header: String, footer: String) -> bool {
        self.player_list.set_header_footer(
            chat::flatten_chat_json(&header),
            chat::flatten_chat_json(&footer),
        );
        false
    }

    fn handle_attach_entity(&mut self, entity_id: i32, vehicle_id: i32, leash: bool) -> bool {
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

    fn handle_entity_animation(&mut self, entity_id: i32, animation: u8) -> bool {
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

    fn handle_entity_status(&mut self, entity_id: i32, status: i8) -> bool {
        if status == 2 {
            if let Some(entity) = self.world.entity_mut(EntityId(entity_id)) {
                entity.start_hurt();
            }
            // The local player's own hurt animation also plays the hurt
            // sound (vanilla EntityPlayer.handleStatusUpdate → playHurtSound).
            // UpdateHealth covers damage that changes health, but EntityStatus
            // fires on every hit (incl. absorbed/blocked), so play it here too.
            if EntityId(entity_id) == self.player.id {
                // Drive the hurt-camera tilt off the authoritative local
                // player (its world copy isn't tick-interpolated, so its
                // hurt timer would never decrement).
                self.player.start_hurt();
                let pos = self.camera.position;
                self.queue_sound("game.player.hurt", pos, 1.0, 1.0);
            }
        }
        // Status 3 (death): start the fall-over + red-corpse animation.
        if status == 3 {
            if let Some(entity) = self.world.entity_mut(EntityId(entity_id)) {
                entity.start_death();
            }
        }
        false
    }

    fn handle_entity_equipment(
        &mut self,
        entity_id: i32,
        slot: i16,
        item: Option<SlotItem>,
    ) -> bool {
        if (0..5).contains(&slot) {
            let slots = self
                .entity_equipment
                .entry(EntityId(entity_id))
                .or_insert_with(Default::default);
            slots[slot as usize] = item;
        }
        false
    }

    fn handle_sound_effect(
        &mut self,
        name: String,
        x: f64,
        y: f64,
        z: f64,
        volume: f32,
        pitch: u8,
    ) -> bool {
        let pos = Vec3::new(x as f32, y as f32, z as f32);
        let rate = pitch as f32 / 63.5;
        self.queue_sound(name, pos, volume, rate);
        false
    }

    fn handle_effect(&mut self, effect_id: i32, x: i32, y: i32, z: i32, data: i32) -> bool {
        if let Some(event) = effect_event(effect_id, data) {
            let pos = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            self.queue_sound(event, pos, 1.0, 1.0);
        }
        false
    }

    fn handle_block_action(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        action_id: u8,
        action_param: u8,
        block_type: i32,
    ) -> bool {
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

/// Vanilla flight physics for a locally-simulated SpawnObject `kind`, as
/// `(gravity, drag, collides)`: the per-tick downward pull, the air-resistance
/// multiplier and whether a block hit sticks it. `None` for kinds that aren't
/// simulated client-side (self-propelled fireballs/fireworks, the eye of ender,
/// the fishing bobber — left on server interpolation).
///
/// Throwables share `EntityThrowable` (drag 0.99): snowball/egg/ender pearl/
/// potion fall at 0.03, the thrown exp bottle at 0.07. Arrows fall at 0.05 and
/// stick into blocks.
fn projectile_physics(kind: u8) -> Option<(f64, f64, bool)> {
    Some(match kind {
        60 => (0.05, 0.99, true),           // arrow
        61 | 62 | 65 | 73 => (0.03, 0.99, false), // snowball / egg / ender pearl / potion
        75 => (0.07, 0.99, false),          // bottle o' enchanting
        _ => return None,
    })
}

/// The item id whose sprite stands in for a SpawnObject projectile `kind`
/// (vanilla projectile render textures), or `None` for kinds rendered
/// elsewhere or not handled. The arrow (60) is drawn as a 3D model, not a sprite.
fn projectile_item_id(kind: u8) -> Option<i16> {
    Some(match kind {
        // Arrow (60) is drawn as a 3D model in the entity model pass, not a sprite.
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

/// The world lightmap at an entity's position, as a per-channel multiplier for
/// its model vertices.
///
/// Vanilla samples one lightmap texel per entity, so this has to be the exact
/// same function the terrain uses — `fpsmaster_render::sky::vanilla_lightmap`,
/// which both this and `chunk.wgsl` are now driven by. It previously used a
/// private curve (`max(sky, block)` raised to a brightness-derived power) that
/// shared nothing with the terrain's, so a mob never quite matched the block it
/// stood on: no warm torch tint, no blue night tint, different falloff.
///
/// Returned in GAMMA space, like the shader's; `model.wgsl` decodes it.
/// A lightning bolt spawned by S2C SpawnGlobalEntity.
///
/// Vanilla's `EntityLightningBolt` picks a random `boltVertex` seed once and
/// renders a forked polyline from it for a handful of ticks, re-randomising the
/// shape a couple of times mid-life. The seed is kept here so the geometry is a
/// pure function of the bolt — no per-frame randomness, so it does not flicker
/// between frames within a tick.
#[derive(Debug, Clone, Copy)]
pub struct LightningBolt {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub seed: u32,
    /// Ticks left to live; vanilla renders a bolt for a few ticks only.
    life: u32,
}

impl LightningBolt {
    /// Vanilla's bolt is visible for `rand.nextInt(3) + 1` "living" ticks on top
    /// of its two-tick strike, so 4 is the top of that range.
    const LIFE_TICKS: u32 = 4;

    fn new(x: f64, y: f64, z: f64, seed: u32) -> Self {
        Self {
            x,
            y,
            z,
            seed,
            life: Self::LIFE_TICKS,
        }
    }

    /// 1.0 at the strike, falling to 0 as the bolt fades.
    pub fn brightness(&self) -> f32 {
        self.life as f32 / Self::LIFE_TICKS as f32
    }
}

/// Client-side weather, ported from `World.updateWeatherBody` and
/// `getRainStrength` / `getThunderStrength`.
///
/// The server sends only "it is / is not raining" (S2B reasons 1 and 2, or an
/// explicit level via reason 7/8); the client ramps toward that target by 0.01
/// per tick, which is why vanilla weather fades in over ~5 seconds instead of
/// popping. `prev_*` holds last tick's value so a frame can interpolate, like
/// every other per-tick quantity here.
///
/// Thunder is layered on top of rain, not an alternative to it: a thunderstorm
/// is `raining && thundering`, and both strengths ramp independently.
#[derive(Debug, Clone, Copy)]
pub struct Weather {
    raining: bool,
    thundering: bool,
    rain: f32,
    prev_rain: f32,
    thunder: f32,
    prev_thunder: f32,
    /// Ticks left on the lightning flash (vanilla `World.lastLightningBolt`,
    /// set to 2 when a bolt spawns and counted down each tick). While it is
    /// non-zero the lightmap's sky term is forced to full.
    lightning_flash: u32,
    /// Standing water, 0..=1 — how much has actually POOLED, as opposed to how
    /// hard it is currently raining.
    ///
    /// Kept separate from `rain` because the two move at very different speeds:
    /// rain reaches full in 5 seconds, but puddles that appear that fast read as
    /// a switch being flipped. This fills over ~20 seconds and drains over ~40,
    /// so pools build up during a downpour and linger after it passes.
    puddle: f32,
    prev_puddle: f32,
    /// Force every column to snow regardless of biome.
    ///
    /// Snow is normally a property of the biome, and every locally generated
    /// world uses the default plains biome — so without this there is no way to
    /// see snow at all outside a cold-biome server. Test affordance, same as
    /// `/weather strike`.
    force_snow: bool,
}

impl Default for Weather {
    fn default() -> Self {
        Self {
            raining: false,
            thundering: false,
            rain: 0.0,
            prev_rain: 0.0,
            thunder: 0.0,
            prev_thunder: 0.0,
            lightning_flash: 0,
            puddle: 0.0,
            prev_puddle: 0.0,
            force_snow: false,
        }
    }
}

impl Weather {
    /// Vanilla ramp rate: 0.01 per tick, so a full fade takes 100 ticks (5 s).
    const RAMP: f32 = 0.01;
    /// Puddles fill over ~20 s and drain over ~40 s. Not a vanilla quantity —
    /// vanilla has no puddles — but tying them to the rain ramp made them snap
    /// into existence the moment the weather changed.
    const PUDDLE_FILL: f32 = 0.0025;
    const PUDDLE_DRAIN: f32 = 0.00125;

    /// Whether every column is being forced to snow.
    pub fn force_snow(&self) -> bool {
        self.force_snow
    }

    pub fn set_force_snow(&mut self, force: bool) {
        self.force_snow = force;
        if force {
            self.raining = true;
        }
    }

    pub fn set_raining(&mut self, raining: bool) {
        self.raining = raining;
        // Vanilla clears thunder along with rain — a thunderstorm cannot outlive
        // the rain it rides on.
        if !raining {
            self.thundering = false;
        }
    }

    pub fn set_thundering(&mut self, thundering: bool) {
        self.thundering = thundering;
        if thundering {
            self.raining = true;
        }
    }

    /// S2B reason 7: the server dictating the rain level outright, bypassing the
    /// ramp. Vanilla assigns `rainingStrength` directly here.
    pub fn set_rain_level(&mut self, level: f32) {
        self.rain = level.clamp(0.0, 1.0);
        self.prev_rain = self.rain;
        self.raining = self.rain > 0.0;
    }

    /// S2B reason 8: the server dictating the thunder level outright.
    pub fn set_thunder_level(&mut self, level: f32) {
        self.thunder = level.clamp(0.0, 1.0);
        self.prev_thunder = self.thunder;
        self.thundering = self.thunder > 0.0;
    }

    /// Start the two-tick full-brightness flash a lightning bolt causes.
    pub fn flash(&mut self) {
        self.lightning_flash = 2;
    }

    pub fn tick(&mut self) {
        self.prev_rain = self.rain;
        self.prev_thunder = self.thunder;
        let step = |current: f32, on: bool| {
            (current + if on { Self::RAMP } else { -Self::RAMP }).clamp(0.0, 1.0)
        };
        self.rain = step(self.rain, self.raining);
        self.thunder = step(self.thunder, self.thundering);
        self.prev_puddle = self.puddle;
        // Pools follow the rain that is actually falling, not the target state,
        // so a brief shower leaves only a trace.
        let delta = if self.raining {
            Self::PUDDLE_FILL * self.rain
        } else {
            -Self::PUDDLE_DRAIN
        };
        self.puddle = (self.puddle + delta).clamp(0.0, 1.0);
        self.lightning_flash = self.lightning_flash.saturating_sub(1);
    }

    /// Rain strength interpolated across the current tick, 0..=1.
    pub fn rain_strength(&self, tick_alpha: f32) -> f32 {
        self.prev_rain + (self.rain - self.prev_rain) * tick_alpha
    }

    /// Thunder strength interpolated across the current tick, 0..=1.
    ///
    /// Vanilla gates thunder by the rain level (`getThunderStrength` returns
    /// `thunderingStrength * rainingStrength`), so thunder can never darken a
    /// sky that is not already raining.
    pub fn thunder_strength(&self, tick_alpha: f32) -> f32 {
        let thunder = self.prev_thunder + (self.thunder - self.prev_thunder) * tick_alpha;
        thunder * self.rain_strength(tick_alpha)
    }

    /// Standing water level for this frame, 0..=1. See [`Self::puddle`].
    pub fn puddle_level(&self, tick_alpha: f32) -> f32 {
        self.prev_puddle + (self.puddle - self.prev_puddle) * tick_alpha
    }

    /// Whether a lightning flash is lighting the sky this tick.
    pub fn lightning_flash(&self) -> bool {
        self.lightning_flash > 0
    }

    pub fn is_raining(&self) -> bool {
        self.raining
    }

    pub fn is_thundering(&self) -> bool {
        self.thundering
    }
}

fn entity_light(world: &World, pos: Vec3, sun_brightness: f32, brightness: f32) -> [f32; 3] {
    let (block_l, sky_l) = world.light_at(
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    );
    fpsmaster_render::sky::vanilla_lightmap(sky_l, block_l, sun_brightness, brightness)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The five 1.8 tool item classes that affect mining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolClass {
    Pickaxe,
    Axe,
    Shovel,
    Sword,
    Shears,
}

/// Vanilla tool item id → (class, `efficiencyOnProperMaterial`, harvest level).
/// Efficiency by tier: wood 2, stone 4, iron 6, diamond 8, gold 12; harvest level
/// wood/gold 0, stone 1, iron 2, diamond 3. Swords/shears use a special speed
/// curve (see [`tool_strength`]); the efficiency here is unused for them.
fn tool_props(id: i16) -> Option<(ToolClass, f32, u8)> {
    use ToolClass::*;
    Some(match id {
        269 => (Shovel, 2.0, 0),
        270 => (Pickaxe, 2.0, 0),
        271 => (Axe, 2.0, 0),
        268 => (Sword, 2.0, 0),
        273 => (Shovel, 4.0, 1),
        274 => (Pickaxe, 4.0, 1),
        275 => (Axe, 4.0, 1),
        272 => (Sword, 4.0, 1),
        256 => (Shovel, 6.0, 2),
        257 => (Pickaxe, 6.0, 2),
        258 => (Axe, 6.0, 2),
        267 => (Sword, 6.0, 2),
        277 => (Shovel, 8.0, 3),
        278 => (Pickaxe, 8.0, 3),
        279 => (Axe, 8.0, 3),
        276 => (Sword, 8.0, 3),
        284 => (Shovel, 12.0, 0),
        285 => (Pickaxe, 12.0, 0),
        286 => (Axe, 12.0, 0),
        283 => (Sword, 12.0, 0),
        359 => (Shears, 1.0, 0),
        _ => return None,
    })
}

/// The tool class that speeds up a block (vanilla `ItemTool.getStrVsBlock`'s
/// material/effective-block check), or `None` if no held tool is effective.
/// Grouped by the block's 1.8 material: pickaxe for rock/ore/metal, shovel for
/// soft ground, axe for wood. Swords/shears/web are handled in [`tool_strength`].
fn block_tool_class(id: u16) -> Option<ToolClass> {
    use ToolClass::*;
    // Only confident rock/wood/ground material matches are listed — over-claiming
    // would make the client predict a faster break than the server (an anticheat
    // flag), so unsure blocks fall through to None (bare-hand speed, which is safe).
    match id {
        // Rock / ore / metal — pickaxe.
        1 | 4 | 14 | 15 | 16 | 21 | 22 | 23 | 24 | 41 | 42 | 43 | 44 | 45 | 48 | 49 | 52 | 56 | 57
        | 61 | 62 | 67 | 70 | 73 | 74 | 77 | 79 | 87 | 98 | 101 | 108 | 109 | 112 | 113 | 114 | 116
        | 118 | 121 | 129 | 130 | 133 | 139 | 145 | 152 | 153 | 155 | 156 | 158 | 159 | 168 | 172
        | 173 | 174 | 179 | 180 | 181 | 182 => Some(Pickaxe),
        27 | 28 | 66 | 157 => Some(Pickaxe), // rails
        // Soft ground — shovel.
        2 | 3 | 12 | 13 | 60 | 78 | 80 | 82 | 88 | 110 => Some(Shovel),
        // Wood / plant / vine — axe (planks, logs, bookshelf, chest, crafting
        // table, jukebox, note block, pumpkin, melon, ladder, signs, fences/
        // gates, doors, trapdoor, daylight sensor, vines).
        5 | 17 | 25 | 47 | 53 | 54 | 58 | 63 | 64 | 65 | 68 | 84 | 85 | 86 | 91 | 96 | 103 | 106
        | 107 | 125 | 126 | 134 | 135 | 136 | 146 | 151 | 162 | 163 | 164 | 183..=197 => Some(Axe),
        _ => None,
    }
}

/// Minimum harvest level a pickaxe must have to harvest a tool-required block
/// (vanilla `setHarvestLevel`): 1 = stone, 2 = iron, 3 = diamond; default 0
/// (wood/gold suffices). Only consulted for [`block_needs_tool`] blocks.
fn block_harvest_level(id: u16) -> u8 {
    match id {
        49 => 3,                                         // obsidian → diamond
        14 | 41 | 56 | 57 | 73 | 74 | 129 | 133 | 152 => 2, // gold/diamond/redstone/emerald ore + their blocks
        15 | 21 | 22 | 42 => 1,                          // iron/lapis ore, lapis & iron blocks → stone
        _ => 0,
    }
}

/// Efficiency-enchant level on a stack (enchant id 32 in the `ench` NBT list),
/// or 0 when absent.
fn efficiency_level(item: &SlotItem) -> i32 {
    let Some(nbt) = item.nbt.as_ref() else {
        return 0;
    };
    let Some(ench) = nbt.get("ench").and_then(|t| t.as_list()) else {
        return 0;
    };
    for entry in ench {
        if let Some(c) = entry.as_compound() {
            if c.get("id").and_then(|t| t.as_short()) == Some(32) {
                return c.get("lvl").and_then(|t| t.as_short()).unwrap_or(0) as i32;
            }
        }
    }
    0
}

/// Vanilla `ItemStack.getStrVsBlock`: the held tool's raw speed multiplier
/// against `block` before enchants/potions. Pickaxe/axe/shovel give their tier
/// efficiency on an effective block (else 1); a sword is 15× on cobweb and 1.5×
/// elsewhere; shears are 15× on cobweb/leaves, 5× on wool, 1× otherwise.
fn tool_strength(item: &SlotItem, block: BlockState) -> f32 {
    if is_sword(item.id) {
        return if block.id == 30 { 15.0 } else { 1.5 };
    }
    if item.id == 359 {
        return match block.id {
            30 | 18 | 161 => 15.0, // cobweb, leaves, leaves2
            35 => 5.0,             // wool
            _ => 1.0,
        };
    }
    match tool_props(item.id) {
        Some((class, efficiency, _)) if block_tool_class(block.id) == Some(class) => efficiency,
        _ => 1.0,
    }
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

/// Block hardness in vanilla 1.8.9 units; negative means unbreakable. The full
/// table, copied from `references/minecraft-data` (pc/1.8 blocks.json) so mining
/// time matches vanilla for every block id. Not-diggable blocks (bedrock, the
/// portals, barrier, the moving-piston block, command block, fluids) map to
/// -1.0 (unbreakable); 0.0 is an instant break.
fn block_hardness(id: u16) -> f32 {
    match id {
        49 => 50.0,  // obsidian
        130 => 22.5, // ender chest
        // mob spawner, iron/diamond/emerald/redstone blocks, anvil, beacon,
        // brewing stand, enchanting table, dropper.
        42 | 52 | 57 | 71 | 101 | 116 | 133 | 145 | 152 | 167 | 173 => 5.0,
        30 => 4.0,                 // cobweb
        23 | 61 | 62 | 158 => 3.5, // dispenser, furnaces, dropper
        // ores, gold/lapis blocks, skull bases, mossy/cracked brick, jukebox,
        // redstone lamp, hopper, sea lantern, wooden doors.
        14 | 15 | 16 | 21 | 22 | 41 | 56 | 64 | 73 | 74 | 96 | 121 | 122 | 129
        | 138 | 153 | 154 | 193 | 194 | 195 | 196 | 197 => 3.0,
        54 | 58 | 146 => 2.5, // chest, crafting table, trapped chest
        // cobblestone, planks, logs, brick, slabs, stairs, fences, walls, pistons…
        4 | 5 | 17 | 43 | 44 | 45 | 48 | 53 | 67 | 84 | 85 | 107 | 108 | 112 | 113
        | 114 | 118 | 125 | 126 | 134 | 135 | 136 | 139 | 162 | 163 | 164 | 181
        | 182 | 183 | 184 | 185 | 186 | 187 | 188 | 189 | 190 | 191 | 192 => 2.0,
        1 | 47 | 98 | 109 | 168 => 1.5, // stone, bookshelf, stone bricks, prismarine
        159 | 172 => 1.25,              // stained / plain hardened clay
        63 | 68 | 86 | 91 | 103 | 144 | 176 | 177 => 1.0, // signs, pumpkins, melon, skull, banners
        24 | 25 | 35 | 128 | 155 | 156 | 179 | 180 => 0.8, // sandstone, note block, wool, quartz, red sandstone
        97 => 0.75,                // monster egg
        27 | 28 | 66 | 157 => 0.7, // rails
        2 | 13 | 19 | 60 | 82 | 110 | 111 => 0.6, // grass, gravel, sponge, farmland, clay, mycelium, lily
        3 | 12 | 29 | 33 | 34 | 69 | 70 | 72 | 77 | 79 | 88 | 92 | 117 | 143 | 147
        | 148 | 170 | 174 => 0.5, // dirt, sand, pistons, lever, plates, ice, soul sand, cake, hay…
        65 | 81 | 87 => 0.4,       // ladder, cactus, netherrack
        20 | 89 | 95 | 102 | 123 | 124 | 160 | 169 => 0.3, // glass, glowstone, stained glass/pane, redstone lamp
        18 | 26 | 78 | 80 | 106 | 127 | 151 | 161 | 178 => 0.2, // leaves, bed, snow, vine, cocoa, daylight sensor
        171 => 0.1, // carpet
        // Instant-break: plants, crops, torches, redstone, flowers, tnt, fire…
        6 | 31 | 32 | 37 | 38 | 39 | 40 | 46 | 50 | 51 | 55 | 59 | 75 | 76 | 83
        | 93 | 94 | 99 | 100 | 104 | 105 | 115 | 131 | 132 | 140 | 141 | 142 | 149
        | 150 | 165 | 175 => 0.0,
        // Unbreakable / not diggable.
        7 | 8 | 9 | 10 | 11 | 36 | 90 | 119 | 120 | 137 | 166 => -1.0,
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

/// Targetable = any non-air, non-fluid block. Vanilla ray-picks every block with
/// a selection box, which is all of them except air; torches, plants, rails and
/// the other no-collision blocks have selection boxes too, so they break like the
/// rest. Fluids stay non-pickable (a normal trace doesn't stop on liquid).
fn is_pickable(block: BlockState) -> bool {
    !block.is_air() && !block.is_liquid()
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

/// The simplest usable infinite terrain generator for single-player. It is a
/// pure height-mapped surface — bedrock, a stone body, three dirt layers and a
/// grass top — sampled from the same overlapping-sine field as the terrain demo
/// ([`terrain_height`]), shifted by the world seed. No caves, biomes, water or
/// structures beyond a couple of trees: single-player only needs somewhere to
/// stand, dig and build. It is deterministic, so a column regenerates
/// identically after being unloaded and revisited.
#[derive(Clone, Copy)]
pub struct WorldGen {
    seed: i64,
}

impl WorldGen {
    pub fn new(seed: i64) -> Self {
        Self { seed }
    }

    /// Surface (top grass block) height at world column `(x, z)`. The seed
    /// offsets the sample point so different seeds yield different terrain from
    /// the shared sine field; clamped to leave room for the dirt/bedrock layers.
    fn height(&self, x: i32, z: i32) -> i32 {
        let ox = (self.seed & 0xffff) as i32 - 0x8000;
        let oz = ((self.seed >> 16) & 0xffff) as i32 - 0x8000;
        terrain_height(x.wrapping_add(ox), z.wrapping_add(oz)).clamp(5, 240)
    }

    /// Generate one 16×16 chunk column into `world`, then light it. Trees are
    /// kept fully inside the column so generating one chunk never spawns a
    /// partial neighbour.
    ///
    /// Returns every section whose light changed — the column's own, plus any in
    /// an already-generated neighbour that the flood reached — so the caller can
    /// mark them dirty. Ignoring the return value leaves stale black meshes on
    /// the neighbour side of the border.
    #[must_use]
    fn generate_chunk(&self, world: &mut World, cx: i32, cz: i32) -> Vec<SectionPos> {
        for lx in 0..16 {
            for lz in 0..16 {
                let x = cx * 16 + lx;
                let z = cz * 16 + lz;
                let h = self.height(x, z);

                world.set_block(x, 0, z, BlockState::new(7, 0)); // bedrock
                for y in 1..h.saturating_sub(3).max(1) {
                    // A little scattered coal/iron so bare stone has some interest.
                    let r = hash2d(x, y.wrapping_mul(37) + z, self.seed as u32 ^ 0x9e37_79b9);
                    let block = if r % 96 == 0 {
                        BlockState::new(16, 0) // coal ore
                    } else if r % 160 == 0 {
                        BlockState::new(15, 0) // iron ore
                    } else {
                        BlockState::STONE
                    };
                    world.set_block(x, y, z, block);
                }
                for y in h.saturating_sub(3).max(1)..h {
                    world.set_block(x, y, z, BlockState::DIRT);
                }
                world.set_block(x, h, z, BlockState::GRASS);
            }
        }

        // Up to two deterministic oak trees per chunk. The trunk is placed at
        // local 2..=13 so the ±2 leaf canopy stays inside this chunk column and
        // doesn't create a phantom neighbour that streaming would skip.
        for salt in [0x1111u32, 0x2222] {
            let r = hash2d(cx, cz, self.seed as u32 ^ salt);
            if r % 3 != 0 {
                continue;
            }
            let lx = (r % 12 + 2) as i32;
            let lz = ((r >> 8) % 12 + 2) as i32;
            let x = cx * 16 + lx;
            let z = cz * 16 + lz;
            let h = self.height(x, z);
            place_tree(world, x, h + 1, z);
        }

        // Cast vertical sky-light for the column: surface and open air above stay
        // fully lit, everything under the top block goes dark. Sections ship
        // fully sky-lit otherwise, which would make the underground fullbright.
        world
            .chunk_mut_or_insert(ChunkPos::new(cx, cz))
            .recompute_vertical_skylight();
        // The cast alone is a hard black edge — it has no horizontal bleed, so the
        // ground under a canopy or overhang would sit at sky 0 (vanilla: ~13).
        // Finish with the bounded sky + block light floods.
        world.light_generated_column(cx, cz)
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
    use fpsmaster_core::{BlockState, EntityId, EntityKind};

    /// Headless load-model check for single-player streaming: simulate frames of
    /// `stream_local_world` + the main loop's dirty drain (`MESH_SUBMITS_PER_FRAME`)
    /// and confirm the generation backlog and loaded-chunk count stay bounded —
    /// i.e. generation never outpaces the mesher and floods the pipeline. No GPU.
    #[test]
    fn local_world_streaming_backlog_is_bounded() {
        const DRAIN_PER_FRAME: usize = 40; // MESH_SUBMITS_PER_FRAME in main.rs
        let mut game = GameState::local_world(0x1234_5678, 1.0);
        let rd = 12u32;
        let mut max_dirty = 0usize;
        let mut max_chunks = 0usize;
        for frame in 0..600 {
            game.stream_local_world(rd);
            let _ = game.take_dirty_chunks_budget(DRAIN_PER_FRAME);
            max_dirty = max_dirty.max(game.dirty_chunks.len());
            max_chunks = max_chunks.max(game.world.chunks().count());
            if frame % 3 == 0 {
                game.player.position.x += 4.0; // keep the player moving into fresh terrain
            }
        }
        println!(
            "[stream] max_dirty_backlog={max_dirty} max_loaded_chunks={max_chunks} final_dirty={} final_chunks={}",
            game.dirty_chunks.len(),
            game.world.chunks().count()
        );
        // The mesher drains 40 sections/frame; the backlog must not run away.
        assert!(
            max_dirty < DRAIN_PER_FRAME * 20,
            "dirty backlog exploded to {max_dirty}"
        );
        assert!(max_chunks < 3000, "loaded chunk count exploded to {max_chunks}");
    }

    #[test]
    fn death_roll_follows_the_vanilla_fall_over_curve() {
        let quarter = 90.0_f32.to_radians();
        // Alive (deathTime 0) → upright, no matter the partial tick.
        assert_eq!(death_roll_radians(0, 0.0), 0.0);
        assert_eq!(death_roll_radians(0, 0.5), 0.0);
        // First death tick starts at 0 and grows monotonically.
        assert_eq!(death_roll_radians(1, 0.0), 0.0);
        assert!(death_roll_radians(5, 0.0) > 0.0);
        assert!(death_roll_radians(10, 0.0) > death_roll_radians(5, 0.0));
        // sqrt arg hits 1 once (deathTime-1)/20*1.6 ≥ 1, i.e. deathTime ≥ 13.5,
        // so by tick 14 the body is fully on its side (90°) and stays clamped.
        assert!((death_roll_radians(14, 0.0) - quarter).abs() < 1e-6);
        assert!((death_roll_radians(20, 0.0) - quarter).abs() < 1e-6);
    }

    #[test]
    fn hurt_camera_roll_matches_the_vanilla_curve() {
        // No hurt → no roll; an expired/zero timer → no roll.
        assert_eq!(hurt_camera_roll(0, 0.0), 0.0);
        assert_eq!(hurt_camera_roll(0, 0.5), 0.0);
        // At the instant of the hit (timer just set to 10, no partial) f/maxHurt
        // is 1.0 → sin(π) = 0, so the tilt starts at ~0 then grows.
        assert!(hurt_camera_roll(10, 0.0).abs() < 1.0e-4);
        // Mid-fade the view is tilted; the magnitude never exceeds 14°.
        let mid = hurt_camera_roll(8, 0.0);
        assert!(mid < 0.0, "tilt rolls one way (negative, like vanilla)");
        for t in 0..=10u8 {
            assert!(hurt_camera_roll(t, 0.0).abs() <= 14.0_f32.to_radians() + 1.0e-6);
        }
    }

    #[test]
    fn dirty_budget_never_starves_chunk_removals() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.player.position = DVec3::new(0.0, 64.0, 0.0); // chunk (0,0)
        gs.world.set_block(5, 64, 7, BlockState::STONE); // loads chunk (0,0)

        // A near remesh (loaded column) and a far removal (the server unloaded
        // chunk (50,50), so its column is gone). With max=1 the nearest-first
        // budget would, before the fix, spend its one slot on the near remesh
        // and leave the far removal queued — its GPU mesh would leak.
        let near = SectionPos::new(0, 4, 0);
        let far_removal = SectionPos::new(50, 4, 50);
        gs.dirty_chunks.insert(near);
        gs.dirty_chunks.insert(far_removal);

        let taken = gs.take_dirty_chunks_budget(1);
        assert!(
            taken.contains(&far_removal),
            "removal must be drained regardless of the nearest-first budget"
        );
        assert!(taken.contains(&near), "the budget should still take the near remesh");
        assert!(gs.dirty_chunks.is_empty(), "nothing left queued");
    }

    #[test]
    fn render_distance_evicts_far_chunks_keeps_data_and_remeshes_on_return() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.world.set_block(5, 64, 5, BlockState::STONE); // chunk (0,0)
        gs.world.set_block(20 * 16 + 5, 64, 5, BlockState::STONE); // chunk (20,0)

        // Player in chunk (0,0), render distance 4 → keep radius 4 + 2 = 6.
        gs.player.position = DVec3::new(0.5, 64.0, 0.5);
        gs.dirty_chunks.clear();
        let removals = gs.enforce_render_distance(4);

        // The far column is dropped (all 16 sections), the near one kept.
        let far = ChunkPos::new(20, 0);
        assert_eq!(removals.iter().filter(|s| s.x == 20 && s.z == 0).count(), 16);
        assert!(!removals.iter().any(|s| s.x == 0 && s.z == 0));
        assert!(gs.evicted_columns.contains(&far));
        // Block data stays in the world, so returning never shows a hole.
        assert!(gs.world.chunk(far).is_some());

        // Standing still (same chunk) is a no-op.
        assert!(gs.enforce_render_distance(4).is_empty());

        // Walk to the far column: it returns to range (re-queued for meshing),
        // and the origin column is now the one evicted.
        gs.player.position = DVec3::new(20.0 * 16.0 + 0.5, 64.0, 0.5);
        gs.dirty_chunks.clear();
        let removals = gs.enforce_render_distance(4);
        assert!(!gs.evicted_columns.contains(&far));
        assert!(gs.dirty_chunks.iter().any(|s| s.x == 20 && s.z == 0));
        assert!(removals.iter().any(|s| s.x == 0 && s.z == 0));
        assert!(gs.evicted_columns.contains(&ChunkPos::new(0, 0)));
    }

    #[test]
    fn pickaxe_mines_stone_far_faster_than_bare_hand() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.player.on_ground = true;
        let stone = BlockState::new(1, 0);
        // Bare hand on stone: needs a tool → divisor 100, speed 1 → 1.5·100 = 150.
        assert_eq!(gs.block_break_ticks(stone), 150.0);
        // Diamond pickaxe (eff 8), harvestable → divisor 30 → ceil(1.5·30/8) = 6.
        gs.inventory[36] = Some(SlotItem::new(278, 1, 0));
        assert_eq!(gs.block_break_ticks(stone), 6.0);
        // Wooden pickaxe (eff 2) → ceil(1.5·30/2) = 23.
        gs.inventory[36] = Some(SlotItem::new(270, 1, 0));
        assert_eq!(gs.block_break_ticks(stone), 23.0);
    }

    #[test]
    fn obsidian_needs_a_diamond_pickaxe_to_harvest() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.player.on_ground = true;
        let obsidian = BlockState::new(49, 0); // hardness 50, harvest level 3
        // Stone pickaxe (tier 1 < 3): can't harvest → divisor 100, speed 4 → 1250.
        gs.inventory[36] = Some(SlotItem::new(274, 1, 0));
        assert_eq!(gs.block_break_ticks(obsidian), 1250.0);
        // Diamond pickaxe (tier 3): harvestable → divisor 30, speed 8 → ceil(187.5)=188.
        gs.inventory[36] = Some(SlotItem::new(278, 1, 0));
        assert_eq!(gs.block_break_ticks(obsidian), 188.0);
    }

    #[test]
    fn haste_and_air_penalty_scale_the_dig_speed() {
        let mut gs = GameState::empty_for_server(1.0);
        let stone = BlockState::new(1, 0);
        gs.inventory[36] = Some(SlotItem::new(278, 1, 0)); // diamond pickaxe
        gs.player.on_ground = true;
        assert_eq!(gs.block_break_ticks(stone), 6.0);
        // Haste I: speed ×1.2 → ceil(1.5·30/9.6) = 5.
        gs.effects.insert(3, ActiveEffect { amplifier: 0, duration: 0 });
        assert_eq!(gs.block_break_ticks(stone), 5.0);
        // Mining in the air divides speed by 5 (vanilla !onGround): 8/5 = 1.6 → ceil(28.125)=29.
        gs.effects.clear();
        gs.player.on_ground = false;
        assert_eq!(gs.block_break_ticks(stone), 29.0);
    }

    #[test]
    fn shovel_and_sword_have_their_own_speed_curves() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.player.on_ground = true;
        // Diamond shovel on dirt (hand-harvestable, hardness 0.5): eff 8, /30 → ceil(1.875)=2.
        let dirt = BlockState::new(3, 0);
        gs.inventory[36] = Some(SlotItem::new(277, 1, 0));
        assert_eq!(gs.block_break_ticks(dirt), 2.0);
        // A pickaxe is NOT effective on dirt → speed 1 → ceil(0.5·30)=15.
        gs.inventory[36] = Some(SlotItem::new(278, 1, 0));
        assert_eq!(gs.block_break_ticks(dirt), 15.0);
        // Sword shreds cobweb (15×); web hardness 4, hand-harvestable → ceil(4·30/15)=8.
        let web = BlockState::new(30, 0);
        gs.inventory[36] = Some(SlotItem::new(276, 1, 0));
        assert_eq!(gs.block_break_ticks(web), 8.0);
    }

    #[test]
    fn block_hardness_matches_vanilla_across_the_tiers() {
        // Spot-check the full 1.8.9 hardness table (copied from minecraft-data)
        // against known vanilla values, one per tier, to guard the big match.
        for (id, expected) in [
            (49u16, 50.0f32), // obsidian
            (130, 22.5),      // ender chest
            (52, 5.0),        // mob spawner
            (30, 4.0),        // cobweb
            (61, 3.5),        // furnace
            (14, 3.0),        // coal ore
            (54, 2.5),        // chest
            (4, 2.0),         // cobblestone
            (1, 1.5),         // stone
            (159, 1.25),      // stained hardened clay
            (63, 1.0),        // standing sign
            (35, 0.8),        // wool
            (97, 0.75),       // monster egg
            (27, 0.7),        // golden rail
            (2, 0.6),         // grass
            (3, 0.5),         // dirt
            (87, 0.4),        // netherrack
            (20, 0.3),        // glass
            (18, 0.2),        // leaves
            (171, 0.1),       // carpet
            (31, 0.0),        // tall grass (instant)
            (46, 0.0),        // tnt (instant)
        ] {
            assert!(
                (block_hardness(id) - expected).abs() < 1e-4,
                "block {id} hardness {} != vanilla {expected}",
                block_hardness(id)
            );
        }
        // Unbreakable tier is negative (bedrock, fluids, portals, command block).
        for id in [7u16, 8, 9, 10, 11, 90, 119, 120, 137, 166] {
            assert!(block_hardness(id) < 0.0, "block {id} should be unbreakable");
        }
        // Every in-range id resolves; none accidentally hits a wrong sign.
        for id in 1..=197u16 {
            let h = block_hardness(id);
            assert!(h.is_finite() && h >= -1.0, "block {id} hardness out of range: {h}");
        }
    }

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
    fn eating_plays_chew_sound_every_four_ticks_from_tick_eight() {
        let mut g = GameState::empty_for_server(1.0);
        g.inventory[36] = Some(item(297, 1)); // bread (EAT) in hotbar slot 0
        // Right-click in air starts the use (sendUseItem sets use_action = Eat).
        use_item(&mut g);
        let _ = g.take_sounds(); // drop any press-tick sounds
        let mut eat_ticks = Vec::new();
        for _ in 0..30 {
            act(
                &mut g,
                TickActions {
                    right_held: true,
                    ..Default::default()
                },
            );
            if g.take_sounds().iter().any(|s| s.event == "random.eat") {
                eat_ticks.push(g.use_item_ticks);
            }
        }
        // Vanilla itemInUseCount <= 25 && % 4 == 0, counting down from 32.
        assert_eq!(eat_ticks, vec![8, 12, 16, 20, 24, 28]);
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
    fn attack_while_blocking_swings_only_with_old_animations() {
        // Enter the sword block, aiming at air.
        let mut gs = looking_along_x();
        gs.inventory[36] = Some(SlotItem::new(276, 1, 0));
        use_item(&mut gs);
        assert_eq!(gs.use_action, ItemUseAction::Block);

        // 1.8 default: a left-click while blocking is swallowed — no swing.
        let p = act(
            &mut gs,
            TickActions {
                attack_pressed: true,
                left_held: true,
                right_held: true,
                old_animations: false,
                ..Default::default()
            },
        );
        assert!(
            !p.iter().any(|x| matches!(x, ServerboundPacket::SwingArm)),
            "1.8 must not swing while blocking, got {p:?}"
        );
        assert!(!gs.is_swinging);
        assert_eq!(gs.use_action, ItemUseAction::Block, "still blocking");

        // 1.7 (old_animations) is ANIMATION-ONLY: the same left-click starts the
        // local arm swing (for the "swing + block" visual) but sends NO packet —
        // the network stays vanilla 1.8 (block-hitting removed), so Grim still
        // sees a plain block. No SwingArm/UseEntity/PlayerDigging goes out.
        let p = act(
            &mut gs,
            TickActions {
                attack_pressed: true,
                left_held: true,
                right_held: true,
                old_animations: true,
                ..Default::default()
            },
        );
        assert!(
            p.is_empty(),
            "old_animations must not send any packet while blocking, got {p:?}"
        );
        assert!(gs.is_swinging, "but the local swing animation starts");
        assert_eq!(gs.use_action, ItemUseAction::Block, "block is not released");
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
        gs.tick(0.05).expect("not a freeze tick");
        let movement = gs.movement_snapshot();
        assert_eq!(gs.use_action, ItemUseAction::Block);
        assert!(!movement.sprinting, "blocking drops sprint within the tick");
    }

    #[test]
    fn sprint_key_with_forward_starts_and_releasing_forward_stops() {
        let mut gs = looking_along_x();
        gs.player.on_ground = true;
        gs.input.forward = true;
        gs.input.sprint = true; // toggle = keyBindSprint.isKeyDown()
        gs.tick(0.05).expect("tick");
        let m = gs.movement_snapshot();
        assert!(m.sprinting, "sprint key + forward starts sprinting");
        // Release forward: moveForward drops below 0.8 → stop.
        gs.input.forward = false;
        gs.tick(0.05).expect("tick");
        let m = gs.movement_snapshot();
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
        gs.tick(0.05).expect("tick");
        let m = gs.movement_snapshot();
        assert!(!m.sprinting, "pressing into a wall reports not-sprinting");
    }

    #[test]
    fn double_tap_forward_starts_sprint_without_the_sprint_key() {
        let mut gs = looking_along_x();
        gs.player.on_ground = true;
        gs.input.sprint = false; // no sprint key — only the double-tap can start
        // First fresh forward press: arms the 7-tick window, no sprint yet.
        gs.input.forward = true;
        gs.tick(0.05).expect("tick");
        let m1 = gs.movement_snapshot();
        assert!(!m1.sprinting, "first tap only arms sprintToggleTimer");
        // Release, then re-press within the window → sprint starts.
        gs.input.forward = false;
        let _ = gs.tick(0.05).expect("tick");
        gs.input.forward = true;
        gs.tick(0.05).expect("tick");
        let m3 = gs.movement_snapshot();
        assert!(m3.sprinting, "a second tap within 7 ticks starts the sprint");
    }

    #[test]
    fn sprint_attack_on_a_player_resets_sprint_and_halves_horizontal_motion() {
        // Hitting another PLAYER slows: EntityOtherPlayerMP.attackEntityFrom
        // returns true on the client, so the sprint hit lands.
        let mut gs = looking_along_x();
        gs.world.upsert_entity(EntityState::new_remote(
            EntityId(1),
            EntityKind::RemotePlayer,
            DVec3::new(2.0, 0.0, 0.5),
            0.0,
            0.0,
        ));
        gs.sprinting = true;
        gs.player.velocity = DVec3::new(1.0, 0.0, 2.0);
        let _ = attack(&mut gs);
        assert!(!gs.sprinting, "a sprint hit on a player cancels the sprint");
        assert!((gs.player.velocity.x - 0.6).abs() < 1e-9);
        assert!((gs.player.velocity.z - 1.2).abs() < 1e-9);
    }

    #[test]
    fn sprint_attack_on_a_mob_does_not_slow_or_cancel_sprint() {
        // Hitting a living mob does NOT slow on the client: EntityLivingBase's
        // attackEntityFrom short-circuits to false under `worldObj.isRemote`, so
        // motion and the sprint flag are untouched (Grim expects no attack-slow
        // here — applying one trips Simulation).
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
        assert!(gs.sprinting, "a mob hit must NOT cancel the sprint");
        assert_eq!(
            gs.player.velocity,
            DVec3::new(1.0, 0.0, 2.0),
            "a mob hit must NOT halve horizontal motion"
        );
    }

    #[test]
    fn sprint_attack_reenables_sprint_same_tick_when_holding_forward() {
        // Vanilla: a sprint-hit cancels sprint inside clickMouse, but the SAME
        // tick's onLivingUpdate re-enables it while W + the sprint key are held,
        // so isSprinting() is still true at onUpdateWalkingPlayer — no
        // StopSprinting is sent (the ×0.6 momentum is the only server-visible
        // effect). Suppressing the re-enable for a tick (the old
        // `sprint_reset_by_attack` path) emitted a StopSprinting → StartSprinting
        // pair vanilla never sends, which Grim flags as Simulation.
        let mut gs = looking_along_x();
        gs.player.yaw = -90.0; // the full tick recomputes the camera from this
        gs.player.on_ground = true;
        gs.sprinting = true;
        gs.input.sprint = true; // toggle stands in for keyBindSprint.isKeyDown()
        gs.input.forward = true;
        // A PLAYER target at eye level (camera ends up at ~y=81.6 after update);
        // attacking a player actually cancels sprint, so the re-enable is tested.
        gs.world.upsert_entity(EntityState::new_remote(
            EntityId(1),
            EntityKind::RemotePlayer,
            DVec3::new(2.0, 81.0, 0.5),
            0.0,
            0.0,
        ));
        gs.set_pending_actions(TickActions {
            attack_pressed: true,
            left_held: true,
            ..Default::default()
        });
        let packets = gs.tick(0.05).expect("not a freeze tick");
        let movement = gs.movement_snapshot();
        assert!(
            packets.iter().any(|p| matches!(
                p,
                ServerboundPacket::UseEntity {
                    target: 1,
                    kind: UseEntityKind::Attack
                }
            )),
            "the attack must land for this test to be meaningful: {packets:?}"
        );
        assert!(
            movement.sprinting,
            "sprint must re-enable the same tick so no StopSprinting is sent"
        );
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
        gs.player.on_ground = true; // standing: no vanilla in-air dig penalty
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
        use fpsmaster_protocol::v1_8_9::packets::{AttributeModifier, EntityProperty};
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
    fn max_health_attribute_feeds_hud_vitals() {
        use fpsmaster_protocol::v1_8_9::packets::EntityProperty;
        let mut gs = GameState::empty_for_server(1.0);
        gs.player.id = EntityId(7);
        gs.apply_play_packet(ClientboundPlayPacket::EntityProperties {
            entity_id: 7,
            properties: vec![EntityProperty {
                key: "generic.maxHealth".to_owned(),
                base: 40.0,
                modifiers: Vec::new(),
            }],
        });
        assert_eq!(gs.max_health, 40.0);
        assert_eq!(gs.hud_vitals().max_health, 40.0);

        // Another entity's maxHealth must not touch the local player.
        gs.apply_play_packet(ClientboundPlayPacket::EntityProperties {
            entity_id: 99,
            properties: vec![EntityProperty {
                key: "generic.maxHealth".to_owned(),
                base: 100.0,
                modifiers: Vec::new(),
            }],
        });
        assert_eq!(gs.max_health, 40.0);
    }

    #[test]
    fn absorption_effect_amplifier_drives_gold_hearts() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.player.id = EntityId(7);
        // Absorption II (amplifier 1) → 4 * (1 + 1) = 8 absorption health.
        gs.apply_play_packet(ClientboundPlayPacket::EntityEffect {
            entity_id: 7,
            effect_id: POTION_ABSORPTION as i8,
            amplifier: 1,
            duration: 600,
            hide_particles: 0,
        });
        assert_eq!(gs.hud_vitals().absorption, 8.0);

        // Removing it clears the gold hearts.
        gs.apply_play_packet(ClientboundPlayPacket::RemoveEntityEffect {
            entity_id: 7,
            effect_id: POTION_ABSORPTION as i8,
        });
        assert_eq!(gs.hud_vitals().absorption, 0.0);
    }

    #[test]
    fn potion_effect_flags_thread_into_hud_vitals() {
        let mut gs = GameState::empty_for_server(1.0);
        gs.player.id = EntityId(7);
        for id in [POTION_REGENERATION, POTION_HUNGER, POTION_POISON, POTION_WITHER] {
            gs.apply_play_packet(ClientboundPlayPacket::EntityEffect {
                entity_id: 7,
                effect_id: id as i8,
                amplifier: 0,
                duration: 200,
                hide_particles: 0,
            });
        }
        let v = gs.hud_vitals();
        assert!(v.regen && v.hunger_effect && v.poison && v.wither);
        // The stored duration round-trips through the effect table.
        assert_eq!(gs.effects[&POTION_POISON].duration, 200);

        // Effects on a non-local entity are ignored.
        gs.apply_play_packet(ClientboundPlayPacket::EntityEffect {
            entity_id: 99,
            effect_id: POTION_ABSORPTION as i8,
            amplifier: 4,
            duration: 200,
            hide_particles: 0,
        });
        assert_eq!(gs.hud_vitals().absorption, 0.0);
    }

    #[test]
    fn respawn_clears_effects_and_resets_max_health() {
        use fpsmaster_protocol::v1_8_9::packets::EntityProperty;
        let mut gs = GameState::empty_for_server(1.0);
        gs.player.id = EntityId(7);
        gs.apply_play_packet(ClientboundPlayPacket::EntityProperties {
            entity_id: 7,
            properties: vec![EntityProperty {
                key: "generic.maxHealth".to_owned(),
                base: 30.0,
                modifiers: Vec::new(),
            }],
        });
        gs.apply_play_packet(ClientboundPlayPacket::EntityEffect {
            entity_id: 7,
            effect_id: POTION_ABSORPTION as i8,
            amplifier: 0,
            duration: 600,
            hide_particles: 0,
        });
        assert_eq!(gs.max_health, 30.0);
        assert!(!gs.effects.is_empty());

        gs.apply_play_packet(ClientboundPlayPacket::Respawn {
            dimension: 0,
            difficulty: 0,
            game_mode: 0,
            level_type: "default".to_owned(),
        });
        assert_eq!(gs.max_health, 20.0);
        assert!(gs.effects.is_empty());
    }

    #[test]
    fn attribute_operations_follow_vanilla_compute_value() {
        use fpsmaster_protocol::v1_8_9::packets::{AttributeModifier, EntityProperty};
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
        use fpsmaster_protocol::v1_8_9::packets::MetadataEntry;
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
        let mut glint = ModelMesh::new();
        let skins = std::collections::HashMap::new();
        g.build_entity_model(&mut mesh, &mut glint, 1.0, 1.0, &skins, f64::INFINITY, false);
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
        use fpsmaster_protocol::v1_8_9::packets::MetadataEntry;
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
        let mut bare_glint = ModelMesh::new();
        g.build_entity_model(&mut bare, &mut bare_glint, 1.0, 1.0, &skins, f64::INFINITY, false);
        assert!(bare.is_empty(), "invisible player with no armor renders nothing");

        // Give it an iron helmet (slot 4, id 306): the worn armor still shows.
        g.apply_play_packet(ClientboundPlayPacket::EntityEquipment {
            entity_id: 8,
            slot: 4,
            item: Some(SlotItem::new(306, 1, 0)),
        });
        let mut armored = ModelMesh::new();
        let mut armored_glint = ModelMesh::new();
        g.build_entity_model(&mut armored, &mut armored_glint, 1.0, 1.0, &skins, f64::INFINITY, false);
        assert!(!armored.is_empty(), "invisible player must still show worn armor");
        // The plain (unenchanted) helmet emits no glint geometry.
        assert!(armored_glint.is_empty(), "unenchanted armor must not glint");

        // Enchant the helmet (non-empty `ench` tag): now it glints.
        use fpsmaster_protocol::nbt::NbtTag;
        let mut nbt = std::collections::HashMap::new();
        let mut ench = std::collections::HashMap::new();
        ench.insert("id".to_string(), NbtTag::Short(0));
        ench.insert("lvl".to_string(), NbtTag::Short(4));
        nbt.insert("ench".to_string(), NbtTag::List(vec![NbtTag::Compound(ench)]));
        let mut helmet = SlotItem::new(306, 1, 0);
        helmet.nbt = Some(nbt);
        g.apply_play_packet(ClientboundPlayPacket::EntityEquipment {
            entity_id: 8,
            slot: 4,
            item: Some(helmet),
        });
        let mut ench_mesh = ModelMesh::new();
        let mut ench_glint = ModelMesh::new();
        g.build_entity_model(&mut ench_mesh, &mut ench_glint, 1.0, 1.0, &skins, f64::INFINITY, false);
        assert!(!ench_glint.is_empty(), "enchanted armor must emit glint geometry");
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
    fn primed_tnt_swells_and_flashes_with_its_fuse() {
        let mut g = GameState::empty_for_server(1.0);
        g.apply_play_packet(ClientboundPlayPacket::SpawnObject {
            entity_id: 7,
            kind: 50, // primed TNT
            x: 0.5,
            y: 80.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            data: 0,
            velocity: None,
        });
        // Fresh fuse (age 0 → fuse 80): a TNT block, no swell, flash window on
        // (80 / 5 = 16, even).
        let cubes = g.primed_tnt_cubes(0.0);
        assert_eq!(cubes.len(), 1);
        assert_eq!(cubes[0].block, BlockState::new(46, 0));
        assert!((cubes[0].scale - 1.0).abs() < 1e-6, "no swell early in the fuse");
        assert!(cubes[0].flash > 0.0, "fuse 80 is in a flash window");

        // age 5 → fuse 75 (75 / 5 = 15, odd): between flash windows, still no swell.
        let mut e = g.world.entity(EntityId(7)).unwrap().clone();
        e.age = 5;
        g.world.upsert_entity(e);
        let cubes = g.primed_tnt_cubes(0.0);
        assert_eq!(cubes[0].flash, 0.0, "fuse 75 is between flash windows");
        assert!((cubes[0].scale - 1.0).abs() < 1e-6);

        // age 78 → fuse 2 (< 10): swelling toward ×1.3.
        let mut e = g.world.entity(EntityId(7)).unwrap().clone();
        e.age = 78;
        g.world.upsert_entity(e);
        let cubes = g.primed_tnt_cubes(0.0);
        assert!(
            cubes[0].scale > 1.0 && cubes[0].scale <= 1.3,
            "swells in the last 10 fuse ticks: {}",
            cubes[0].scale
        );
    }

    #[test]
    fn projectile_kinds_map_to_item_sprites() {
        assert_eq!(projectile_item_id(60), None); // arrow is a 3D model, not a sprite
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
    fn adjacent_same_type_chests_render_one_large_model() {
        let mut g = GameState::empty_for_server(1.0);
        // Camera at the origin column looking +Z (yaw 0) at chests a few blocks
        // ahead, so they sit inside the frustum and the distance cutoff.
        g.camera.position = Vec3::new(0.5, 80.0, 0.0);

        // A normal-chest pair adjacent along X (facing south, meta 3): the
        // canonical half is the smaller-X one; both share chunk (0,0).
        g.world.set_block(0, 79, 6, BlockState::new(54, 3));
        g.world.set_block(1, 79, 6, BlockState::new(54, 3));

        let mut mesh = ModelMesh::new();
        g.build_chest_models(&mut mesh, 1.0, 0.0, 4096.0);
        // One large chest = 3 boxes = 72 verts (two singles would be 144).
        assert_eq!(
            mesh.vertices.len(),
            72,
            "an adjacent same-type pair must emit one large model, not two singles"
        );
        // The model spans both cells along X (0..2), confirming the large box.
        let lo = mesh.vertices.iter().map(|v| v.position[0]).fold(f32::MAX, f32::min);
        let hi = mesh.vertices.iter().map(|v| v.position[0]).fold(f32::MIN, f32::max);
        assert!(hi - lo > 1.5, "large chest must span two cells along X ({lo}..{hi})");

        // A lone chest of the same type (no same-id X neighbour) stays single.
        let mut g2 = GameState::empty_for_server(1.0);
        g2.camera.position = Vec3::new(0.5, 80.0, 0.0);
        g2.world.set_block(0, 79, 6, BlockState::new(54, 3));
        let mut single = ModelMesh::new();
        g2.build_chest_models(&mut single, 1.0, 0.0, 4096.0);
        assert_eq!(single.vertices.len(), 72, "a lone chest must emit one single model");
        let s_lo = single.vertices.iter().map(|v| v.position[0]).fold(f32::MAX, f32::min);
        let s_hi = single.vertices.iter().map(|v| v.position[0]).fold(f32::MIN, f32::max);
        assert!(s_hi - s_lo < 1.1, "single chest must fit in one cell ({s_lo}..{s_hi})");

        // Two adjacent chests of DIFFERENT types (normal + trapped) do not pair:
        // each renders its own single model (2 × 72 verts).
        let mut g3 = GameState::empty_for_server(1.0);
        g3.camera.position = Vec3::new(0.5, 80.0, 0.0);
        g3.world.set_block(0, 79, 6, BlockState::new(54, 3));
        g3.world.set_block(1, 79, 6, BlockState::new(146, 3));
        let mut mixed = ModelMesh::new();
        g3.build_chest_models(&mut mixed, 1.0, 0.0, 4096.0);
        assert_eq!(mixed.vertices.len(), 144, "different-type neighbours stay two singles");
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
        use fpsmaster_protocol::v1_8_9::packets::MetadataEntry;
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

    #[test]
    fn silent_yaw_remap_keeps_idle_and_magnitude() {
        // Idle stays idle (no spurious movement under a silent look).
        assert_eq!(remap_input_to_yaw(0.0, 0.0, 0.0, 90.0), (0.0, 0.0));
        // The input magnitude (here a use-item 0.2) is preserved, only the
        // direction (sign pattern) is remapped.
        let (f, s) = remap_input_to_yaw(0.2, 0.0, 0.0, 90.0);
        assert!((f.abs().max(s.abs()) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn quantize_rotation_stays_on_step_lattice() {
        let step = 0.15_f32; // default-sensitivity mouse factor
        let base = 12.3_f32;
        for &target in &[12.34_f32, 45.7, -30.2, 357.0, 12.3] {
            let q = quantize_rotation(target, base, step);
            // On the base + n*step lattice (so deltas stay multiples of step).
            let n = (q - base) / step;
            assert!((n - n.round()).abs() < 1e-2, "off lattice: target={target} n={n}");
            // And within step/2 of the requested aim (shortest turn).
            let err = wrap_degrees(q - target).abs();
            assert!(err <= step / 2.0 + 1e-3, "aim error {err} exceeds step/2");
        }
        // A non-positive step is a no-op (no quantisation data).
        assert_eq!(quantize_rotation(40.0, 0.0, 0.0), 40.0);
    }

    #[test]
    fn silent_yaw_remap_preserves_world_direction() {
        // Walking forward (world +Z) while the server sees a yaw rotated by a
        // multiple of 45° must still move the player toward world +Z — the remap
        // picks the legal input under the silent yaw that points the same way.
        for &ys in &[45.0_f32, 90.0, 135.0, 180.0, -90.0] {
            let (f, s) = remap_input_to_yaw(1.0, 0.0, 0.0, ys);
            let intended = movement_direction(1.0, 0.0, 0.0);
            let got = movement_direction(f, s, ys);
            let cos = (intended.x * got.x + intended.z * got.z)
                / (intended.length() * got.length());
            assert!(cos > 0.92, "silent yaw {ys}: world dir drifted, cos={cos}");
        }
    }

    // ─── Sound triggers (music / ambient / moving / local prediction) ─────────

    /// Stand the local player on a stone floor at an integer column, on the
    /// ground, and clear any queued sounds so a test starts from a clean slate.
    fn stand_on_stone(g: &mut GameState, x: i32, y: i32, z: i32) {
        for dz in -4..=4 {
            for dx in -4..=4 {
                g.world.set_block(x + dx, y - 1, z + dz, BlockState::new(1, 0));
            }
        }
        g.player.position = DVec3::new(x as f64 + 0.5, y as f64, z as f64 + 0.5);
        g.previous_player_position = g.player.position;
        g.player.on_ground = true;
        g.prev_on_ground = true;
        g.player.sync_aabb_to_position();
        g.camera.position = to_render_vec3(g.player.position);
        let _ = g.take_sounds();
    }

    #[test]
    fn footstep_plays_block_step_sound_at_the_vanilla_interval() {
        let mut g = GameState::empty_for_server(1.0);
        stand_on_stone(&mut g, 0, 65, 0);
        // Walk a bit over one tick: the accumulator is horizontal ×0.6, so ~1.7
        // blocks trips the first step (nextStepDistance starts at 1).
        g.previous_player_position = g.player.position;
        g.player.position.x += 2.0;
        g.camera.position = to_render_vec3(g.player.position);
        g.tick_local_movement_sounds();
        let sounds = g.take_sounds();
        assert_eq!(sounds.len(), 1, "one footstep expected");
        assert_eq!(sounds[0].event, "step.stone");
        assert!((sounds[0].volume - 0.15).abs() < 1e-6);
    }

    #[test]
    fn no_footstep_while_airborne() {
        let mut g = GameState::empty_for_server(1.0);
        stand_on_stone(&mut g, 0, 65, 0);
        g.player.on_ground = false;
        g.previous_player_position = g.player.position;
        g.player.position.x += 2.0;
        g.tick_local_movement_sounds();
        assert!(
            g.take_sounds().iter().all(|s| !s.event.starts_with("step.")),
            "no footstep off the ground"
        );
    }

    #[test]
    fn fall_landing_plays_hurt_sound_past_three_blocks() {
        let mut g = GameState::empty_for_server(1.0);
        stand_on_stone(&mut g, 0, 65, 0);
        // Accumulate a >3 block fall while airborne, then land.
        g.player.on_ground = false;
        g.prev_on_ground = false;
        g.fall_distance = 5.0;
        g.previous_player_position = g.player.position;
        g.player.on_ground = true; // touchdown edge this tick
        g.tick_local_movement_sounds();
        let sounds = g.take_sounds();
        assert!(
            sounds.iter().any(|s| s.event == "game.player.hurt.fall.small"),
            "a 5-block fall plays the small fall-hurt sound: {sounds:?}"
        );
    }

    #[test]
    fn short_fall_is_silent() {
        let mut g = GameState::empty_for_server(1.0);
        stand_on_stone(&mut g, 0, 65, 0);
        g.player.on_ground = false;
        g.prev_on_ground = false;
        g.fall_distance = 2.0; // below the 3-block threshold
        g.player.on_ground = true;
        g.tick_local_movement_sounds();
        assert!(
            !g.take_sounds().iter().any(|s| s.event.starts_with("game.player.hurt.fall")),
            "a 2-block fall makes no fall-hurt sound"
        );
    }

    #[test]
    fn collect_item_plays_pop_for_local_player() {
        let mut g = GameState::empty_for_server(1.0);
        g.player.id = EntityId(1);
        // An item entity to collect (object kind 2 = item).
        g.spawn_remote_entity(9, EntityKind::Object(2), 3.0, 65.0, 3.0, 0.0, 0.0);
        let _ = g.take_sounds();
        g.apply_play_packet(ClientboundPlayPacket::CollectItem {
            collected_id: 9,
            collector_id: 1,
        });
        let sounds = g.take_sounds();
        assert_eq!(sounds.len(), 1);
        assert_eq!(sounds[0].event, "random.pop");
    }

    #[test]
    fn collect_xp_orb_plays_orb_sound() {
        let mut g = GameState::empty_for_server(1.0);
        g.player.id = EntityId(1);
        g.apply_play_packet(ClientboundPlayPacket::SpawnExperienceOrb {
            entity_id: 8,
            x: 2.0,
            y: 65.0,
            z: 0.0,
            count: 5,
        });
        let _ = g.take_sounds();
        g.apply_play_packet(ClientboundPlayPacket::CollectItem {
            collected_id: 8,
            collector_id: 1,
        });
        let sounds = g.take_sounds();
        assert_eq!(sounds.len(), 1);
        assert_eq!(sounds[0].event, "random.orb");
    }

    #[test]
    fn collect_by_other_player_is_silent() {
        let mut g = GameState::empty_for_server(1.0);
        g.player.id = EntityId(1);
        g.spawn_remote_entity(9, EntityKind::Object(2), 3.0, 65.0, 3.0, 0.0, 0.0);
        let _ = g.take_sounds();
        // Collector 42 is not us.
        g.apply_play_packet(ClientboundPlayPacket::CollectItem {
            collected_id: 9,
            collector_id: 42,
        });
        assert!(g.take_sounds().is_empty());
    }

    #[test]
    fn ambient_cave_triggers_on_a_dark_air_pocket() {
        let mut g = GameState::empty_for_server(1.0);
        g.joined_game = true;
        g.player.position = DVec3::new(8.5, 40.0, 8.5);
        g.previous_player_position = g.player.position;
        // Force the countdown to fire and drive the LCG to a known dark-air cell.
        // Rather than reverse the LCG, fill the whole chunk section with air at a
        // low, sky-blocked light so whatever cell it samples qualifies, then put
        // the player far from it vertically so the >4 block check passes.
        g.mood_tick_countdown = 0;
        // Load every section of the column as all-air with 0 light, so whatever
        // y the LCG samples reads as dark (sky-blocked) air.
        for sy in 0..16 {
            g.world
                .load_section(0, 0, sy, &[0u16; 4096], &[0u8; 2048], &[0u8; 2048]);
        }
        // Ensure the column reports loaded.
        assert!(g.world.is_block_column_loaded(8, 8));
        let mut fired = false;
        // The sampled y may not be far enough on the first roll; tick a few times.
        for _ in 0..64 {
            g.mood_tick_countdown = 0;
            g.tick_ambient_mood();
            if g.take_sounds().iter().any(|s| s.event == "ambient.cave.cave") {
                fired = true;
                break;
            }
        }
        assert!(fired, "a dark air pocket should eventually play ambient.cave.cave");
        // After a successful play the cooldown is rearmed into the 5–15 min band.
        assert!((6000..=18000).contains(&g.mood_tick_countdown));
    }

    #[test]
    fn music_ticker_starts_a_game_track_and_reschedules_on_finish() {
        let mut g = GameState::empty_for_server(1.0);
        g.joined_game = true;
        g.dimension = 0;
        // Drive the delay down to zero; the ticker should emit one Play command.
        g.music_ticker.delay_ticks = 1;
        let wanted = g.desired_music_type();
        assert_eq!(wanted, MusicType::Game);
        g.music_ticker.tick(wanted);
        let cmds = g.take_music_commands();
        assert!(
            matches!(cmds.as_slice(), [MusicCommand::Play(e)] if e == "music.game"),
            "expected a single Play(music.game): {cmds:?}"
        );
        // While the host reports the track playing, no new command is emitted.
        g.music_ticker.tick(wanted);
        assert!(g.take_music_commands().is_empty());
        // When the track finishes, the ticker clears it and waits out a delay.
        g.set_music_playing(false);
        g.music_ticker.tick(wanted);
        assert!(g.take_music_commands().is_empty(), "a finished track just schedules a delay");
        assert!(g.music_ticker.current.is_none());
    }

    #[test]
    fn music_ticker_switches_track_when_type_changes() {
        let mut g = GameState::empty_for_server(1.0);
        g.joined_game = true;
        g.dimension = 0;
        g.music_ticker.delay_ticks = 1;
        g.music_ticker.tick(MusicType::Game);
        let _ = g.take_music_commands(); // consume the initial Play
        // Now the world is the nether: the ticker stops the game track.
        g.music_ticker.tick(MusicType::Nether);
        let cmds = g.take_music_commands();
        assert!(
            cmds.iter().any(|c| matches!(c, MusicCommand::Stop)),
            "a music-type change stops the current track: {cmds:?}"
        );
    }

    #[test]
    fn minecart_spawn_attaches_a_looping_moving_sound() {
        let mut g = GameState::empty_for_server(1.0);
        g.joined_game = true;
        // Spawn a rideable minecart (object kind 10).
        g.apply_play_packet(ClientboundPlayPacket::SpawnObject {
            entity_id: 20,
            kind: 10,
            x: 4.0,
            y: 65.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            data: 0,
            velocity: None,
        });
        g.refresh_entity_sound_targets();
        let cmds = g.take_moving_sound_commands();
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                MovingSoundCommand::Attach { event, looping, .. }
                    if event == "minecart.base" && *looping
            )),
            "a minecart gets a looping minecart.base emitter: {cmds:?}"
        );
        // A second refresh does not double-attach.
        g.refresh_entity_sound_targets();
        assert!(
            !g.take_moving_sound_commands()
                .iter()
                .any(|c| matches!(c, MovingSoundCommand::Attach { .. })),
            "the minecart sound is attached only once"
        );
        // Destroying the cart stops its emitter.
        g.apply_play_packet(ClientboundPlayPacket::DestroyEntities { entity_ids: vec![20] });
        let cmds = g.take_moving_sound_commands();
        assert!(
            cmds.iter().any(|c| matches!(c, MovingSoundCommand::Stop { .. })),
            "destroying the cart stops the emitter: {cmds:?}"
        );
    }
}


#[cfg(test)]
mod weather_tests {
    use super::*;

    fn run(weather: &mut Weather, ticks: usize) {
        for _ in 0..ticks {
            weather.tick();
        }
    }

    #[test]
    fn weather_ramps_instead_of_popping() {
        let mut w = Weather::default();
        assert_eq!(w.rain_strength(1.0), 0.0);

        w.set_raining(true);
        run(&mut w, 50);
        let half = w.rain_strength(1.0);
        assert!(
            (half - 0.5).abs() < 0.02,
            "0.01/tick puts 50 ticks at ~half: {half}"
        );

        // Vanilla takes 100 ticks (5 s) to reach full, then clamps.
        run(&mut w, 60);
        assert_eq!(w.rain_strength(1.0), 1.0);

        w.set_raining(false);
        run(&mut w, 110);
        assert_eq!(w.rain_strength(1.0), 0.0, "and fades back out");
    }

    #[test]
    fn thunder_implies_rain_and_clearing_rain_clears_thunder() {
        let mut w = Weather::default();
        w.set_thundering(true);
        assert!(w.is_raining(), "a thunderstorm is rain plus thunder");

        // 0.01 accumulated in f32 lands a hair under 1.0 at exactly 100 ticks,
        // so the clamp has not fired yet — compare with a tolerance rather than
        // pretending the ramp is exact.
        run(&mut w, 100);
        assert!((w.rain_strength(1.0) - 1.0).abs() < 1e-4);
        assert!((w.thunder_strength(1.0) - 1.0).abs() < 1e-4);

        w.set_raining(false);
        assert!(!w.is_thundering(), "thunder cannot outlive its rain");
        run(&mut w, 100);
        assert!(w.thunder_strength(1.0) < 1e-4);
    }

    #[test]
    fn thunder_is_gated_by_the_rain_level() {
        // Vanilla getThunderStrength multiplies by the rain strength, so thunder
        // can never darken a sky that is not already overcast.
        let mut w = Weather::default();
        w.set_thundering(true);
        run(&mut w, 20);
        let rain = w.rain_strength(1.0);
        assert!(w.thunder_strength(1.0) <= rain + 1e-6);
        assert!(w.thunder_strength(1.0) < 0.05, "still barely thundering");
    }

    #[test]
    fn explicit_levels_bypass_the_ramp() {
        // S2B reason 7/8 set the level outright; no interpolation artefact.
        let mut w = Weather::default();
        w.set_rain_level(0.75);
        assert_eq!(w.rain_strength(0.0), 0.75);
        assert_eq!(w.rain_strength(1.0), 0.75);
        w.set_thunder_level(1.0);
        assert_eq!(w.thunder_strength(1.0), 0.75, "gated by rain");
    }

    #[test]
    fn lightning_flash_lasts_two_ticks() {
        let mut w = Weather::default();
        assert!(!w.lightning_flash());
        w.flash();
        assert!(w.lightning_flash());
        w.tick();
        assert!(w.lightning_flash(), "still lit on the second tick");
        w.tick();
        assert!(!w.lightning_flash());
    }
}

#[cfg(test)]
mod snow_weather_tests {
    use super::*;

    #[test]
    fn snow_forces_rain_on_and_clear_cancels_it() {
        let mut w = Weather::default();
        assert!(!w.force_snow());
        w.set_force_snow(true);
        assert!(w.is_raining(), "snow is precipitation, so it must be raining");
        assert!(w.force_snow());
        w.set_force_snow(false);
        w.set_raining(false);
        assert!(!w.force_snow());
    }

    #[test]
    fn puddles_fill_far_slower_than_rain_and_drain_slower_still() {
        let mut w = Weather::default();
        w.set_raining(true);
        // Rain is full in 100 ticks; puddles must be nowhere near it, or they
        // snap in with the weather change instead of pooling.
        for _ in 0..100 {
            w.tick();
        }
        assert!((w.rain_strength(1.0) - 1.0).abs() < 1e-3);
        let after_5s = w.puddle_level(1.0);
        assert!(
            after_5s < 0.3,
            "puddles barely started after 5s of rain: {after_5s}"
        );

        // Not full at 20s: the fill rate scales with the rain actually falling,
        // so the 5-second ramp-up only contributes at half rate. Full lands
        // nearer 22s.
        for _ in 0..300 {
            w.tick();
        }
        let at_20s = w.puddle_level(1.0);
        assert!((0.8..1.0).contains(&at_20s), "{at_20s}");
        for _ in 0..100 {
            w.tick();
        }
        assert!((w.puddle_level(1.0) - 1.0).abs() < 1e-3, "full by ~25s");

        // Draining is slower than filling, so pools linger after the rain stops.
        w.set_raining(false);
        for _ in 0..200 {
            w.tick();
        }
        let left = w.puddle_level(1.0);
        assert!(
            left > 0.5,
            "10s after the rain stops most water is still there: {left}"
        );
    }

    #[test]
    fn a_brief_shower_leaves_only_a_trace() {
        // Fill rate scales with the rain actually falling, so weather that ramps
        // up and straight back down should not pool.
        let mut w = Weather::default();
        w.set_raining(true);
        for _ in 0..40 {
            w.tick();
        }
        w.set_raining(false);
        for _ in 0..40 {
            w.tick();
        }
        assert!(w.puddle_level(1.0) < 0.1, "{}", w.puddle_level(1.0));
    }
}

#[cfg(test)]
mod precipitation_height_tests {
    use super::*;

    fn state_with(blocks: &[(i32, i32, i32)]) -> GameState {
        let mut world = World::new();
        // Touch the column so the chunk exists even when nothing is placed.
        world.set_block(0, 0, 0, BlockState::AIR);
        for &(x, y, z) in blocks {
            world.set_block(x, y, z, BlockState::STONE);
        }
        GameState::new(world, EntityId(0), DVec3::new(0.5, 80.0, 0.5), 1.6)
    }

    #[test]
    fn returns_one_above_the_topmost_solid_block() {
        let state = state_with(&[(4, 60, 4), (4, 71, 4), (4, 12, 4)]);
        assert_eq!(state.precipitation_height(4, 4), 72);
    }

    #[test]
    fn an_empty_or_unloaded_column_is_zero() {
        let state = state_with(&[(4, 60, 4)]);
        // Loaded chunk, but this column has nothing in it.
        assert_eq!(state.precipitation_height(9, 9), 0);
        // Chunk never loaded at all.
        assert_eq!(state.precipitation_height(500, 500), 0);
    }

    #[test]
    fn finds_blocks_above_the_topmost_allocated_section_boundary() {
        // The scan starts at the top of the highest allocated section; a block
        // sitting in that section's last row must still be found, and one in a
        // section allocated later must lift the answer.
        let state = state_with(&[(4, 60, 4)]);
        assert_eq!(state.precipitation_height(4, 4), 61);
        let taller = state_with(&[(4, 60, 4), (4, 255, 4)]);
        assert_eq!(taller.precipitation_height(4, 4), 256);
    }
}
