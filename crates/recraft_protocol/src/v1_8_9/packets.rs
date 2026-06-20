use crate::{
    codec::PacketFrame,
    io::{PacketReader, PacketWriter, ProtocolError, Result},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextState {
    Status = 1,
    Login = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityAction {
    StartSneaking = 0,
    StopSneaking = 1,
    StopSleeping = 2,
    StartSprinting = 3,
    StopSprinting = 4,
    RidingJump = 5,
    OpenInventory = 6,
}

/// 1.8 C02 UseEntity action discriminant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UseEntityKind {
    Interact,
    Attack,
    /// Right-click at a specific point on the entity (cursor offset).
    InteractAt {
        x: f32,
        y: f32,
        z: f32,
    },
}

/// 1.8 C07 PlayerDigging status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiggingStatus {
    StartDestroy = 0,
    CancelDestroy = 1,
    FinishDestroy = 2,
    DropItemStack = 3,
    DropItem = 4,
    ReleaseUseItem = 5,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerboundPacket {
    Handshake {
        protocol_version: i32,
        host: String,
        port: u16,
        next_state: NextState,
    },
    LoginStart {
        username: String,
    },
    KeepAlive {
        id: i32,
    },
    /// C15 ClientSettings — sent once after JoinGame so the server treats us as
    /// a fully initialised client (locale, view distance, shown skin parts).
    ClientSettings {
        locale: String,
        view_distance: i8,
        chat_mode: i8,
        chat_colors: bool,
        skin_parts: u8,
    },
    /// C17 PluginMessage — used here to announce the client brand on MC|Brand.
    PluginMessage {
        channel: String,
        data: Vec<u8>,
    },
    Player {
        on_ground: bool,
    },
    PlayerPosition {
        x: f64,
        y: f64,
        z: f64,
        on_ground: bool,
    },
    PlayerLook {
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },
    PlayerPositionLook {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },
    EntityAction {
        entity_id: i32,
        action: EntityAction,
        aux_data: i32,
    },
    UseEntity {
        target: i32,
        kind: UseEntityKind,
    },
    PlayerDigging {
        status: DiggingStatus,
        x: i32,
        y: i32,
        z: i32,
        face: u8,
    },
    PlayerBlockPlacement {
        x: i32,
        y: i32,
        z: i32,
        face: u8,
        /// Held item slot; `None` encodes the "empty slot" (-1) the vanilla
        /// client sends when the hand carries no trackable item.
        held_item: Option<HeldItem>,
        cursor_x: u8,
        cursor_y: u8,
        cursor_z: u8,
    },
    HeldItemChange {
        slot: i16,
    },
    /// C0E ClickWindow — a slot click in an open window. `action_number` is the
    /// per-window transaction counter the server echoes via ConfirmTransaction;
    /// `mode`/`button` select the click kind (normal/shift/number/drag/drop).
    ClickWindow {
        window_id: u8,
        slot: i16,
        button: i8,
        action_number: i16,
        mode: i8,
        clicked_item: Option<SlotItem>,
    },
    /// C0D CloseWindow — the client closed the window (also sent for the
    /// player inventory, window id 0).
    CloseWindow {
        window_id: u8,
    },
    /// C10 CreativeInventoryAction — set a slot directly (creative mode is
    /// client-authoritative over the inventory, no transaction).
    CreativeInventoryAction {
        slot: i16,
        clicked_item: Option<SlotItem>,
    },
    /// C0A Animation — swing the main arm (no payload in 1.8).
    SwingArm,
    /// C16 ClientStatus — action 0 performs a respawn (sent after death).
    ClientStatus {
        action: i32,
    },
    /// C0F ConfirmTransaction — echoes a server transaction (window-confirm)
    /// back so the server can measure round-trip timing. Vanilla replies to any
    /// unaccepted transaction with `accepted = true`; anti-cheats (Grim) rely on
    /// this pong for their timing and setback machinery.
    ConfirmTransaction {
        window_id: i8,
        action_number: i16,
        accepted: bool,
    },
    /// C01 ChatMessage — a plain chat string (commands start with '/'); the
    /// 1.8 server kicks for messages longer than 100 characters.
    ChatMessage {
        message: String,
    },
    /// C13 PlayerAbilities — sent when the client toggles flight (double-tap
    /// jump / landing). Vanilla echoes back the full capability set it last
    /// received via S39, with only the `flying` bit under client control.
    PlayerAbilities {
        invulnerable: bool,
        flying: bool,
        allow_flying: bool,
        creative: bool,
        fly_speed: f32,
        walk_speed: f32,
    },
    /// C14 TabComplete — ask the server to complete the chat-box text. The
    /// chat box never carries a looked-at block, so `has_position` is always
    /// false here and no block position is written.
    TabComplete {
        text: String,
        has_position: bool,
    },
}

/// Minimal Slot data for a held item in a block-placement packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldItem {
    pub id: i16,
    pub count: u8,
    pub damage: i16,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientboundLoginPacket {
    Disconnect {
        reason_json: String,
    },
    EncryptionRequest {
        server_id: String,
        public_key: Vec<u8>,
        verify_token: Vec<u8>,
    },
    LoginSuccess {
        uuid: String,
        username: String,
    },
    SetCompression {
        threshold: i32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct BulkChunkData {
    pub x: i32,
    pub z: i32,
    pub primary_bit_mask: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockChangeRecord {
    pub x: u8,
    pub y: u8,
    pub z: u8,
    pub id: u16,
    pub meta: u8,
}

/// S3E Teams action, decoded from the packet's mode byte. Display name,
/// friendly-fire, visibility and color are skipped — only the prefix/suffix
/// (which carry the visible sidebar text on most servers) and membership
/// matter to the client HUD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamAction {
    Create {
        prefix: String,
        suffix: String,
        players: Vec<String>,
    },
    Remove,
    Update {
        prefix: String,
        suffix: String,
    },
    AddPlayers {
        players: Vec<String>,
    },
    RemovePlayers {
        players: Vec<String>,
    },
}

/// One attribute in an S20 EntityProperties packet.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityProperty {
    pub key: String,
    pub base: f64,
    pub modifiers: Vec<AttributeModifier>,
}

/// A vanilla attribute modifier: 16-byte UUID, amount, and operation
/// (0 = add, 1 = add base-relative, 2 = multiply).
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeModifier {
    pub uuid: [u8; 16],
    pub amount: f64,
    pub operation: u8,
}

/// S45 Title action discriminant. The vanilla enum is sent as a VarInt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleAction {
    Title {
        json: String,
    },
    Subtitle {
        json: String,
    },
    Times {
        fade_in: i32,
        stay: i32,
        fade_out: i32,
    },
    Clear,
    Reset,
}

/// One signed property of a player's GameProfile (S38 add action). The
/// `textures` property's base64 `value` carries the skin/cape URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerProperty {
    pub name: String,
    pub value: String,
    pub signature: Option<String>,
}

/// S38 PlayerListItem per-entry action. The packet's single action int applies
/// to every entry; each entry already encodes which action it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerListItemAction {
    Add {
        name: String,
        properties: Vec<PlayerProperty>,
        gamemode: i32,
        ping: i32,
        /// Chat-JSON display name, or `None` to fall back to the plain name.
        display_name: Option<String>,
    },
    UpdateGameMode {
        gamemode: i32,
    },
    UpdateLatency {
        ping: i32,
    },
    UpdateDisplayName {
        display_name: Option<String>,
    },
    Remove,
}

/// One S38 PlayerListItem entry: the player UUID plus its action payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerListItemEntry {
    pub uuid: [u8; 16],
    pub action: PlayerListItemAction,
}

/// One decoded entry of a 1.8 EntityMetadata (S1C) array. `index` is the
/// data-watcher slot; the meaningful indices are documented per entity type.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataEntry {
    pub index: u8,
    pub value: MetadataValue,
}

