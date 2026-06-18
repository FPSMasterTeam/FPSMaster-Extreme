//! The shared JSON bridge used by *both* the JS and native extension backends.
//!
//! Structured data crosses the mod boundary as JSON. The host exposes three
//! operations to a running hook — enqueue a command, answer a read-view query,
//! record a HUD draw — implemented here against a thread-local "current dispatch
//! context" that the backend sets (via [`cur`] guards) for the duration of each
//! synchronous hook call. The JS backend wires these to QuickJS native globals;
//! the native backend wires them to `extern "C"` function pointers.

use serde_json::{json, Value};

use crate::command::{EntityFilter, ExtCommand, LogLevel, RenderPreset};
use crate::event::ExtEvent;
use crate::hud::{HudCtx, HudDraw};
use crate::input::InputEvent;
use crate::packet::{PacketBuild, PacketType, PacketView};
use crate::view::ReadViews;

/// The current synchronous dispatch context, exposed to mod code (JS native
/// functions / native fn pointers) for the duration of one hook call.
pub(crate) mod cur {
    use super::*;
    use std::cell::Cell;

    type ViewsPtr = *const (dyn ReadViews + 'static);

    thread_local! {
        static VIEWS: Cell<Option<ViewsPtr>> = const { Cell::new(None) };
        static COMMANDS: Cell<Option<*mut Vec<ExtCommand>>> = const { Cell::new(None) };
        static HUD: Cell<Option<*mut HudDraw>> = const { Cell::new(None) };
    }

    /// Sets the live read-views for the duration of a hook call.
    pub struct ViewsGuard;
    impl ViewsGuard {
        pub fn enter(v: &dyn ReadViews) -> Self {
            // SAFETY: dropped at the end of the synchronous hook call, before
            // `v`'s borrow ends, so the erased pointer never dangles.
            let erased: ViewsPtr = unsafe { std::mem::transmute(v as *const dyn ReadViews) };
            VIEWS.with(|c| c.set(Some(erased)));
            ViewsGuard
        }
    }
    impl Drop for ViewsGuard {
        fn drop(&mut self) {
            VIEWS.with(|c| c.set(None));
        }
    }

    pub struct CommandsGuard;
    impl CommandsGuard {
        pub fn enter(c: &mut Vec<ExtCommand>) -> Self {
            COMMANDS.with(|cell| cell.set(Some(c as *mut _)));
            CommandsGuard
        }
    }
    impl Drop for CommandsGuard {
        fn drop(&mut self) {
            COMMANDS.with(|c| c.set(None));
        }
    }

    pub struct HudGuard;
    impl HudGuard {
        pub fn enter(h: &mut HudDraw) -> Self {
            HUD.with(|cell| cell.set(Some(h as *mut _)));
            HudGuard
        }
    }
    impl Drop for HudGuard {
        fn drop(&mut self) {
            HUD.with(|c| c.set(None));
        }
    }

    pub fn with_views<R>(f: impl FnOnce(&dyn ReadViews) -> R) -> Option<R> {
        VIEWS.with(|c| c.get()).map(|p| {
            // SAFETY: only set during a hook call; the pointee outlives the call.
            let v: &dyn ReadViews = unsafe { &*p };
            f(v)
        })
    }

    pub fn push_command(cmd: ExtCommand) {
        if let Some(p) = COMMANDS.with(|c| c.get()) {
            // SAFETY: set for the duration of the hook; exclusive on this thread.
            unsafe { (*p).push(cmd) };
        }
    }

    pub fn with_hud<R>(f: impl FnOnce(&mut HudDraw) -> R) -> Option<R> {
        HUD.with(|c| c.get()).map(|p| {
            // SAFETY: set for the duration of draw_hud; exclusive on this thread.
            let h: &mut HudDraw = unsafe { &mut *p };
            f(h)
        })
    }
}

// ---- mod -> host operations (JSON) ----

/// Enqueue a command from a `{"t":..}` JSON payload.
pub(crate) fn handle_cmd(json: &str) {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return;
    };
    let t = v.get("t").and_then(Value::as_str).unwrap_or("");
    let cmd = match t {
        "chat" => Some(ExtCommand::Chat(str_field(&v, "s"))),
        "packet" => v
            .get("p")
            .and_then(parse_packet_build)
            .map(ExtCommand::SendServerbound),
        "log" => Some(ExtCommand::Log(
            log_level(v.get("l").and_then(Value::as_i64).unwrap_or(2)),
            str_field(&v, "m"),
        )),
        "particle" => Some(ExtCommand::SpawnParticle {
            kind: int_field(&v, "kind") as i32,
            x: f64_field(&v, "x"),
            y: f64_field(&v, "y"),
            z: f64_field(&v, "z"),
            ox: f64_field(&v, "ox") as f32,
            oy: f64_field(&v, "oy") as f32,
            oz: f64_field(&v, "oz") as f32,
            speed: f64_field(&v, "speed") as f32,
            count: int_field(&v, "count") as i32,
        }),
        "sound" => Some(ExtCommand::PlaySound {
            event: str_field(&v, "event"),
            x: f64_field(&v, "x"),
            y: f64_field(&v, "y"),
            z: f64_field(&v, "z"),
            volume: f64_field(&v, "volume") as f32,
            pitch: f64_field(&v, "pitch") as f32,
        }),
        "render" => parse_render_preset(&v).map(ExtCommand::Render),
        "block" => Some(ExtCommand::RegisterBlock {
            id: int_field(&v, "id") as u16,
            texture: {
                let t = str_field(&v, "texture");
                if t.is_empty() {
                    "stone".to_string()
                } else {
                    t
                }
            },
            opaque: v.get("opaque").and_then(Value::as_bool).unwrap_or(true),
            alpha: v.get("alpha").and_then(Value::as_f64).unwrap_or(1.0) as f32,
            luminance: int_field(&v, "lum").clamp(0, 15) as u8,
            tint: rgb_field(&v, "tint"),
        }),
        "place" => Some(ExtCommand::PlaceBlock {
            x: int_field(&v, "x") as i32,
            y: int_field(&v, "y") as i32,
            z: int_field(&v, "z") as i32,
            face: int_field(&v, "face") as u8,
            cursor: [
                int_field(&v, "cx").clamp(0, 15) as u8,
                int_field(&v, "cy").clamp(0, 15) as u8,
                int_field(&v, "cz").clamp(0, 15) as u8,
            ],
        }),
        "dig" => Some(ExtCommand::Digging {
            status: int_field(&v, "status") as u8,
            x: int_field(&v, "x") as i32,
            y: int_field(&v, "y") as i32,
            z: int_field(&v, "z") as i32,
            face: int_field(&v, "face") as u8,
        }),
        "attack" => Some(ExtCommand::AttackEntity {
            id: int_field(&v, "id") as i32,
        }),
        "interact" => Some(ExtCommand::InteractEntity {
            id: int_field(&v, "id") as i32,
            at: if v.get("ax").is_some() {
                Some([
                    f64_field(&v, "ax") as f32,
                    f64_field(&v, "ay") as f32,
                    f64_field(&v, "az") as f32,
                ])
            } else {
                None
            },
        }),
        "click" => Some(ExtCommand::ContainerClick {
            slot: int_field(&v, "slot") as i16,
            button: int_field(&v, "button") as i8,
            mode: int_field(&v, "mode") as i8,
        }),
        "close" => Some(ExtCommand::ContainerClose),
        "openInv" => Some(ExtCommand::OpenInventory),
        "selectSlot" => Some(ExtCommand::SelectSlot {
            slot: int_field(&v, "slot") as i32,
        }),
        "swing" => Some(ExtCommand::SwingArm),
        "useItem" => Some(ExtCommand::UseItem),
        "rotate" => Some(ExtCommand::SetRotation {
            yaw: f64_field(&v, "yaw") as f32,
            pitch: f64_field(&v, "pitch") as f32,
            silent: v.get("silent").and_then(Value::as_bool).unwrap_or(false),
        }),
        "clearRotate" => Some(ExtCommand::ClearSilentRotation),
        "saveConfig" => Some(ExtCommand::SaveConfig {
            dir: str_field(&v, "dir"),
            json: str_field(&v, "json"),
        }),
        _ => None,
    };
    if let Some(cmd) = cmd {
        cur::push_command(cmd);
    }
}

