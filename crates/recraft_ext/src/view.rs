//! Read-views: stable, copyable snapshots of game state, plus the [`ReadViews`]
//! trait the host implements so mods can query live state from inside a hook
//! without the host snapshotting the whole world every tick.

/// A copyable snapshot of the local player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerView {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub vx: f64,
    pub vy: f64,
    pub vz: f64,
    pub on_ground: bool,
    pub health: f32,
    pub food: i32,
    pub sneaking: bool,
    pub sprinting: bool,
}

/// A single block state (1.8 pre-flattening: numeric id + 4-bit meta).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockView {
    pub id: u16,
    pub meta: u8,
}

impl BlockView {
    pub const AIR: BlockView = BlockView { id: 0, meta: 0 };

    /// Highest id a 1.8.9 vanilla server can send; ids above behave as air.
    pub const MAX_VANILLA_ID: u16 = 197;

    pub fn is_air(&self) -> bool {
        self.id == 0 || self.id > Self::MAX_VANILLA_ID
    }
}

/// Discriminates an entity's category for a read-view / event payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKindView {
    Player,
    /// Carries the 1.8 SpawnMob network type id (e.g. 54 = zombie).
    Mob(u8),
    /// Carries the 1.8 SpawnObject network type id (e.g. 2 = dropped item).
    Object(u8),
    ExperienceOrb,
}

/// A copyable snapshot of a remote entity.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityView {
    pub id: i32,
    pub kind: EntityKindView,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub name: Option<String>,
    pub health: Option<f32>,
}

/// Live, read-only access to game state. Implemented by the host
/// (`recraft_app`) over its `GameState`, and handed to every hook so a mod can
/// answer arbitrary queries (e.g. `block_at`) without the host eagerly
/// snapshotting. The host promises these run on the main thread, so reads see a
/// consistent state for the duration of one hook call.
pub trait ReadViews {
    fn player(&self) -> PlayerView;
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockView;
    fn entities(&self) -> Vec<EntityView>;
    fn entity(&self, id: i32) -> Option<EntityView>;
    fn world_time(&self) -> i64;
    fn dimension(&self) -> i32;
    fn loaded_chunk_count(&self) -> usize;
}