/// A 1.8 metadata value, tagged by the type bits of the entry header.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    Byte(i8),
    Short(i16),
    Int(i32),
    Float(f32),
    Str(String),
    Slot(Option<SlotItem>),
    IntVec(i32, i32, i32),
    FloatVec(f32, f32, f32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientboundPlayPacket {
    KeepAlive {
        id: i32,
    },
    /// S03 TimeUpdate — the world age and time of day (both in ticks). A
    /// negative `time_of_day` signals a fixed time (gamerule doDaylightCycle
    /// off); its magnitude is the actual time.
    TimeUpdate {
        world_age: i64,
        time_of_day: i64,
    },
    /// S02 ChatMessage — a JSON chat component plus its display position
    /// (0 = chat, 1 = system message, 2 = action bar above the hotbar).
    ChatMessage {
        json: String,
        position: i8,
    },
    /// S39 PlayerAbilities — the server-driven capability set (vanilla
    /// `PlayerCapabilities`): flags byte (1 invulnerable, 2 flying,
    /// 4 allow-flying, 8 creative) plus fly/walk speeds. Applied
    /// unconditionally by the vanilla client.
    PlayerAbilities {
        invulnerable: bool,
        flying: bool,
        allow_flying: bool,
        creative: bool,
        fly_speed: f32,
        walk_speed: f32,
    },
    /// S20 EntityProperties — attribute sync. The movement-speed attribute
    /// (with potion/sprint modifiers) drives the player's ground speed.
    EntityProperties {
        entity_id: i32,
        properties: Vec<EntityProperty>,
    },
    /// S3B ScoreboardObjective — create (0), remove (1) or retitle (2) an
    /// objective. `display_name` is empty for removals; the render-type
    /// string ("integer"/"hearts") is skipped.
    ScoreboardObjective {
        name: String,
        mode: u8,
        display_name: String,
    },
    /// S3C UpdateScore — set (`value` = Some) or remove (`value` = None) a
    /// score entry. On removal the objective name may be empty, meaning
    /// "remove the entry from every objective".
    UpdateScore {
        name: String,
        objective: String,
        value: Option<i32>,
    },
    /// S3D DisplayScoreboard — bind an objective to a display slot
    /// (0 = player list, 1 = sidebar, 2 = below name); empty name clears it.
    DisplayScoreboard {
        position: i8,
        objective: String,
    },
    /// S3E Teams — team lifecycle and membership. Sidebar lines on most
    /// servers are fake "players" whose visible text lives in the team
    /// prefix/suffix.
    Teams {
        name: String,
        action: TeamAction,
    },
    /// S45 Title — center-screen title/subtitle text plus fade timings.
    Title {
        action: TitleAction,
    },
    JoinGame {
        entity_id: i32,
        game_mode: u8,
        dimension: i8,
        difficulty: u8,
        max_players: u8,
        level_type: String,
        reduced_debug_info: bool,
    },
    PlayerPositionLook {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        flags: i8,
    },
    UpdateHealth {
        health: f32,
        food: i32,
        food_saturation: f32,
    },
    /// C1F SetExperience — the XP bar fill (0..1) and current level.
    SetExperience {
        bar: f32,
        level: i32,
    },
    Respawn {
        dimension: i32,
        difficulty: u8,
        game_mode: u8,
        level_type: String,
    },
    ChunkData {
        x: i32,
        z: i32,
        ground_up: bool,
        primary_bit_mask: u16,
        data: Vec<u8>,
    },
    MultiBlockChange {
        chunk_x: i32,
        chunk_z: i32,
        changes: Vec<BlockChangeRecord>,
    },
    BlockChange {
        x: i32,
        y: i32,
        z: i32,
        id: u16,
        meta: u8,
    },
    ChunkBulk {
        sky_light_sent: bool,
        chunks: Vec<BulkChunkData>,
    },
    /// S2A SpawnParticle: a client-side particle effect at a world position with
    /// a spread box, a per-particle speed and a count. `args` carries the extra
    /// VarInts some particle types need (e.g. block/item id for *_CRACK / DUST).
    SpawnParticle {
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
    },
    Disconnect {
        reason_json: String,
    },
    SpawnPlayer {
        entity_id: i32,
        /// The player's account UUID, linking this entity to its roster entry
        /// (name, skin texture property). Vanilla used to discard it.
        uuid: [u8; 16],
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    },
    SpawnMob {
        entity_id: i32,
        kind: u8,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        head_pitch: f32,
        /// Trailing data-watcher metadata (sneak/name/age flags etc.).
        metadata: Vec<MetadataEntry>,
    },
    /// S11 SpawnExperienceOrb — a floating XP orb. `count` is the experience
    /// value the orb carries (drives the sprite size in the renderer).
    SpawnExperienceOrb {
        entity_id: i32,
        x: f64,
        y: f64,
        z: f64,
        count: i16,
    },
    SpawnObject {
        entity_id: i32,
        kind: i8,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        /// Object-type-specific data int (e.g. thrower id, block state).
        data: i32,
        /// Initial velocity in blocks/tick, present only when `data != 0`.
        velocity: Option<(f64, f64, f64)>,
    },
    /// Relative move deltas, already converted from fixed-point to blocks.
    EntityRelativeMove {
        entity_id: i32,
        dx: f64,
        dy: f64,
        dz: f64,
    },
    EntityLookMove {
        entity_id: i32,
        dx: f64,
        dy: f64,
        dz: f64,
        yaw: f32,
        pitch: f32,
    },
    EntityLook {
        entity_id: i32,
        yaw: f32,
        pitch: f32,
    },
    EntityTeleport {
        entity_id: i32,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    },
    DestroyEntities {
        entity_ids: Vec<i32>,
    },
    /// S12 EntityVelocity — velocity already converted from 1/8000 block per
    /// tick shorts to blocks per tick.
    EntityVelocity {
        entity_id: i32,
        vx: f64,
        vy: f64,
        vz: f64,
    },
    /// S2F SetSlot — the server updates a single window slot.
    SetSlot {
        window_id: i8,
        slot: i16,
        item: Option<SlotItem>,
    },
    /// S2D OpenWindow — the server asks the client to open a container window.
    /// `slots` excludes the player inventory; `entity_id` is only present for the
    /// horse window (inventory_type == "EntityHorse").
    OpenWindow {
        window_id: u8,
        inventory_type: String,
        title: String,
        slots: u8,
        entity_id: Option<i32>,
    },
    /// S2E CloseWindow — the server force-closes the given window.
    CloseWindowS {
        window_id: u8,
    },
    /// S30 WindowItems — the server replaces the whole window contents.
    WindowItems {
        window_id: u8,
        items: Vec<Option<SlotItem>>,
    },
    /// S31 WindowProperty — a window field (furnace progress, brewing time,
    /// enchant levels, beacon power…).
    WindowProperty {
        window_id: u8,
        property: i16,
        value: i16,
    },
    /// S09 HeldItemChange — the server changes our selected hotbar slot.
    HeldItemChange {
        slot: i8,
    },
    /// S32 ConfirmTransaction — a server transaction (window-confirm) the client
    /// must echo back. `accepted` is false for the server-initiated transactions
    /// that anti-cheats use as a timing ping.
    ConfirmTransaction {
        window_id: i8,
        action_number: i16,
        accepted: bool,
    },
    /// S19 EntityHeadLook — the head yaw of an entity (separate from the body
    /// yaw carried by movement packets), drives head-turn rendering.
    EntityHeadLook {
        entity_id: i32,
        head_yaw: f32,
    },
    /// S1C EntityMetadata — a sparse data-watcher update (sneak/sprint flags,
    /// custom nametag, item-entity contents, …).
    EntityMetadata {
        entity_id: i32,
        metadata: Vec<MetadataEntry>,
    },
    /// S1D EntityEffect — a potion effect applied to an entity. `amplifier` is
    /// the level minus one (Speed I = 0); `duration` is in ticks.
    EntityEffect {
        entity_id: i32,
        effect_id: i8,
        amplifier: i8,
        duration: i32,
        hide_particles: i8,
    },
    /// S1E RemoveEntityEffect — a potion effect cleared from an entity.
    RemoveEntityEffect {
        entity_id: i32,
        effect_id: i8,
    },
    /// S04 EntityEquipment — armor/held item on another entity. `slot` is
    /// 0 = held, 1-4 = boots/leggings/chest/helmet.
    EntityEquipment {
        entity_id: i32,
        slot: i16,
        item: Option<SlotItem>,
    },
    /// S0D CollectItem — `collected_id` was picked up by `collector_id`
    /// (drives the shrink-toward-collector pickup animation).
    CollectItem {
        collected_id: i32,
        collector_id: i32,
    },
    /// S0B Animation — `animation` 0 = swing arm, 1 = take damage, 2 = leave
    /// bed, 4 = critical effect, 5 = magic-critical effect.
    EntityAnimation {
        entity_id: i32,
        animation: u8,
    },
    /// S1A EntityStatus — entity event byte: 2 = hurt, 3 = dead, etc.
    EntityStatus {
        entity_id: i32,
        status: i8,
    },
    /// S1B AttachEntity — `entity_id` mounts (or leashes) onto `vehicle_id`;
    /// `vehicle_id == -1` detaches, and `leash` distinguishes a leash from a
    /// ride. Both ids are full ints in 1.8, not VarInts.
    AttachEntity {
        entity_id: i32,
        vehicle_id: i32,
        leash: bool,
    },
    /// S38 PlayerListItem — add/update/remove tab-list player profiles.
    PlayerListItem {
        entries: Vec<PlayerListItemEntry>,
    },
    /// S47 PlayerListHeaderFooter — the chat-JSON header and footer drawn above
    /// and below the tab list.
    PlayerListHeaderFooter {
        header: String,
        footer: String,
    },
    /// S29 SoundEffect — a named sound event at a world position. Coordinates
    /// arrive as ÷8 fixed-point and are converted to blocks here. `pitch` is
    /// the raw wire byte (vanilla stores `playback_rate * 63`).
    SoundEffect {
        name: String,
        x: f64,
        y: f64,
        z: f64,
        volume: f32,
        pitch: u8,
    },
    /// S24 BlockAction — a transient block animation/sound (note-block pluck,
    /// piston extend/retract, chest lid). `action_id` and `action_param` are
    /// block-type-specific (for a note block: instrument and note); `block_type`
    /// is the block id the action applies to.
    BlockAction {
        x: i32,
        y: i32,
        z: i32,
        action_id: u8,
        action_param: u8,
        block_type: i32,
    },
    /// S28 Effect — a hardcoded effect played by integer id (1000 click,
    /// 1003 door toggle, 2001 block break with blockstate `data`, …). When
    /// `disable_relative_volume` is set the sound plays at full volume.
    Effect {
        effect_id: i32,
        x: i32,
        y: i32,
        z: i32,
        data: i32,
        disable_relative_volume: bool,
    },
    /// S3A TabComplete — the completion candidates for the last C14 request.
    TabComplete {
        matches: Vec<String>,
    },
    /// S33 UpdateSign — the four lines of text on a sign block-entity, each a
    /// chat-JSON string, at the sign's block position.
    UpdateSign {
        x: i32,
        y: i32,
        z: i32,
        lines: [String; 4],
    },
    /// S3F PluginMessage — a custom-payload channel and its raw bytes (e.g.
    /// `MC|TrList` for villager trade offers, `MC|Brand` for the server brand).
    PluginMessage {
        channel: String,
        data: Vec<u8>,
    },
    Unknown {
        id: i32,
        body: Vec<u8>,
    },
}

/// Decoded Slot data: id, count, damage, and the optional NBT compound.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotItem {
    pub id: i16,
    pub count: u8,
    pub damage: i16,
    pub nbt: Option<crate::nbt::NbtCompound>,
}

impl SlotItem {
    pub fn new(id: i16, count: u8, damage: i16) -> Self {
        Self { id, count, damage, nbt: None }
    }
}