/// Parse an optional `[r, g, b]` (0..1) color array field.
fn rgb_field(v: &Value, k: &str) -> Option<[f32; 3]> {
    let arr = v.get(k)?.as_array()?;
    if arr.len() < 3 {
        return None;
    }
    Some([
        arr[0].as_f64().unwrap_or(0.0) as f32,
        arr[1].as_f64().unwrap_or(0.0) as f32,
        arr[2].as_f64().unwrap_or(0.0) as f32,
    ])
}

/// Answer a read-view query from a `{"k":..}` JSON payload, returning JSON.
pub(crate) fn handle_query(json: &str) -> String {
    let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    let kind = v.get("k").and_then(Value::as_str).unwrap_or("");
    cur::with_views(|views| match kind {
        "player" => {
            let p = views.player();
            json!({
                "x": p.x, "y": p.y, "z": p.z, "yaw": p.yaw, "pitch": p.pitch,
                "vx": p.vx, "vy": p.vy, "vz": p.vz, "onGround": p.on_ground,
                "health": p.health, "food": p.food, "sneaking": p.sneaking, "sprinting": p.sprinting
            })
            .to_string()
        }
        "block" => {
            let b = views.block_at(
                int_field(&v, "x") as i32,
                int_field(&v, "y") as i32,
                int_field(&v, "z") as i32,
            );
            json!({
                "id": b.id, "meta": b.meta, "isAir": b.is_air,
                "luminance": b.luminance, "opaque": b.opaque, "shape": b.shape
            })
            .to_string()
        }
        "entities" => {
            let list: Vec<Value> = views
                .entities()
                .into_iter()
                .map(|e| {
                    let (kind, type_id) = entity_kind_js(e.kind);
                    json!({
                        "id": e.id, "kind": kind, "typeId": type_id,
                        "x": e.x, "y": e.y, "z": e.z, "yaw": e.yaw, "pitch": e.pitch,
                        "onGround": e.on_ground, "name": e.name, "health": e.health
                    })
                })
                .collect();
            Value::Array(list).to_string()
        }
        "time" => views.world_time().to_string(),
        "dim" => views.dimension().to_string(),
        "chunks" => views.loaded_chunk_count().to_string(),
        "connected" => views.connected().to_string(),
        "held" => item_json(views.held_item()),
        "selectedSlot" => views.selected_slot().to_string(),
        "inventory" => {
            let list: Vec<Value> = views
                .inventory()
                .into_iter()
                .map(|it| match it {
                    Some(i) => json!({ "id": i.id, "count": i.count, "damage": i.damage }),
                    None => Value::Null,
                })
                .collect();
            Value::Array(list).to_string()
        }
        "capabilities" => {
            let c = views.capabilities();
            json!({
                "invulnerable": c.invulnerable, "flying": c.flying,
                "allowFlying": c.allow_flying, "creative": c.creative,
                "flySpeed": c.fly_speed, "walkSpeed": c.walk_speed
            })
            .to_string()
        }
        "effects" => {
            let list: Vec<Value> = views
                .effects()
                .into_iter()
                .map(|e| json!({ "id": e.id, "amplifier": e.amplifier, "duration": e.duration }))
                .collect();
            Value::Array(list).to_string()
        }
        "xp" => {
            let (bar, level) = views.xp();
            json!({ "bar": bar, "level": level }).to_string()
        }
        "container" => match views.open_container() {
            Some(c) => json!({ "windowId": c.window_id, "kind": c.kind, "size": c.size }).to_string(),
            None => "null".to_string(),
        },
        _ => "null".to_string(),
    })
    .unwrap_or_else(|| "null".to_string())
}