impl ServerboundPacket {
    pub fn into_frame(self) -> PacketFrame {
        match self {
            Self::Handshake {
                protocol_version,
                host,
                port,
                next_state,
            } => {
                let mut body = PacketWriter::new();
                body.write_var_i32(protocol_version);
                body.write_string(&host);
                body.write_u16(port);
                body.write_var_i32(next_state as i32);
                PacketFrame::new(0x00, body.into_inner())
            }
            Self::LoginStart { username } => {
                let mut body = PacketWriter::new();
                body.write_string(&username);
                PacketFrame::new(0x00, body.into_inner())
            }
            Self::KeepAlive { id } => {
                let mut body = PacketWriter::new();
                body.write_var_i32(id);
                PacketFrame::new(0x00, body.into_inner())
            }
            Self::ClientSettings {
                locale,
                view_distance,
                chat_mode,
                chat_colors,
                skin_parts,
            } => {
                let mut body = PacketWriter::new();
                body.write_string(&locale);
                body.write_i8(view_distance);
                body.write_i8(chat_mode);
                body.write_bool(chat_colors);
                body.write_u8(skin_parts);
                PacketFrame::new(0x15, body.into_inner())
            }
            Self::PluginMessage { channel, data } => {
                let mut body = PacketWriter::new();
                body.write_string(&channel);
                body.write_bytes(&data);
                PacketFrame::new(0x17, body.into_inner())
            }
            Self::Player { on_ground } => {
                let mut body = PacketWriter::new();
                body.write_bool(on_ground);
                PacketFrame::new(0x03, body.into_inner())
            }
            Self::PlayerPosition { x, y, z, on_ground } => {
                let mut body = PacketWriter::new();
                body.write_f64(x);
                body.write_f64(y);
                body.write_f64(z);
                body.write_bool(on_ground);
                PacketFrame::new(0x04, body.into_inner())
            }
            Self::PlayerLook {
                yaw,
                pitch,
                on_ground,
            } => {
                let mut body = PacketWriter::new();
                body.write_f32(yaw);
                body.write_f32(pitch);
                body.write_bool(on_ground);
                PacketFrame::new(0x05, body.into_inner())
            }
            Self::PlayerPositionLook {
                x,
                y,
                z,
                yaw,
                pitch,
                on_ground,
            } => {
                let mut body = PacketWriter::new();
                body.write_f64(x);
                body.write_f64(y);
                body.write_f64(z);
                body.write_f32(yaw);
                body.write_f32(pitch);
                body.write_bool(on_ground);
                PacketFrame::new(0x06, body.into_inner())
            }
            Self::EntityAction {
                entity_id,
                action,
                aux_data,
            } => {
                let mut body = PacketWriter::new();
                body.write_var_i32(entity_id);
                body.write_var_i32(action as i32);
                body.write_var_i32(aux_data);
                PacketFrame::new(0x0b, body.into_inner())
            }
            Self::UseEntity { target, kind } => {
                let mut body = PacketWriter::new();
                body.write_var_i32(target);
                match kind {
                    UseEntityKind::Interact => body.write_var_i32(0),
                    UseEntityKind::Attack => body.write_var_i32(1),
                    UseEntityKind::InteractAt { x, y, z } => {
                        body.write_var_i32(2);
                        body.write_f32(x);
                        body.write_f32(y);
                        body.write_f32(z);
                    }
                }
                PacketFrame::new(0x02, body.into_inner())
            }
            Self::PlayerDigging {
                status,
                x,
                y,
                z,
                face,
            } => {
                let mut body = PacketWriter::new();
                body.write_u8(status as u8);
                body.write_i64(encode_block_pos(x, y, z));
                body.write_u8(face);
                PacketFrame::new(0x07, body.into_inner())
            }
            Self::PlayerBlockPlacement {
                x,
                y,
                z,
                face,
                held_item,
                cursor_x,
                cursor_y,
                cursor_z,
            } => {
                let mut body = PacketWriter::new();
                body.write_i64(encode_block_pos(x, y, z));
                body.write_u8(face);
                match held_item {
                    None => body.write_i16(-1),
                    Some(item) => {
                        body.write_i16(item.id);
                        body.write_u8(item.count);
                        body.write_i16(item.damage);
                        body.write_u8(0); // empty NBT
                    }
                }
                body.write_u8(cursor_x);
                body.write_u8(cursor_y);
                body.write_u8(cursor_z);
                PacketFrame::new(0x08, body.into_inner())
            }
            Self::HeldItemChange { slot } => {
                let mut body = PacketWriter::new();
                body.write_i16(slot);
                PacketFrame::new(0x09, body.into_inner())
            }
            Self::ClickWindow {
                window_id,
                slot,
                button,
                action_number,
                mode,
                clicked_item,
            } => {
                let mut body = PacketWriter::new();
                body.write_u8(window_id);
                body.write_i16(slot);
                body.write_i8(button);
                body.write_i16(action_number);
                body.write_i8(mode);
                write_slot(&mut body, clicked_item);
                PacketFrame::new(0x0e, body.into_inner())
            }
            Self::CloseWindow { window_id } => {
                let mut body = PacketWriter::new();
                body.write_u8(window_id);
                PacketFrame::new(0x0d, body.into_inner())
            }
            Self::CreativeInventoryAction { slot, clicked_item } => {
                let mut body = PacketWriter::new();
                body.write_i16(slot);
                write_slot(&mut body, clicked_item);
                PacketFrame::new(0x10, body.into_inner())
            }
            Self::SwingArm => PacketFrame::new(0x0a, Vec::new()),
            Self::ClientStatus { action } => {
                let mut body = PacketWriter::new();
                body.write_var_i32(action);
                PacketFrame::new(0x16, body.into_inner())
            }
            Self::ConfirmTransaction {
                window_id,
                action_number,
                accepted,
            } => {
                let mut body = PacketWriter::new();
                body.write_i8(window_id);
                body.write_i16(action_number);
                body.write_bool(accepted);
                PacketFrame::new(0x0f, body.into_inner())
            }
            Self::ChatMessage { message } => {
                let mut body = PacketWriter::new();
                body.write_string(&message);
                PacketFrame::new(0x01, body.into_inner())
            }
            Self::PlayerAbilities {
                invulnerable,
                flying,
                allow_flying,
                creative,
                fly_speed,
                walk_speed,
            } => {
                let mut body = PacketWriter::new();
                body.write_u8(abilities_flags(invulnerable, flying, allow_flying, creative));
                body.write_f32(fly_speed);
                body.write_f32(walk_speed);
                PacketFrame::new(0x13, body.into_inner())
            }
            Self::TabComplete { text, has_position } => {
                let mut body = PacketWriter::new();
                body.write_string(&text);
                body.write_bool(has_position);
                PacketFrame::new(0x14, body.into_inner())
            }
        }
    }
}

/// Pack the vanilla abilities flag byte (shared by S39 and C13).
fn abilities_flags(invulnerable: bool, flying: bool, allow_flying: bool, creative: bool) -> u8 {
    u8::from(invulnerable)
        | u8::from(flying) << 1
        | u8::from(allow_flying) << 2
        | u8::from(creative) << 3
}

/// Encode a block position into the 1.8 packed long (26/12/26 bits).
fn encode_block_pos(x: i32, y: i32, z: i32) -> i64 {
    (((x as i64) & 0x03ff_ffff) << 38) | (((y as i64) & 0x0fff) << 26) | ((z as i64) & 0x03ff_ffff)
}

impl ClientboundLoginPacket {
    pub fn from_frame(frame: PacketFrame) -> Result<Self> {
        let mut body = PacketReader::new(&frame.body);
        match frame.id {
            0x00 => Ok(Self::Disconnect {
                reason_json: body.read_string(32767)?,
            }),
            0x01 => {
                let server_id = body.read_string(20)?;
                let public_key_len = body.read_var_i32()? as usize;
                let public_key = body.read_bytes(public_key_len)?.to_vec();
                let verify_token_len = body.read_var_i32()? as usize;
                let verify_token = body.read_bytes(verify_token_len)?.to_vec();
                Ok(Self::EncryptionRequest {
                    server_id,
                    public_key,
                    verify_token,
                })
            }
            0x02 => Ok(Self::LoginSuccess {
                uuid: body.read_string(36)?,
                username: body.read_string(16)?,
            }),
            0x03 => Ok(Self::SetCompression {
                threshold: body.read_var_i32()?,
            }),
            id => Err(ProtocolError::InvalidPacketId(id, "login/clientbound")),
        }
    }
}

impl ClientboundPlayPacket {
    pub fn from_frame(frame: PacketFrame) -> Result<Self> {
        let mut body = PacketReader::new(&frame.body);
        match frame.id {
            0x00 => Ok(Self::KeepAlive {
                id: body.read_var_i32()?,
            }),
            0x03 => Ok(Self::TimeUpdate {
                world_age: body.read_i64()?,
                time_of_day: body.read_i64()?,
            }),
            0x01 => Ok(Self::JoinGame {
                entity_id: body.read_i32()?,
                game_mode: body.read_u8()?,
                dimension: body.read_i8()?,
                difficulty: body.read_u8()?,
                max_players: body.read_u8()?,
                level_type: body.read_string(16)?,
                reduced_debug_info: body.read_bool()?,
            }),
            0x06 => Ok(Self::UpdateHealth {
                health: body.read_f32()?,
                food: body.read_var_i32()?,
                food_saturation: body.read_f32()?,
            }),
            0x1f => Ok(Self::SetExperience {
                bar: body.read_f32()?,
                level: body.read_var_i32()?,
                // trailing total-experience varint is ignored
            }),
            0x07 => Ok(Self::Respawn {
                dimension: body.read_i32()?,
                difficulty: body.read_u8()?,
                game_mode: body.read_u8()?,
                level_type: body.read_string(16)?,
            }),
            0x08 => Ok(Self::PlayerPositionLook {
                x: body.read_f64()?,
                y: body.read_f64()?,
                z: body.read_f64()?,
                yaw: body.read_f32()?,
                pitch: body.read_f32()?,
                flags: body.read_i8()?,
            }),
            0x21 => Ok(Self::ChunkData {
                x: body.read_i32()?,
                z: body.read_i32()?,
                ground_up: body.read_bool()?,
                primary_bit_mask: body.read_u16()?,
                data: {
                    let len = body.read_var_i32()? as usize;
                    body.read_bytes(len)?.to_vec()
                },
            }),
            0x22 => {
                let chunk_x = body.read_i32()?;
                let chunk_z = body.read_i32()?;
                let count = body.read_var_i32()? as usize;
                let mut changes = Vec::with_capacity(count);
                for _ in 0..count {
                    let packed = body.read_u16()?;
                    let (id, meta) = decode_legacy_block_state_id(body.read_var_i32()?)?;
                    changes.push(BlockChangeRecord {
                        x: ((packed >> 12) & 15) as u8,
                        y: (packed & 255) as u8,
                        z: ((packed >> 8) & 15) as u8,
                        id,
                        meta,
                    });
                }
                Ok(Self::MultiBlockChange {
                    chunk_x,
                    chunk_z,
                    changes,
                })
            }
            0x23 => {
                let (x, y, z) = read_block_pos(&mut body)?;
                let (id, meta) = decode_legacy_block_state_id(body.read_var_i32()?)?;
                Ok(Self::BlockChange { x, y, z, id, meta })
            }
            0x2a => {
                let particle_id = body.read_i32()?;
                let _long_distance = body.read_bool()?;
                let x = body.read_f32()?;
                let y = body.read_f32()?;
                let z = body.read_f32()?;
                let offset_x = body.read_f32()?;
                let offset_y = body.read_f32()?;
                let offset_z = body.read_f32()?;
                let speed = body.read_f32()?;
                let count = body.read_i32()?;
                // Extra VarInts: 2 for ITEM_CRACK (36), 1 for BLOCK_CRACK (37) /
                // BLOCK_DUST (38), none otherwise (EnumParticleTypes.argumentCount).
                let arg_count = match particle_id {
                    36 => 2,
                    37 | 38 => 1,
                    _ => 0,
                };
                let mut args = Vec::with_capacity(arg_count);
                for _ in 0..arg_count {
                    args.push(body.read_var_i32()?);
                }
                Ok(Self::SpawnParticle {
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
                })
            }
            0x26 => {
                let sky_light_sent = body.read_bool()?;
                let count = body.read_var_i32()? as usize;
                let mut meta = Vec::with_capacity(count);
                for _ in 0..count {
                    meta.push((body.read_i32()?, body.read_i32()?, body.read_u16()?));
                }
                let mut chunks = Vec::with_capacity(count);
                for (x, z, primary_bit_mask) in meta {
                    let len = bulk_chunk_data_len(primary_bit_mask, sky_light_sent);
                    chunks.push(BulkChunkData {
                        x,
                        z,
                        primary_bit_mask,
                        data: body.read_bytes(len)?.to_vec(),
                    });
                }
                Ok(Self::ChunkBulk {
                    sky_light_sent,
                    chunks,
                })
            }
            0x0c => {
                let entity_id = body.read_var_i32()?;
                let mut uuid = [0u8; 16];
                uuid.copy_from_slice(body.read_bytes(16)?);
                let x = fixed_point(body.read_i32()?);
                let y = fixed_point(body.read_i32()?);
                let z = fixed_point(body.read_i32()?);
                let yaw = angle(body.read_i8()?);
                let pitch = angle(body.read_i8()?);
                // Trailing current-item Short and metadata array are ignored.
                Ok(Self::SpawnPlayer {
                    entity_id,
                    uuid,
                    x,
                    y,
                    z,
                    yaw,
                    pitch,
                })
            }
            0x0e => {
                let entity_id = body.read_var_i32()?;
                let kind = body.read_i8()?;
                let x = fixed_point(body.read_i32()?);
                let y = fixed_point(body.read_i32()?);
                let z = fixed_point(body.read_i32()?);
                let pitch = angle(body.read_i8()?);
                let yaw = angle(body.read_i8()?);
                let data = body.read_i32()?;
                let velocity = if data != 0 {
                    Some((
                        body.read_i16()? as f64 / 8000.0,
                        body.read_i16()? as f64 / 8000.0,
                        body.read_i16()? as f64 / 8000.0,
                    ))
                } else {
                    None
                };
                Ok(Self::SpawnObject {
                    entity_id,
                    kind,
                    x,
                    y,
                    z,
                    yaw,
                    pitch,
                    data,
                    velocity,
                })
            }
            0x0f => {
                let entity_id = body.read_var_i32()?;
                let kind = body.read_u8()?;
                let x = fixed_point(body.read_i32()?);
                let y = fixed_point(body.read_i32()?);
                let z = fixed_point(body.read_i32()?);
                let yaw = angle(body.read_i8()?);
                let pitch = angle(body.read_i8()?);
                let head_pitch = angle(body.read_i8()?);
                // Velocity shorts precede the metadata array.
                body.read_i16()?;
                body.read_i16()?;
                body.read_i16()?;
                let metadata = read_metadata(&mut body)?;
                Ok(Self::SpawnMob {
                    entity_id,
                    kind,
                    x,
                    y,
                    z,
                    yaw,
                    pitch,
                    head_pitch,
                    metadata,
                })
            }
            0x11 => Ok(Self::SpawnExperienceOrb {
                entity_id: body.read_var_i32()?,
                x: fixed_point(body.read_i32()?),
                y: fixed_point(body.read_i32()?),
                z: fixed_point(body.read_i32()?),
                count: body.read_i16()?,
            }),
            0x15 => Ok(Self::EntityRelativeMove {
                entity_id: body.read_var_i32()?,
                dx: fixed_point_delta(body.read_i8()?),
                dy: fixed_point_delta(body.read_i8()?),
                dz: fixed_point_delta(body.read_i8()?),
            }),
            0x16 => {
                let entity_id = body.read_var_i32()?;
                Ok(Self::EntityLook {
                    entity_id,
                    yaw: angle(body.read_i8()?),
                    pitch: angle(body.read_i8()?),
                })
            }
            0x17 => Ok(Self::EntityLookMove {
                entity_id: body.read_var_i32()?,
                dx: fixed_point_delta(body.read_i8()?),
                dy: fixed_point_delta(body.read_i8()?),
                dz: fixed_point_delta(body.read_i8()?),
                yaw: angle(body.read_i8()?),
                pitch: angle(body.read_i8()?),
            }),
            0x18 => Ok(Self::EntityTeleport {
                entity_id: body.read_var_i32()?,
                x: fixed_point(body.read_i32()?),
                y: fixed_point(body.read_i32()?),
                z: fixed_point(body.read_i32()?),
                yaw: angle(body.read_i8()?),
                pitch: angle(body.read_i8()?),
            }),
            0x13 => {
                let count = body.read_var_i32()? as usize;
                let mut entity_ids = Vec::with_capacity(count);
                for _ in 0..count {
                    entity_ids.push(body.read_var_i32()?);
                }
                Ok(Self::DestroyEntities { entity_ids })
            }
            0x12 => Ok(Self::EntityVelocity {
                entity_id: body.read_var_i32()?,
                vx: body.read_i16()? as f64 / 8000.0,
                vy: body.read_i16()? as f64 / 8000.0,
                vz: body.read_i16()? as f64 / 8000.0,
            }),
            0x2d => {
                let window_id = body.read_u8()?;
                let inventory_type = body.read_string(32)?;
                let title = body.read_string(256)?;
                let slots = body.read_u8()?;
                let entity_id = if inventory_type == "EntityHorse" {
                    Some(body.read_i32()?)
                } else {
                    None
                };
                Ok(Self::OpenWindow {
                    window_id,
                    inventory_type,
                    title,
                    slots,
                    entity_id,
                })
            }
            0x2e => Ok(Self::CloseWindowS {
                window_id: body.read_u8()?,
            }),
            0x2f => Ok(Self::SetSlot {
                window_id: body.read_i8()?,
                slot: body.read_i16()?,
                item: read_slot(&mut body)?,
            }),
            0x31 => Ok(Self::WindowProperty {
                window_id: body.read_u8()?,
                property: body.read_i16()?,
                value: body.read_i16()?,
            }),
            0x30 => {
                let window_id = body.read_u8()?;
                let count = body.read_i16()?;
                if count < 0 {
                    return Err(ProtocolError::InvalidData(
                        "negative WindowItems slot count",
                    ));
                }
                let mut items = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    items.push(read_slot(&mut body)?);
                }
                Ok(Self::WindowItems { window_id, items })
            }
            0x09 => Ok(Self::HeldItemChange {
                slot: body.read_i8()?,
            }),
            0x32 => Ok(Self::ConfirmTransaction {
                window_id: body.read_i8()?,
                action_number: body.read_i16()?,
                accepted: body.read_bool()?,
            }),
            0x02 => Ok(Self::ChatMessage {
                json: body.read_string(32767)?,
                position: body.read_i8()?,
            }),
            0x39 => {
                let flags = body.read_u8()?;
                Ok(Self::PlayerAbilities {
                    invulnerable: flags & 1 != 0,
                    flying: flags & 2 != 0,
                    allow_flying: flags & 4 != 0,
                    creative: flags & 8 != 0,
                    fly_speed: body.read_f32()?,
                    walk_speed: body.read_f32()?,
                })
            }
            0x3b => {
                let name = body.read_string(64)?;
                let mode = body.read_u8()?;
                let display_name = if mode == 0 || mode == 2 {
                    let display = body.read_string(128)?;
                    body.read_string(64)?; // render type ("integer"/"hearts")
                    display
                } else {
                    String::new()
                };
                Ok(Self::ScoreboardObjective {
                    name,
                    mode,
                    display_name,
                })
            }
            0x3c => {
                let name = body.read_string(64)?;
                let action = body.read_u8()?;
                let objective = body.read_string(64)?;
                let value = if action != 1 {
                    Some(body.read_var_i32()?)
                } else {
                    None
                };
                Ok(Self::UpdateScore {
                    name,
                    objective,
                    value,
                })
            }
            0x3d => Ok(Self::DisplayScoreboard {
                position: body.read_i8()?,
                objective: body.read_string(64)?,
            }),
            0x3e => {
                let name = body.read_string(64)?;
                let mode = body.read_u8()?;
                let read_players = |body: &mut PacketReader<'_>| -> Result<Vec<String>> {
                    let count = body.read_var_i32()?;
                    if count < 0 {
                        return Err(ProtocolError::InvalidData("negative team player count"));
                    }
                    let mut players = Vec::with_capacity(count as usize);
                    for _ in 0..count {
                        players.push(body.read_string(64)?);
                    }
                    Ok(players)
                };
                let action = match mode {
                    0 => {
                        body.read_string(128)?; // display name
                        let prefix = body.read_string(64)?;
                        let suffix = body.read_string(64)?;
                        body.read_u8()?; // friendly fire
                        body.read_string(64)?; // name tag visibility
                        body.read_i8()?; // color
                        TeamAction::Create {
                            prefix,
                            suffix,
                            players: read_players(&mut body)?,
                        }
                    }
                    1 => TeamAction::Remove,
                    2 => {
                        body.read_string(128)?; // display name
                        let prefix = body.read_string(64)?;
                        let suffix = body.read_string(64)?;
                        body.read_u8()?;
                        body.read_string(64)?;
                        body.read_i8()?;
                        TeamAction::Update { prefix, suffix }
                    }
                    3 => TeamAction::AddPlayers {
                        players: read_players(&mut body)?,
                    },
                    4 => TeamAction::RemovePlayers {
                        players: read_players(&mut body)?,
                    },
                    _ => return Err(ProtocolError::InvalidData("unknown team mode")),
                };
                Ok(Self::Teams { name, action })
            }
            0x20 => {
                let entity_id = body.read_var_i32()?;
                let count = body.read_i32()?;
                if count < 0 {
                    return Err(ProtocolError::InvalidData("negative property count"));
                }
                let mut properties = Vec::with_capacity(count.min(64) as usize);
                for _ in 0..count {
                    let key = body.read_string(64)?;
                    let base = body.read_f64()?;
                    let modifier_count = body.read_var_i32()?;
                    if modifier_count < 0 {
                        return Err(ProtocolError::InvalidData("negative modifier count"));
                    }
                    let mut modifiers = Vec::with_capacity(modifier_count.min(16) as usize);
                    for _ in 0..modifier_count {
                        let mut uuid = [0u8; 16];
                        uuid.copy_from_slice(body.read_bytes(16)?);
                        modifiers.push(AttributeModifier {
                            uuid,
                            amount: body.read_f64()?,
                            operation: body.read_u8()?,
                        });
                    }
                    properties.push(EntityProperty {
                        key,
                        base,
                        modifiers,
                    });
                }
                Ok(Self::EntityProperties {
                    entity_id,
                    properties,
                })
            }
            0x40 => Ok(Self::Disconnect {
                reason_json: body.read_string(32767)?,
            }),
            0x45 => Ok(Self::Title {
                action: read_title_action(&mut body)?,
            }),
            0x04 => Ok(Self::EntityEquipment {
                entity_id: body.read_var_i32()?,
                slot: body.read_i16()?,
                item: read_slot(&mut body)?,
            }),
            0x0b => Ok(Self::EntityAnimation {
                entity_id: body.read_var_i32()?,
                animation: body.read_u8()?,
            }),
            0x0d => Ok(Self::CollectItem {
                collected_id: body.read_var_i32()?,
                collector_id: body.read_var_i32()?,
            }),
            0x19 => Ok(Self::EntityHeadLook {
                entity_id: body.read_var_i32()?,
                head_yaw: angle(body.read_i8()?),
            }),
            0x1a => Ok(Self::EntityStatus {
                entity_id: body.read_i32()?,
                status: body.read_i8()?,
            }),
            0x1b => Ok(Self::AttachEntity {
                entity_id: body.read_i32()?,
                vehicle_id: body.read_i32()?,
                leash: body.read_bool()?,
            }),
            0x1c => Ok(Self::EntityMetadata {
                entity_id: body.read_var_i32()?,
                metadata: read_metadata(&mut body)?,
            }),
            0x1d => Ok(Self::EntityEffect {
                entity_id: body.read_var_i32()?,
                effect_id: body.read_i8()?,
                amplifier: body.read_i8()?,
                duration: body.read_var_i32()?,
                hide_particles: body.read_i8()?,
            }),
            0x1e => Ok(Self::RemoveEntityEffect {
                entity_id: body.read_var_i32()?,
                effect_id: body.read_i8()?,
            }),
            0x38 => {
                let action = body.read_var_i32()?;
                let count = body.read_var_i32()?;
                if count < 0 {
                    return Err(ProtocolError::InvalidData("negative player-list count"));
                }
                let mut entries = Vec::with_capacity(count.min(1024) as usize);
                for _ in 0..count {
                    let mut uuid = [0u8; 16];
                    uuid.copy_from_slice(body.read_bytes(16)?);
                    let action = read_player_list_action(&mut body, action)?;
                    entries.push(PlayerListItemEntry { uuid, action });
                }
                Ok(Self::PlayerListItem { entries })
            }
            0x47 => Ok(Self::PlayerListHeaderFooter {
                header: body.read_string(32767)?,
                footer: body.read_string(32767)?,
            }),
            0x29 => Ok(Self::SoundEffect {
                name: body.read_string(256)?,
                x: body.read_i32()? as f64 / 8.0,
                y: body.read_i32()? as f64 / 8.0,
                z: body.read_i32()? as f64 / 8.0,
                volume: body.read_f32()?,
                pitch: body.read_u8()?,
            }),
            0x24 => {
                let (x, y, z) = read_block_pos(&mut body)?;
                let action_id = body.read_u8()?;
                let action_param = body.read_u8()?;
                let block_type = body.read_var_i32()?;
                Ok(Self::BlockAction {
                    x,
                    y,
                    z,
                    action_id,
                    action_param,
                    block_type,
                })
            }
            0x28 => {
                let effect_id = body.read_i32()?;
                let (x, y, z) = read_block_pos(&mut body)?;
                let data = body.read_i32()?;
                let disable_relative_volume = body.read_bool()?;
                Ok(Self::Effect {
                    effect_id,
                    x,
                    y,
                    z,
                    data,
                    disable_relative_volume,
                })
            }
            0x3a => {
                let count = body.read_var_i32()?;
                if count < 0 {
                    return Err(ProtocolError::InvalidData("negative tab-complete count"));
                }
                let mut matches = Vec::with_capacity(count.min(1024) as usize);
                for _ in 0..count {
                    matches.push(body.read_string(32767)?);
                }
                Ok(Self::TabComplete { matches })
            }
            0x33 => {
                let (x, y, z) = read_block_pos(&mut body)?;
                let lines = [
                    body.read_string(32767)?,
                    body.read_string(32767)?,
                    body.read_string(32767)?,
                    body.read_string(32767)?,
                ];
                Ok(Self::UpdateSign { x, y, z, lines })
            }
            0x3f => {
                let channel = body.read_string(32767)?;
                let data = body.read_remaining().to_vec();
                Ok(Self::PluginMessage { channel, data })
            }
            id => Ok(Self::Unknown {
                id,
                body: frame.body,
            }),
        }
    }
}