/// Record a HUD draw from an `{"o":..}` JSON payload.
pub(crate) fn handle_hud(json: &str) {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return;
    };
    let op = v.get("o").and_then(Value::as_str).unwrap_or("");
    cur::with_hud(|hud| {
        let (x, y) = (int_field(&v, "x") as i32, int_field(&v, "y") as i32);
        match op {
            "rect" => hud.rect(
                x,
                y,
                int_field(&v, "w") as i32,
                int_field(&v, "h") as i32,
                int_field(&v, "c") as u32,
            ),
            "text" => {
                let s = int_field(&v, "s") as i32;
                let c = int_field(&v, "c") as u32;
                let text = str_field(&v, "text");
                if int_field(&v, "sh") != 0 {
                    hud.text(x, y, s, c, text);
                } else {
                    hud.text_plain(x, y, s, c, text);
                }
            }
            "item" => {
                let sz = int_field(&v, "sz") as i32;
                hud.item_icon(x, y, sz, sz, int_field(&v, "id") as i16);
            }
            "block" => {
                let sz = int_field(&v, "sz") as i32;
                hud.block_item(
                    x,
                    y,
                    sz,
                    sz,
                    int_field(&v, "id") as u16,
                    int_field(&v, "meta") as u8,
                );
            }
            _ => {}
        }
    });
}

// ---- host -> mod projections (JSON) ----