fn read_title_action(body: &mut PacketReader<'_>) -> Result<TitleAction> {
    Ok(match body.read_var_i32()? {
        0 => TitleAction::Title {
            json: body.read_string(32767)?,
        },
        1 => TitleAction::Subtitle {
            json: body.read_string(32767)?,
        },
        2 => TitleAction::Times {
            fade_in: body.read_i32()?,
            stay: body.read_i32()?,
            fade_out: body.read_i32()?,
        },
        3 => TitleAction::Clear,
        4 => TitleAction::Reset,
        _ => return Err(ProtocolError::InvalidData("unknown title action")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1_8_9::PROTOCOL_VERSION;

    #[test]
    fn handshake_packet_matches_protocol_47_layout() {
        let frame = ServerboundPacket::Handshake {
            protocol_version: PROTOCOL_VERSION,
            host: "localhost".to_owned(),
            port: 25565,
            next_state: NextState::Login,
        }
        .into_frame();

        assert_eq!(frame.id, 0x00);
        let mut reader = PacketReader::new(&frame.body);
        assert_eq!(reader.read_var_i32().unwrap(), 47);
        assert_eq!(reader.read_string(255).unwrap(), "localhost");
        assert_eq!(reader.read_u16().unwrap(), 25565);
        assert_eq!(reader.read_var_i32().unwrap(), 2);
        assert!(reader.is_empty());
    }

    #[test]
    fn clientbound_join_game_decodes() {
        let mut body = PacketWriter::new();
        body.write_i32(123);
        body.write_u8(0);
        body.write_i8(0);
        body.write_u8(2);
        body.write_u8(20);
        body.write_string("default");
        body.write_bool(false);

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x01, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::JoinGame {
                entity_id: 123,
                game_mode: 0,
                dimension: 0,
                difficulty: 2,
                max_players: 20,
                level_type: "default".to_owned(),
                reduced_debug_info: false,
            }
        );
    }

    #[test]
    fn clientbound_block_change_decodes_position_and_legacy_state() {
        let mut body = PacketWriter::new();
        body.write_bytes(&encoded_block_pos(-1, 64, 2));
        body.write_var_i32((1 << 4) | 0);

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x23, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::BlockChange {
                x: -1,
                y: 64,
                z: 2,
                id: 1,
                meta: 0,
            }
        );
    }

    #[test]
    fn clientbound_multi_block_change_decodes_crammed_positions() {
        let mut body = PacketWriter::new();
        body.write_i32(-2);
        body.write_i32(3);
        body.write_var_i32(2);
        body.write_u16((1 << 12) | (2 << 8) | 64);
        body.write_var_i32((2 << 4) | 0);
        body.write_u16((15 << 12) | 255);
        body.write_var_i32((5 << 4) | 3);

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x22, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::MultiBlockChange {
                chunk_x: -2,
                chunk_z: 3,
                changes: vec![
                    BlockChangeRecord {
                        x: 1,
                        y: 64,
                        z: 2,
                        id: 2,
                        meta: 0,
                    },
                    BlockChangeRecord {
                        x: 15,
                        y: 255,
                        z: 0,
                        id: 5,
                        meta: 3,
                    },
                ],
            }
        );
    }

    #[test]
    fn entity_action_packet_writes_vanilla_enum_ordinal() {
        let frame = ServerboundPacket::EntityAction {
            entity_id: 42,
            action: EntityAction::StartSprinting,
            aux_data: 0,
        }
        .into_frame();

        assert_eq!(frame.id, 0x0b);
        let mut reader = PacketReader::new(&frame.body);
        assert_eq!(reader.read_var_i32().unwrap(), 42);
        assert_eq!(reader.read_var_i32().unwrap(), 3);
        assert_eq!(reader.read_var_i32().unwrap(), 0);
        assert!(reader.is_empty());
    }

    #[test]
    fn player_packet_writes_on_ground_only() {
        let frame = ServerboundPacket::Player { on_ground: true }.into_frame();

        assert_eq!(frame.id, 0x03);
        assert_eq!(frame.body, vec![1]);
    }

    #[test]
    fn use_entity_attack_writes_target_and_action() {
        let frame = ServerboundPacket::UseEntity {
            target: 99,
            kind: UseEntityKind::Attack,
        }
        .into_frame();
        assert_eq!(frame.id, 0x02);
        let mut reader = PacketReader::new(&frame.body);
        assert_eq!(reader.read_var_i32().unwrap(), 99);
        assert_eq!(reader.read_var_i32().unwrap(), 1);
        assert!(reader.is_empty());
    }

    #[test]
    fn player_digging_writes_status_position_face() {
        let frame = ServerboundPacket::PlayerDigging {
            status: DiggingStatus::StartDestroy,
            x: -1,
            y: 64,
            z: 2,
            face: 1,
        }
        .into_frame();
        assert_eq!(frame.id, 0x07);
        let mut reader = PacketReader::new(&frame.body);
        assert_eq!(reader.read_u8().unwrap(), 0);
        let (x, y, z) = read_block_pos(&mut reader).unwrap();
        assert_eq!((x, y, z), (-1, 64, 2));
        assert_eq!(reader.read_u8().unwrap(), 1);
    }

    #[test]
    fn block_placement_empty_hand_writes_negative_slot() {
        let frame = ServerboundPacket::PlayerBlockPlacement {
            x: 0,
            y: 0,
            z: 0,
            face: 255,
            held_item: None,
            cursor_x: 8,
            cursor_y: 8,
            cursor_z: 8,
        }
        .into_frame();
        assert_eq!(frame.id, 0x08);
        let mut reader = PacketReader::new(&frame.body);
        read_block_pos(&mut reader).unwrap();
        assert_eq!(reader.read_u8().unwrap(), 255);
        assert_eq!(reader.read_i16().unwrap(), -1);
    }

    #[test]
    fn swing_arm_has_no_payload() {
        let frame = ServerboundPacket::SwingArm.into_frame();
        assert_eq!(frame.id, 0x0a);
        assert!(frame.body.is_empty());
    }

    #[test]
    fn spawn_mob_decodes_fixed_point_and_angles() {
        let mut body = PacketWriter::new();
        body.write_var_i32(42); // entity id
        body.write_u8(54); // zombie
        body.write_i32(32 * 10); // x = 10.0
        body.write_i32(32 * 64); // y = 64.0
        body.write_i32(32 * -5); // z = -5.0
        body.write_i8(64); // yaw = 90 deg
        body.write_i8(0); // pitch
        body.write_i8(0); // head pitch
        body.write_i16(0); // velocity x
        body.write_i16(0); // velocity y
        body.write_i16(0); // velocity z
        body.write_u8(0x7f); // empty metadata terminator

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x0f, body.into_inner())).unwrap();
        match packet {
            ClientboundPlayPacket::SpawnMob {
                entity_id,
                kind,
                x,
                y,
                z,
                yaw,
                ..
            } => {
                assert_eq!(entity_id, 42);
                assert_eq!(kind, 54);
                assert!((x - 10.0).abs() < 1e-9);
                assert!((y - 64.0).abs() < 1e-9);
                assert!((z + 5.0).abs() < 1e-9);
                assert!((yaw - 90.0).abs() < 0.01);
            }
            other => panic!("expected SpawnMob, got {other:?}"),
        }
    }

    #[test]
    fn spawn_experience_orb_decodes_fixed_point_and_count() {
        let mut body = PacketWriter::new();
        body.write_var_i32(7);
        body.write_i32(32 * 10); // x = 10.0
        body.write_i32(32 * 64); // y = 64.0
        body.write_i32(32 * -3); // z = -3.0
        body.write_i16(150); // xp value
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x11, body.into_inner())).unwrap();
        match packet {
            ClientboundPlayPacket::SpawnExperienceOrb {
                entity_id,
                x,
                y,
                z,
                count,
            } => {
                assert_eq!(entity_id, 7);
                assert!((x - 10.0).abs() < 1e-9);
                assert!((y - 64.0).abs() < 1e-9);
                assert!((z + 3.0).abs() < 1e-9);
                assert_eq!(count, 150);
            }
            other => panic!("expected SpawnExperienceOrb, got {other:?}"),
        }
    }

    #[test]
    fn block_action_decodes_position_and_note() {
        let mut body = PacketWriter::new();
        body.write_bytes(&encoded_block_pos(1, 65, -2));
        body.write_u8(2); // instrument (snare)
        body.write_u8(12); // note
        body.write_var_i32(25); // note block
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x24, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::BlockAction {
                x: 1,
                y: 65,
                z: -2,
                action_id: 2,
                action_param: 12,
                block_type: 25,
            }
        );
    }

    #[test]
    fn update_sign_decodes_position_and_four_lines() {
        let mut body = PacketWriter::new();
        body.write_bytes(&encoded_block_pos(7, 64, -3));
        body.write_string("{\"text\":\"line0\"}");
        body.write_string("{\"text\":\"line1\"}");
        body.write_string("");
        body.write_string("{\"text\":\"line3\"}");
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x33, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::UpdateSign {
                x: 7,
                y: 64,
                z: -3,
                lines: [
                    "{\"text\":\"line0\"}".to_owned(),
                    "{\"text\":\"line1\"}".to_owned(),
                    "".to_owned(),
                    "{\"text\":\"line3\"}".to_owned(),
                ],
            }
        );
    }

    #[test]
    fn destroy_entities_decodes_id_list() {
        let mut body = PacketWriter::new();
        body.write_var_i32(2);
        body.write_var_i32(7);
        body.write_var_i32(9);
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x13, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::DestroyEntities {
                entity_ids: vec![7, 9]
            }
        );
    }

    #[test]
    fn read_slot_decodes_empty_slot() {
        let mut reader = PacketReader::new(&[0xff, 0xff]);
        assert_eq!(read_slot(&mut reader).unwrap(), None);
        assert!(reader.is_empty());
    }

    #[test]
    fn read_slot_decodes_item_without_nbt() {
        // id=1, count=1, damage=0, TAG_End (no NBT)
        let bytes = [0x00, 0x01, 0x01, 0x00, 0x00, 0x00];
        let mut reader = PacketReader::new(&bytes);
        assert_eq!(
            read_slot(&mut reader).unwrap(),
            Some(SlotItem::new(1, 1, 0))
        );
        assert!(reader.is_empty());
    }

    #[test]
    fn read_slot_parses_empty_compound_nbt() {
        // id=1, count=1, damage=0, then TAG_Compound, empty name, TAG_End.
        let bytes = [0x00, 0x01, 0x01, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00];
        let mut reader = PacketReader::new(&bytes);
        let item = read_slot(&mut reader).unwrap().unwrap();
        assert_eq!((item.id, item.count, item.damage), (1, 1, 0));
        assert_eq!(item.nbt, Some(std::collections::HashMap::new()));
        assert!(reader.is_empty());
    }

    #[test]
    fn read_slot_parses_nested_nbt_children() {
        // id=276, count=1, damage=3, then a compound with a string and a list.
        let mut bytes = vec![0x01, 0x14, 0x01, 0x00, 0x03];
        bytes.extend_from_slice(&[0x0a, 0x00, 0x00]); // TAG_Compound "" {
        bytes.extend_from_slice(&[0x08, 0x00, 0x01, b'x', 0x00, 0x02, b'h', b'i']); // String x: "hi"
        bytes.extend_from_slice(&[0x09, 0x00, 0x01, b'l', 0x03, 0x00, 0x00, 0x00, 0x02]); // List l: 2x Int
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x08]);
        bytes.push(0x00); // TAG_End }
        let mut reader = PacketReader::new(&bytes);
        let item = read_slot(&mut reader).unwrap().unwrap();
        assert_eq!((item.id, item.count, item.damage), (276, 1, 3));
        let nbt = item.nbt.as_ref().unwrap();
        assert_eq!(nbt.get("x").and_then(|t| t.as_str()), Some("hi"));
        assert_eq!(nbt.get("l").and_then(|t| t.as_list()).map(|l| l.len()), Some(2));
        assert!(reader.is_empty());
    }

    #[test]
    fn clientbound_window_items_decodes_slot_list() {
        let mut body = PacketWriter::new();
        body.write_u8(0); // window id
        body.write_i16(2); // slot count
        body.write_i16(-1); // slot 0: empty
        body.write_i16(1); // slot 1: stone x1
        body.write_u8(1);
        body.write_i16(0);
        body.write_u8(0); // no NBT

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x30, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::WindowItems {
                window_id: 0,
                items: vec![
                    None,
                    Some(SlotItem::new(1, 1, 0)),
                ],
            }
        );
    }

    #[test]
    fn clientbound_set_slot_decodes_item() {
        let mut body = PacketWriter::new();
        body.write_i8(0); // window id
        body.write_i16(36); // slot
        body.write_i16(4); // cobblestone
        body.write_u8(64);
        body.write_i16(0);
        body.write_u8(0); // no NBT

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x2f, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::SetSlot {
                window_id: 0,
                slot: 36,
                item: Some(SlotItem::new(4, 64, 0)),
            }
        );
    }

    #[test]
    fn clientbound_held_item_change_decodes_slot() {
        let packet = ClientboundPlayPacket::from_frame(PacketFrame::new(0x09, vec![3])).unwrap();
        assert_eq!(packet, ClientboundPlayPacket::HeldItemChange { slot: 3 });
    }

    #[test]
    fn clientbound_entity_velocity_converts_shorts() {
        let mut body = PacketWriter::new();
        body.write_var_i32(7);
        body.write_i16(8000); // 1.0 block/tick
        body.write_i16(-4000); // -0.5 block/tick
        body.write_i16(0);

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x12, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::EntityVelocity {
                entity_id: 7,
                vx: 1.0,
                vy: -0.5,
                vz: 0.0,
            }
        );
    }

    #[test]
    fn clientbound_entity_properties_decodes_attributes_and_modifiers() {
        let uuid = [
            0x66, 0x2a, 0x6b, 0x8d, 0xda, 0x3e, 0x4c, 0x1c, 0x88, 0x13, 0x96, 0xea, 0x60, 0x97,
            0x27, 0x8d,
        ];
        let mut body = PacketWriter::new();
        body.write_var_i32(7); // entity id
        body.write_i32(1); // property count
        body.write_string("generic.movementSpeed");
        body.write_f64(0.1);
        body.write_var_i32(1); // modifier count
        body.write_bytes(&uuid);
        body.write_f64(0.3);
        body.write_u8(2);

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x20, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::EntityProperties {
                entity_id: 7,
                properties: vec![EntityProperty {
                    key: "generic.movementSpeed".to_owned(),
                    base: 0.1,
                    modifiers: vec![AttributeModifier {
                        uuid,
                        amount: 0.3,
                        operation: 2,
                    }],
                }],
            }
        );
    }

    #[test]
    fn clientbound_entity_effect_decodes_fields() {
        let mut body = PacketWriter::new();
        body.write_var_i32(7); // entity id
        body.write_i8(22); // absorption
        body.write_i8(1); // amplifier (level II)
        body.write_var_i32(600); // duration ticks
        body.write_i8(0); // hide particles
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x1d, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::EntityEffect {
                entity_id: 7,
                effect_id: 22,
                amplifier: 1,
                duration: 600,
                hide_particles: 0,
            }
        );
    }

    #[test]
    fn clientbound_remove_entity_effect_decodes_fields() {
        let mut body = PacketWriter::new();
        body.write_var_i32(7); // entity id
        body.write_i8(19); // poison
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x1e, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::RemoveEntityEffect {
                entity_id: 7,
                effect_id: 19,
            }
        );
    }

    #[test]
    fn clientbound_abilities_decodes_flags_and_speeds() {
        let mut body = PacketWriter::new();
        body.write_u8(0x0d); // invulnerable + allow flying + creative, not flying
        body.write_f32(0.05);
        body.write_f32(0.1);
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x39, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::PlayerAbilities {
                invulnerable: true,
                flying: false,
                allow_flying: true,
                creative: true,
                fly_speed: 0.05,
                walk_speed: 0.1,
            }
        );
    }

    #[test]
    fn serverbound_abilities_writes_vanilla_layout() {
        let frame = ServerboundPacket::PlayerAbilities {
            invulnerable: false,
            flying: true,
            allow_flying: true,
            creative: false,
            fly_speed: 0.05,
            walk_speed: 0.1,
        }
        .into_frame();
        assert_eq!(frame.id, 0x13);
        let mut reader = PacketReader::new(&frame.body);
        assert_eq!(reader.read_u8().unwrap(), 0x06); // flying | allow flying
        assert_eq!(reader.read_f32().unwrap(), 0.05);
        assert_eq!(reader.read_f32().unwrap(), 0.1);
        assert!(reader.is_empty());
    }

    #[test]
    fn serverbound_chat_writes_plain_string() {
        let frame = ServerboundPacket::ChatMessage {
            message: "hello".to_owned(),
        }
        .into_frame();
        assert_eq!(frame.id, 0x01);
        let mut reader = PacketReader::new(&frame.body);
        assert_eq!(reader.read_string(100).unwrap(), "hello");
        assert!(reader.is_empty());
    }

    #[test]
    fn serverbound_tab_complete_writes_text_and_flag() {
        let frame = ServerboundPacket::TabComplete {
            text: "/te".to_owned(),
            has_position: false,
        }
        .into_frame();
        assert_eq!(frame.id, 0x14);
        let mut reader = PacketReader::new(&frame.body);
        assert_eq!(reader.read_string(100).unwrap(), "/te");
        assert!(!reader.read_bool().unwrap());
        assert!(reader.is_empty());
    }

    #[test]
    fn clientbound_tab_complete_decodes_match_list() {
        let mut body = PacketWriter::new();
        body.write_var_i32(2);
        body.write_string("/tell");
        body.write_string("/time");
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x3a, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::TabComplete {
                matches: vec!["/tell".to_owned(), "/time".to_owned()],
            }
        );
    }

    #[test]
    fn clientbound_chat_decodes_json_and_position() {
        let mut body = PacketWriter::new();
        body.write_string(r#"{"text":"hi"}"#);
        body.write_i8(2);
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x02, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::ChatMessage {
                json: r#"{"text":"hi"}"#.to_owned(),
                position: 2,
            }
        );
    }

    #[test]
    fn clientbound_title_decodes_text_and_times() {
        let mut body = PacketWriter::new();
        body.write_var_i32(0);
        body.write_string(r#"{"text":"Main","color":"gold"}"#);
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x45, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::Title {
                action: TitleAction::Title {
                    json: r#"{"text":"Main","color":"gold"}"#.to_owned(),
                },
            }
        );

        let mut body = PacketWriter::new();
        body.write_var_i32(1);
        body.write_string(r#"{"text":"Sub"}"#);
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x45, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::Title {
                action: TitleAction::Subtitle {
                    json: r#"{"text":"Sub"}"#.to_owned(),
                },
            }
        );

        let mut body = PacketWriter::new();
        body.write_var_i32(2);
        body.write_i32(10);
        body.write_i32(70);
        body.write_i32(20);
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x45, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::Title {
                action: TitleAction::Times {
                    fade_in: 10,
                    stay: 70,
                    fade_out: 20,
                },
            }
        );
    }

    #[test]
    fn clientbound_title_decodes_clear_and_reset() {
        for (wire, expected) in [(3, TitleAction::Clear), (4, TitleAction::Reset)] {
            let mut body = PacketWriter::new();
            body.write_var_i32(wire);
            let packet =
                ClientboundPlayPacket::from_frame(PacketFrame::new(0x45, body.into_inner()))
                    .unwrap();
            assert_eq!(packet, ClientboundPlayPacket::Title { action: expected });
        }
    }

    #[test]
    fn scoreboard_objective_create_decodes_display_name() {
        let mut body = PacketWriter::new();
        body.write_string("obj");
        body.write_u8(0);
        body.write_string("Title");
        body.write_string("integer");
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x3b, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::ScoreboardObjective {
                name: "obj".to_owned(),
                mode: 0,
                display_name: "Title".to_owned(),
            }
        );
    }

    #[test]
    fn scoreboard_objective_remove_has_no_display_name() {
        let mut body = PacketWriter::new();
        body.write_string("obj");
        body.write_u8(1);
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x3b, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::ScoreboardObjective {
                name: "obj".to_owned(),
                mode: 1,
                display_name: String::new(),
            }
        );
    }

    #[test]
    fn update_score_decodes_set_and_remove() {
        let mut body = PacketWriter::new();
        body.write_string("line1");
        body.write_u8(0);
        body.write_string("obj");
        body.write_var_i32(42);
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x3c, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::UpdateScore {
                name: "line1".to_owned(),
                objective: "obj".to_owned(),
                value: Some(42),
            }
        );

        let mut body = PacketWriter::new();
        body.write_string("line1");
        body.write_u8(1);
        body.write_string("");
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x3c, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::UpdateScore {
                name: "line1".to_owned(),
                objective: String::new(),
                value: None,
            }
        );
    }

    #[test]
    fn display_scoreboard_decodes_sidebar_slot() {
        let mut body = PacketWriter::new();
        body.write_i8(1);
        body.write_string("obj");
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x3d, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::DisplayScoreboard {
                position: 1,
                objective: "obj".to_owned(),
            }
        );
    }

    #[test]
    fn teams_create_decodes_prefix_suffix_players() {
        let mut body = PacketWriter::new();
        body.write_string("team1");
        body.write_u8(0);
        body.write_string("Team One"); // display name (skipped)
        body.write_string("\u{a7}aPRE ");
        body.write_string(" \u{a7}cSUF");
        body.write_u8(3); // friendly fire
        body.write_string("always"); // visibility
        body.write_i8(2); // color
        body.write_var_i32(2);
        body.write_string("line1");
        body.write_string("line2");
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x3e, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::Teams {
                name: "team1".to_owned(),
                action: TeamAction::Create {
                    prefix: "\u{a7}aPRE ".to_owned(),
                    suffix: " \u{a7}cSUF".to_owned(),
                    players: vec!["line1".to_owned(), "line2".to_owned()],
                },
            }
        );
    }

    #[test]
    fn teams_membership_updates_decode() {
        let mut body = PacketWriter::new();
        body.write_string("team1");
        body.write_u8(4);
        body.write_var_i32(1);
        body.write_string("line1");
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x3e, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::Teams {
                name: "team1".to_owned(),
                action: TeamAction::RemovePlayers {
                    players: vec!["line1".to_owned()],
                },
            }
        );
    }

    fn encoded_block_pos(x: i32, y: i32, z: i32) -> [u8; 8] {
        let value = ((x as u64) & 0x03ff_ffff) << 38
            | ((y as u64) & 0x0fff) << 26
            | ((z as u64) & 0x03ff_ffff);
        value.to_be_bytes()
    }

    #[test]
    fn spawn_player_keeps_the_uuid() {
        let mut body = PacketWriter::new();
        body.write_var_i32(7);
        let uuid = [9u8; 16];
        body.write_bytes(&uuid);
        body.write_i32(32 * 10); // x = 10.0
        body.write_i32(32 * 64); // y = 64.0
        body.write_i32(32 * -3); // z = -3.0
        body.write_i8(64); // yaw 90°
        body.write_i8(0);
        body.write_i16(0); // current item
        body.write_u8(0x7f); // empty metadata

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x0c, body.into_inner())).unwrap();
        match packet {
            ClientboundPlayPacket::SpawnPlayer {
                entity_id,
                uuid: got,
                x,
                yaw,
                ..
            } => {
                assert_eq!(entity_id, 7);
                assert_eq!(got, uuid);
                assert!((x - 10.0).abs() < 1e-9);
                assert!((yaw - 90.0).abs() < 1e-3);
            }
            other => panic!("expected SpawnPlayer, got {other:?}"),
        }
    }

    #[test]
    fn player_list_item_add_decodes_profile_and_textures() {
        let mut body = PacketWriter::new();
        body.write_var_i32(0); // action ADD
        body.write_var_i32(1); // one entry
        body.write_bytes(&[1u8; 16]);
        body.write_string("Notch");
        body.write_var_i32(1); // one property
        body.write_string("textures");
        body.write_string("base64data");
        body.write_bool(true);
        body.write_string("sig");
        body.write_var_i32(1); // gamemode creative
        body.write_var_i32(42); // ping
        body.write_bool(true);
        body.write_string("{\"text\":\"Notch\"}");

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x38, body.into_inner())).unwrap();
        let ClientboundPlayPacket::PlayerListItem { entries } = packet else {
            panic!("expected PlayerListItem");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uuid, [1u8; 16]);
        match &entries[0].action {
            PlayerListItemAction::Add {
                name,
                properties,
                gamemode,
                ping,
                display_name,
            } => {
                assert_eq!(name, "Notch");
                assert_eq!(properties[0].name, "textures");
                assert_eq!(properties[0].value, "base64data");
                assert_eq!(properties[0].signature.as_deref(), Some("sig"));
                assert_eq!(*gamemode, 1);
                assert_eq!(*ping, 42);
                assert_eq!(display_name.as_deref(), Some("{\"text\":\"Notch\"}"));
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn entity_metadata_decodes_typed_entries() {
        let mut body = PacketWriter::new();
        body.write_var_i32(5);
        // index 0, type 0 (byte): entity flags
        body.write_u8((0 << 5) | 0);
        body.write_i8(0x02); // sneaking bit
        // index 10, type 5 (slot): item-entity contents
        body.write_u8((5 << 5) | 10);
        body.write_i16(1); // stone
        body.write_u8(3);
        body.write_i16(0);
        body.write_u8(0); // no NBT
        body.write_u8(0x7f); // terminator

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x1c, body.into_inner())).unwrap();
        let ClientboundPlayPacket::EntityMetadata { entity_id, metadata } = packet else {
            panic!("expected EntityMetadata");
        };
        assert_eq!(entity_id, 5);
        assert_eq!(metadata.len(), 2);
        assert_eq!(metadata[0].index, 0);
        assert_eq!(metadata[0].value, MetadataValue::Byte(0x02));
        assert_eq!(metadata[1].index, 10);
        assert_eq!(
            metadata[1].value,
            MetadataValue::Slot(Some(SlotItem::new(1, 3, 0)))
        );
    }

    #[test]
    fn click_window_round_trips_through_write_slot() {
        let frame = ServerboundPacket::ClickWindow {
            window_id: 0,
            slot: 36,
            button: 0,
            action_number: 1,
            mode: 0,
            clicked_item: Some(SlotItem::new(1, 64, 0)),
        }
        .into_frame();
        assert_eq!(frame.id, 0x0e);
        let mut r = PacketReader::new(&frame.body);
        assert_eq!(r.read_u8().unwrap(), 0);
        assert_eq!(r.read_i16().unwrap(), 36);
        assert_eq!(r.read_i8().unwrap(), 0);
        assert_eq!(r.read_i16().unwrap(), 1);
        assert_eq!(r.read_i8().unwrap(), 0);
        assert_eq!(read_slot(&mut r).unwrap(), Some(SlotItem::new(1, 64, 0)));
        assert!(r.is_empty());
    }

    #[test]
    fn attach_entity_decodes_ints_and_leash() {
        let mut body = PacketWriter::new();
        body.write_i32(5);
        body.write_i32(-1);
        body.write_bool(false);
        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x1b, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::AttachEntity {
                entity_id: 5,
                vehicle_id: -1,
                leash: false,
            }
        );
    }

    #[test]
    fn creative_action_and_close_window_encode_ids() {
        let creative = ServerboundPacket::CreativeInventoryAction {
            slot: 36,
            clicked_item: None,
        }
        .into_frame();
        assert_eq!(creative.id, 0x10);
        let mut r = PacketReader::new(&creative.body);
        assert_eq!(r.read_i16().unwrap(), 36);
        assert_eq!(r.read_i16().unwrap(), -1); // empty slot

        let close = ServerboundPacket::CloseWindow { window_id: 3 }.into_frame();
        assert_eq!(close.id, 0x0d);
        assert_eq!(close.body, vec![3]);
    }
}

/// 1.8 sends absolute entity coordinates as fixed-point integers (block * 32).
fn fixed_point(value: i32) -> f64 {
    value as f64 / 32.0
}

/// Relative-move deltas are the same fixed-point scale in a signed byte.
fn fixed_point_delta(value: i8) -> f64 {
    value as f64 / 32.0
}

/// 1.8 sends rotations as a signed byte covering the full 0..360 circle.
fn angle(value: i8) -> f32 {
    value as f32 * 360.0 / 256.0
}

fn bulk_chunk_data_len(primary_bit_mask: u16, sky_light_sent: bool) -> usize {
    let sections = primary_bit_mask.count_ones() as usize;
    let mut len = sections * (4096 * 2 + 2048);
    if sky_light_sent {
        len += sections * 2048;
    }
    len + 256
}

/// Write a 1.8 Slot: Short id (-1 = empty), Byte count, Short damage, and a
/// single 0 byte for the absent NBT tag. `recraft` never sends item NBT.
fn write_slot(body: &mut PacketWriter, item: Option<SlotItem>) {
    match item {
        None => body.write_i16(-1),
        Some(item) => {
            body.write_i16(item.id);
            body.write_u8(item.count);
            body.write_i16(item.damage);
            body.write_u8(0); // empty NBT
        }
    }
}