pub(crate) fn packet_view_json(p: &PacketView) -> String {
    let ty = packet_type_name(p.ty());
    let body = match p {
        PacketView::KeepAlive { id } => json!({ "id": id }),
        PacketView::Chat { text, position, json: raw } => {
            json!({ "text": text, "position": position, "json": raw })
        }
        PacketView::BlockChange { x, y, z, id, meta } => {
            json!({ "x": x, "y": y, "z": z, "id": id, "meta": meta })
        }
        PacketView::SpawnMob { id, kind, x, y, z } => {
            json!({ "id": id, "mobKind": kind, "x": x, "y": y, "z": z })
        }
        PacketView::SpawnPlayer { id, x, y, z } => json!({ "id": id, "x": x, "y": y, "z": z }),
        PacketView::DestroyEntities { ids } => json!({ "ids": ids }),
        PacketView::UpdateHealth { health, food } => json!({ "health": health, "food": food }),
        PacketView::SoundEffect { name, x, y, z, volume, pitch } => {
            json!({ "name": name, "x": x, "y": y, "z": z, "volume": volume, "pitch": pitch })
        }
        PacketView::OutChat { message } => json!({ "message": message }),
        PacketView::OutPlayerPosition { x, y, z, on_ground } => {
            json!({ "x": x, "y": y, "z": z, "onGround": on_ground })
        }
        PacketView::OutPlayerDigging { status, x, y, z, face } => {
            json!({ "status": status, "x": x, "y": y, "z": z, "face": face })
        }
        PacketView::Other { raw_id, .. } => json!({ "rawId": raw_id }),
    };
    merge_type(ty, body)
}

pub(crate) fn event_json(e: &ExtEvent) -> String {
    let (ty, body) = match e {
        ExtEvent::BlockChange { x, y, z, id, meta } => {
            ("BlockChange", json!({ "x": x, "y": y, "z": z, "id": id, "meta": meta }))
        }
        ExtEvent::ChunkLoad { x, z } => ("ChunkLoad", json!({ "x": x, "z": z })),
        ExtEvent::ChunkUnload { x, z } => ("ChunkUnload", json!({ "x": x, "z": z })),
        ExtEvent::Chat { text, position, json: raw } => {
            ("Chat", json!({ "text": text, "position": position, "json": raw }))
        }
        ExtEvent::EntitySpawn { id, kind, x, y, z } => {
            let (k, tid) = entity_kind_js(*kind);
            ("EntitySpawn", json!({ "id": id, "kind": k, "typeId": tid, "x": x, "y": y, "z": z }))
        }
        ExtEvent::EntityRemove { id } => ("EntityRemove", json!({ "id": id })),
        ExtEvent::PlayerHealth { health, food } => {
            ("PlayerHealth", json!({ "health": health, "food": food }))
        }
        ExtEvent::Disconnected { reason } => ("Disconnected", json!({ "reason": reason })),
    };
    merge_type(ty, body)
}

pub(crate) fn input_json(input: &InputEvent) -> String {
    json!({ "key": input.key, "pressed": input.pressed }).to_string()
}

pub(crate) fn hud_ctx_json(ctx: &HudCtx) -> String {
    json!({ "width": ctx.width, "height": ctx.height, "scale": ctx.scale,
        "screenOpen": ctx.screen_open })
    .to_string()
}

// ---- small json helpers ----

fn merge_type(ty: &str, mut body: Value) -> String {
    if let Value::Object(map) = &mut body {
        map.insert("type".to_string(), Value::String(ty.to_string()));
    }
    body.to_string()
}

fn str_field(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}
fn int_field(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}
fn f64_field(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}
fn log_level(l: i64) -> LogLevel {
    match l {
        0 => LogLevel::Error,
        1 => LogLevel::Warn,
        3 => LogLevel::Debug,
        _ => LogLevel::Info,
    }
}

fn item_json(item: Option<crate::view::ItemView>) -> String {
    match item {
        Some(i) => json!({ "id": i.id, "count": i.count, "damage": i.damage }).to_string(),
        None => "null".to_string(),
    }
}

fn entity_kind_js(kind: crate::view::EntityKindView) -> (&'static str, i64) {
    use crate::view::EntityKindView as K;
    match kind {
        K::Player => ("player", -1),
        K::Mob(id) => ("mob", id as i64),
        K::Object(id) => ("object", id as i64),
        K::ExperienceOrb => ("orb", -1),
    }
}