/// Read one S38 PlayerListItem entry body for the packet's shared `action`.
fn read_player_list_action(
    body: &mut PacketReader<'_>,
    action: i32,
) -> Result<PlayerListItemAction> {
    Ok(match action {
        0 => {
            let name = body.read_string(16)?;
            let prop_count = body.read_var_i32()?;
            if prop_count < 0 {
                return Err(ProtocolError::InvalidData("negative property count"));
            }
            let mut properties = Vec::with_capacity(prop_count.min(64) as usize);
            for _ in 0..prop_count {
                let name = body.read_string(32767)?;
                let value = body.read_string(32767)?;
                let signature = if body.read_bool()? {
                    Some(body.read_string(32767)?)
                } else {
                    None
                };
                properties.push(PlayerProperty {
                    name,
                    value,
                    signature,
                });
            }
            let gamemode = body.read_var_i32()?;
            let ping = body.read_var_i32()?;
            let display_name = if body.read_bool()? {
                Some(body.read_string(32767)?)
            } else {
                None
            };
            PlayerListItemAction::Add {
                name,
                properties,
                gamemode,
                ping,
                display_name,
            }
        }
        1 => PlayerListItemAction::UpdateGameMode {
            gamemode: body.read_var_i32()?,
        },
        2 => PlayerListItemAction::UpdateLatency {
            ping: body.read_var_i32()?,
        },
        3 => PlayerListItemAction::UpdateDisplayName {
            display_name: if body.read_bool()? {
                Some(body.read_string(32767)?)
            } else {
                None
            },
        },
        4 => PlayerListItemAction::Remove,
        _ => return Err(ProtocolError::InvalidData("unknown player-list action")),
    })
}