fn parse_render_preset(v: &Value) -> Option<RenderPreset> {
    let unpack = |c: u32| {
        [
            ((c >> 24) & 255) as f32 / 255.0,
            ((c >> 16) & 255) as f32 / 255.0,
            ((c >> 8) & 255) as f32 / 255.0,
        ]
    };
    let on = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    match v.get("r").and_then(Value::as_str).unwrap_or("") {
        "blockTint" => {
            let meta = int_field(v, "meta");
            Some(RenderPreset::BlockTint {
                block_id: int_field(v, "id") as u16,
                meta: if meta < 0 { None } else { Some(meta as u8) },
                color: unpack(int_field(v, "color") as u32),
            })
        }
        "fullbright" => Some(RenderPreset::Fullbright(on("on"))),
        "chunkBorders" => Some(RenderPreset::ChunkBorders(on("on"))),
        "entityBox" => Some(RenderPreset::EntityBox {
            kind: match v.get("filter").and_then(Value::as_str).unwrap_or("") {
                "players" => Some(EntityFilter::Players),
                "mobs" => Some(EntityFilter::Mobs),
                "items" => Some(EntityFilter::Items),
                _ => None,
            },
            color: unpack(int_field(v, "color") as u32),
            enabled: on("on"),
        }),
        "nametagScale" => Some(RenderPreset::NametagScale(f64_field(v, "v") as f32)),
        "particleDensity" => Some(RenderPreset::ParticleDensity(f64_field(v, "v") as f32)),
        _ => None,
    }
}

fn parse_packet_build(p: &Value) -> Option<PacketBuild> {
    match p.get("type").and_then(Value::as_str).unwrap_or("") {
        "chat" => Some(PacketBuild::Chat(str_field(p, "message"))),
        "playerPosition" => Some(PacketBuild::PlayerPosition {
            x: f64_field(p, "x"),
            y: f64_field(p, "y"),
            z: f64_field(p, "z"),
            on_ground: p.get("onGround").and_then(Value::as_bool).unwrap_or(true),
        }),
        "playerLook" => Some(PacketBuild::PlayerLook {
            yaw: f64_field(p, "yaw") as f32,
            pitch: f64_field(p, "pitch") as f32,
            on_ground: p.get("onGround").and_then(Value::as_bool).unwrap_or(true),
        }),
        "heldItemChange" => Some(PacketBuild::HeldItemChange {
            slot: int_field(p, "slot") as i16,
        }),
        "swingArm" => Some(PacketBuild::SwingArm),
        "playerDigging" => Some(PacketBuild::PlayerDigging {
            status: int_field(p, "status") as u8,
            x: int_field(p, "x") as i32,
            y: int_field(p, "y") as i32,
            z: int_field(p, "z") as i32,
            face: int_field(p, "face") as u8,
        }),
        _ => None,
    }
}

fn packet_type_name(ty: PacketType) -> &'static str {
    use PacketType as T;
    match ty {
        T::KeepAlive => "KeepAlive",
        T::JoinGame => "JoinGame",
        T::Respawn => "Respawn",
        T::ChatMessage => "ChatMessage",
        T::BlockChange => "BlockChange",
        T::MultiBlockChange => "MultiBlockChange",
        T::ChunkData => "ChunkData",
        T::ChunkBulk => "ChunkBulk",
        T::SpawnPlayer => "SpawnPlayer",
        T::SpawnMob => "SpawnMob",
        T::SpawnObject => "SpawnObject",
        T::SpawnExperienceOrb => "SpawnExperienceOrb",
        T::EntityMove => "EntityMove",
        T::EntityTeleport => "EntityTeleport",
        T::EntityVelocity => "EntityVelocity",
        T::DestroyEntities => "DestroyEntities",
        T::EntityMetadata => "EntityMetadata",
        T::PlayerPositionLook => "PlayerPositionLook",
        T::UpdateHealth => "UpdateHealth",
        T::SetExperience => "SetExperience",
        T::SetSlot => "SetSlot",
        T::WindowItems => "WindowItems",
        T::HeldItemChange => "HeldItemChange",
        T::SoundEffect => "SoundEffect",
        T::SpawnParticle => "SpawnParticle",
        T::Effect => "Effect",
        T::BlockAction => "BlockAction",
        T::TimeUpdate => "TimeUpdate",
        T::Disconnect => "Disconnect",
        T::ClientboundOther => "ClientboundOther",
        T::SbChatMessage => "SbChatMessage",
        T::SbPlayerPosition => "SbPlayerPosition",
        T::SbPlayerLook => "SbPlayerLook",
        T::SbPlayerPositionLook => "SbPlayerPositionLook",
        T::SbPlayerDigging => "SbPlayerDigging",
        T::SbPlayerBlockPlacement => "SbPlayerBlockPlacement",
        T::SbHeldItemChange => "SbHeldItemChange",
        T::SbAnimation => "SbAnimation",
        T::SbUseEntity => "SbUseEntity",
        T::SbEntityAction => "SbEntityAction",
        T::SbKeepAlive => "SbKeepAlive",
        T::ServerboundOther => "ServerboundOther",
    }
}