/// Read a 1.8 EntityMetadata array: type-tagged, index-headed entries
/// terminated by the 0x7f sentinel byte.
fn read_metadata(body: &mut PacketReader<'_>) -> Result<Vec<MetadataEntry>> {
    let mut entries = Vec::new();
    loop {
        let header = body.read_u8()?;
        if header == 0x7f {
            return Ok(entries);
        }
        let index = header & 0x1f;
        let value = match header >> 5 {
            0 => MetadataValue::Byte(body.read_i8()?),
            1 => MetadataValue::Short(body.read_i16()?),
            2 => MetadataValue::Int(body.read_i32()?),
            3 => MetadataValue::Float(body.read_f32()?),
            4 => MetadataValue::Str(body.read_string(32767)?),
            5 => MetadataValue::Slot(read_slot(body)?),
            6 => MetadataValue::IntVec(body.read_i32()?, body.read_i32()?, body.read_i32()?),
            7 => MetadataValue::FloatVec(body.read_f32()?, body.read_f32()?, body.read_f32()?),
            _ => return Err(ProtocolError::InvalidData("unknown metadata type")),
        };
        entries.push(MetadataEntry { index, value });
    }
}

/// Read a 1.8 Slot: Short id (-1 = empty), then Byte count, Short damage and
/// an optional NBT compound.
pub fn read_slot(body: &mut PacketReader<'_>) -> Result<Option<SlotItem>> {
    let id = body.read_i16()?;
    if id == -1 {
        return Ok(None);
    }
    let count = body.read_u8()?;
    let damage = body.read_i16()?;
    let nbt = crate::nbt::read_slot_nbt(body)?;
    Ok(Some(SlotItem { id, count, damage, nbt }))
}


fn read_block_pos(reader: &mut PacketReader<'_>) -> Result<(i32, i32, i32)> {
    let value = reader.read_i64()? as u64;
    let x = sign_extend((value >> 38) & 0x03ff_ffff, 26);
    let y = sign_extend((value >> 26) & 0x0fff, 12);
    let z = sign_extend(value & 0x03ff_ffff, 26);
    Ok((x, y, z))
}

fn sign_extend(value: u64, bits: u32) -> i32 {
    let shift = 64 - bits;
    ((value << shift) as i64 >> shift) as i32
}

fn decode_legacy_block_state_id(state_id: i32) -> Result<(u16, u8)> {
    if !(0..=0xffff).contains(&state_id) {
        return Err(ProtocolError::InvalidData(
            "legacy block state id out of range",
        ));
    }
    let value = state_id as u16;
    Ok((value >> 4, (value & 15) as u8))
}
