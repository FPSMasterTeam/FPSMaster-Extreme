//! Stable plugin ABI for recraft native (`cdylib`) extensions.
//!
//! This is the obfuscation-exempt contract a native mod compiles against. It
//! deliberately depends only on `abi_stable` — never on recraft internals — so
//! the host can refactor and obfuscate everything behind it while native mods
//! keep loading (`abi_stable` checks the type layout + version at load time and
//! rejects a mismatch).
//!
//! The data interchange mirrors the JS layer: structured values cross as JSON
//! strings, and a mod talks to the host through three function pointers in
//! [`HostApi`] (`cmd` / `query` / `hud`). That keeps this contract tiny (no need
//! to mirror every view/command type in `StableAbi` form) and identical in
//! semantics to the JS API, so docs and behaviour stay in lock-step.

// abi_stable's `#[sabi_trait]` macro emits impls inside an anonymous const and
// names generated types with underscores; both are intentional.
#![allow(non_local_definitions, non_camel_case_types)]

use abi_stable::{
    declare_root_module_statics,
    library::RootModule,
    package_version_strings,
    sabi_trait,
    sabi_types::VersionStrings,
    std_types::{RBox, RStr, RString, RVec},
    StableAbi,
};

/// A vertex for a native render hook: world position, RGBA color, and a UV into
/// the entity atlas (the host renders these with the entity/model pipeline, so a
/// solid-color mesh points its UVs at an opaque atlas texel and lets `color`
/// modulate). Mirrors the host's internal model vertex.
#[repr(C)]
#[derive(StableAbi, Copy, Clone, Debug, PartialEq)]
pub struct ExtVertex {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
    pub u: f32,
    pub v: f32,
}

/// The native API semver, mirrored from the manifest `api` requirement. The
/// `abi_stable` layout check is the second, stronger safety net.
pub const API_VERSION: (u32, u32, u32) = (0, 3, 0);

/// Everything a native mod needs in scope to declare its plugin and root module.
/// `use recraft_ext_api::prelude::*;` and implement [`NativePlugin`].
pub mod prelude {
    pub use crate::{
        ExtApi, ExtApiRef, ExtVertex, HostApi, NativePlugin, NativePlugin_TO, PluginObj,
    };
    pub use abi_stable::{
        export_root_module,
        prefix_type::PrefixTypeTrait,
        sabi_extern_fn,
        sabi_trait::TD_Opaque,
        std_types::{RBox, RStr, RString, RVec},
    };
}

/// Host-side entry points a plugin calls during a hook. All payloads are JSON
/// (same schema as the JS layer): `cmd` enqueues a command, `query` answers a
/// read-view, `hud` records a HUD draw. Valid only for the duration of the hook
/// call that handed it out.
#[repr(C)]
#[derive(StableAbi, Copy, Clone)]
pub struct HostApi {
    pub cmd: extern "C" fn(RString),
    pub query: extern "C" fn(RString) -> RString,
    pub hud: extern "C" fn(RString),
    /// Submit native render-hook geometry (replaces the previous submission;
    /// empty clears it). The trailing `u32` is a registered texture handle the
    /// UVs sample (`0` = the entity atlas, the pre-0.3 behaviour). Native-only —
    /// there is no JS equivalent.
    pub geometry: extern "C" fn(RVec<ExtVertex>, RVec<u32>, u32),
    /// Register an RGBA texture (`width*height*4` bytes, row-major, no padding)
    /// and return its handle. Draw it with `hud_image` / reference it from
    /// custom geometry. Added in 0.3.
    pub register_texture: extern "C" fn(RVec<u8>, u32, u32) -> u32,
    /// Decode a PNG/JPEG file at `path` and register it, returning its handle
    /// (0 on failure). Added in 0.3.
    pub load_texture: extern "C" fn(RString) -> u32,
    /// Replace a registered texture's pixels (`width*height*4` RGBA bytes,
    /// matching its current dimensions). Cheap per-frame path for streamed
    /// frames (e.g. an off-screen renderer). Added in 0.3.
    pub update_texture: extern "C" fn(u32, RVec<u8>),
    /// Drop a registered texture. Added in 0.3.
    pub free_texture: extern "C" fn(u32),
}

impl HostApi {
    /// Enqueue a command (JSON, e.g. `{"t":"chat","s":"hi"}`).
    pub fn cmd(&self, json: impl Into<RString>) {
        (self.cmd)(json.into());
    }
    /// Answer a read-view query (JSON in, JSON out, e.g. `{"k":"player"}`).
    pub fn query(&self, json: impl Into<RString>) -> RString {
        (self.query)(json.into())
    }
    /// Record a HUD draw command (JSON, only meaningful during `draw_hud`).
    pub fn hud(&self, json: impl Into<RString>) {
        (self.hud)(json.into());
    }

    // ---- ergonomic helpers (build the JSON for you) ----

    /// Send a chat message / command.
    pub fn send_chat(&self, message: &str) {
        self.cmd(format!(r#"{{"t":"chat","s":"{}"}}"#, esc(message)));
    }
    /// Log at info level through the host logger (tagged with the mod id).
    pub fn log(&self, message: &str) {
        self.cmd(format!(r#"{{"t":"log","l":2,"m":"{}"}}"#, esc(message)));
    }
    pub fn warn(&self, message: &str) {
        self.cmd(format!(r#"{{"t":"log","l":1,"m":"{}"}}"#, esc(message)));
    }
    pub fn error(&self, message: &str) {
        self.cmd(format!(r#"{{"t":"log","l":0,"m":"{}"}}"#, esc(message)));
    }
    /// The local player view as a JSON string (parse with your JSON lib of choice).
    pub fn player_json(&self) -> RString {
        self.query(r#"{"k":"player"}"#)
    }
    /// A block view (`{"id":..,"meta":..}`) as JSON.
    pub fn block_json(&self, x: i32, y: i32, z: i32) -> RString {
        self.query(format!(r#"{{"k":"block","x":{x},"y":{y},"z":{z}}}"#))
    }
    /// Shadowed HUD text at GUI-pixel `(x, y)`. `color` is packed `0xRRGGBBAA`.
    pub fn hud_text(&self, x: i32, y: i32, text: &str, color: u32) {
        self.hud(format!(
            r#"{{"o":"text","x":{x},"y":{y},"s":1,"c":{color},"text":"{}","sh":1}}"#,
            esc(text)
        ));
    }
    /// A filled HUD rect. `color` is packed `0xRRGGBBAA`.
    pub fn hud_rect(&self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        self.hud(format!(
            r#"{{"o":"rect","x":{x},"y":{y},"w":{w},"h":{h},"c":{color}}}"#
        ));
    }
    /// HUD text with an explicit scale and shadow toggle. Added in 0.3.
    pub fn hud_text_ex(&self, x: i32, y: i32, text: &str, color: u32, scale: i32, shadow: bool) {
        self.hud(format!(
            r#"{{"o":"text","x":{x},"y":{y},"s":{scale},"c":{color},"text":"{}","sh":{}}}"#,
            esc(text),
            shadow as u8
        ));
    }
    /// Blit a registered texture (whole image) into the rect. Added in 0.3.
    pub fn hud_image(&self, x: i32, y: i32, w: i32, h: i32, tex: u32) {
        self.hud(format!(
            r#"{{"o":"image","x":{x},"y":{y},"w":{w},"h":{h},"tex":{tex}}}"#
        ));
    }
    /// Blit a sub-rectangle `(sx,sy,sw,sh)` of a registered texture into the
    /// rect (sprite sheets, damage rects). Added in 0.3.
    #[allow(clippy::too_many_arguments)]
    pub fn hud_image_src(
        &self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        tex: u32,
        sx: u32,
        sy: u32,
        sw: u32,
        sh: u32,
    ) {
        self.hud(format!(
            r#"{{"o":"image","x":{x},"y":{y},"w":{w},"h":{h},"tex":{tex},"sx":{sx},"sy":{sy},"sw":{sw},"sh":{sh}}}"#
        ));
    }
    /// A straight HUD line from `(x0,y0)` to `(x1,y1)`, `width` px thick.
    /// `color` is packed `0xRRGGBBAA`. Added in 0.3.
    pub fn hud_line(&self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32, width: i32) {
        self.hud(format!(
            r#"{{"o":"line","x":{x0},"y":{y0},"x2":{x1},"y2":{y1},"c":{color},"w":{width}}}"#
        ));
    }
    /// A vertical gradient rect (`top` color at the top edge → `bottom` at the
    /// bottom), packed `0xRRGGBBAA`. Added in 0.3.
    pub fn hud_gradient(&self, x: i32, y: i32, w: i32, h: i32, top: u32, bottom: u32) {
        self.hud(format!(
            r#"{{"o":"gradient","x":{x},"y":{y},"w":{w},"h":{h},"c":{top},"c2":{bottom}}}"#
        ));
    }

    /// Register an RGBA texture (`width*height*4` bytes) and return its handle.
    pub fn register_texture(&self, rgba: &[u8], width: u32, height: u32) -> u32 {
        (self.register_texture)(RVec::from(rgba.to_vec()), width, height)
    }
    /// Decode and register a PNG/JPEG file, returning its handle (0 on failure).
    pub fn load_texture(&self, path: &str) -> u32 {
        (self.load_texture)(RString::from(path))
    }
    /// Replace a registered texture's pixels (same dimensions as registered).
    pub fn update_texture(&self, handle: u32, rgba: &[u8]) {
        (self.update_texture)(handle, RVec::from(rgba.to_vec()))
    }
    /// Drop a registered texture.
    pub fn free_texture(&self, handle: u32) {
        (self.free_texture)(handle)
    }

    /// Install a full-screen post effect: `wgsl` must define
    /// `fn effect(uv: vec2<f32>, color: vec4<f32>) -> vec4<f32>`. In scope: the
    /// scene `color`, a `U` uniform (`U.resolution: vec2<f32>`, `U.time: f32`),
    /// and `src_tex`/`src_samp` for arbitrary scene sampling. A compile error is
    /// logged host-side and the previous effect kept. Added in 0.3.
    pub fn set_post_effect(&self, wgsl: &str) {
        self.cmd(format!(r#"{{"t":"postEffect","wgsl":"{}"}}"#, esc(wgsl)));
    }
    /// Remove the full-screen post effect. Added in 0.3.
    pub fn clear_post_effect(&self) {
        self.cmd(r#"{"t":"clearPostEffect"}"#);
    }
    /// Statically tint a block id (all metas). `color` is packed `0xRRGGBBAA`.
    pub fn set_block_tint(&self, id: u16, color: u32) {
        self.cmd(format!(
            r#"{{"t":"render","r":"blockTint","id":{id},"meta":-1,"color":{color}}}"#
        ));
    }

    /// Register a content-mod full-cube block at `id` (recraft-authoritative
    /// worlds only). `texture` reuses a vanilla base name; `luminance` is 0..=15.
    pub fn register_block(&self, id: u16, texture: &str, opaque: bool, alpha: f32, luminance: u8) {
        self.cmd(format!(
            r#"{{"t":"block","id":{id},"texture":"{}","opaque":{opaque},"alpha":{alpha},"lum":{luminance}}}"#,
            esc(texture)
        ));
    }

    /// Submit native render-hook geometry (world-space, drawn after entities).
    /// Replaces the previous submission; pass empties to clear. UVs point at the
    /// entity-atlas white texel, so `color` is the surface color. Native-only.
    pub fn submit_geometry(&self, vertices: &[ExtVertex], indices: &[u32]) {
        (self.geometry)(RVec::from(vertices.to_vec()), RVec::from(indices.to_vec()), 0);
    }

    /// Like [`submit_geometry`], but the UVs (normalized 0..1) sample a
    /// registered texture (see [`register_texture`](Self::register_texture) /
    /// [`load_texture`](Self::load_texture)) instead of the entity atlas. Added
    /// in 0.3.
    pub fn submit_geometry_textured(&self, vertices: &[ExtVertex], indices: &[u32], texture: u32) {
        (self.geometry)(
            RVec::from(vertices.to_vec()),
            RVec::from(indices.to_vec()),
            texture,
        );
    }

    // ---- read-views (JSON in, JSON/primitive out; added in 0.2) ----

    /// All remote entities as a JSON array.
    pub fn entities_json(&self) -> RString {
        self.query(r#"{"k":"entities"}"#)
    }
    /// One entity by id as JSON (`null` if unknown).
    pub fn entity_json(&self, id: i32) -> RString {
        self.query(format!(r#"{{"k":"entity","id":{id}}}"#))
    }
    /// The held item as JSON (`null` if empty).
    pub fn held_item_json(&self) -> RString {
        self.query(r#"{"k":"held"}"#)
    }
    /// The 45-slot inventory as a JSON array (`null` per empty slot).
    pub fn inventory_json(&self) -> RString {
        self.query(r#"{"k":"inventory"}"#)
    }
    /// The player's abilities as JSON.
    pub fn capabilities_json(&self) -> RString {
        self.query(r#"{"k":"capabilities"}"#)
    }
    /// Active potion effects as a JSON array.
    pub fn effects_json(&self) -> RString {
        self.query(r#"{"k":"effects"}"#)
    }
    /// XP `{"bar":..,"level":..}` as JSON.
    pub fn xp_json(&self) -> RString {
        self.query(r#"{"k":"xp"}"#)
    }
    /// The open container `{windowId,kind,size}` as JSON (`null` if none).
    pub fn container_json(&self) -> RString {
        self.query(r#"{"k":"container"}"#)
    }
    /// The selected hotbar slot (0..8).
    pub fn selected_slot(&self) -> i32 {
        self.query(r#"{"k":"selectedSlot"}"#)
            .as_str()
            .trim()
            .parse()
            .unwrap_or(0)
    }
    /// World time in ticks.
    pub fn world_time(&self) -> i64 {
        self.query(r#"{"k":"time"}"#)
            .as_str()
            .trim()
            .parse()
            .unwrap_or(0)
    }
    /// Current dimension id.
    pub fn dimension(&self) -> i32 {
        self.query(r#"{"k":"dim"}"#)
            .as_str()
            .trim()
            .parse()
            .unwrap_or(0)
    }
    /// Whether a server connection is active.
    pub fn connected(&self) -> bool {
        self.query(r#"{"k":"connected"}"#).as_str().trim() == "true"
    }

    // ---- world / player actions (added in 0.2) ----

    /// Place the held item against a block face; `cursor` is the in-block hit
    /// point (0..15 each), e.g. `[8, 8, 8]` for the face centre.
    pub fn place_block(&self, x: i32, y: i32, z: i32, face: u8, cursor: [u8; 3]) {
        self.cmd(format!(
            r#"{{"t":"place","x":{x},"y":{y},"z":{z},"face":{face},"cx":{},"cy":{},"cz":{}}}"#,
            cursor[0], cursor[1], cursor[2]
        ));
    }
    /// Raw digging: `status` 0 start, 1 cancel, 2 finish, 3 drop-stack, 4 drop,
    /// 5 release-use.
    pub fn dig(&self, status: u8, x: i32, y: i32, z: i32, face: u8) {
        self.cmd(format!(
            r#"{{"t":"dig","status":{status},"x":{x},"y":{y},"z":{z},"face":{face}}}"#
        ));
    }
    /// Attack an entity by id.
    pub fn attack_entity(&self, id: i32) {
        self.cmd(format!(r#"{{"t":"attack","id":{id}}}"#));
    }
    /// Right-click / interact with an entity (no hit point).
    pub fn interact_entity(&self, id: i32) {
        self.cmd(format!(r#"{{"t":"interact","id":{id}}}"#));
    }
    /// Interact with an entity at a specific hit point (InteractAt).
    pub fn interact_entity_at(&self, id: i32, at: [f32; 3]) {
        self.cmd(format!(
            r#"{{"t":"interact","id":{id},"ax":{},"ay":{},"az":{}}}"#,
            at[0], at[1], at[2]
        ));
    }
    /// Click a slot in the open window (vanilla ClickWindow codes).
    pub fn container_click(&self, slot: i16, button: i8, mode: i8) {
        self.cmd(format!(
            r#"{{"t":"click","slot":{slot},"button":{button},"mode":{mode}}}"#
        ));
    }
    /// Close the open window.
    pub fn container_close(&self) {
        self.cmd(r#"{"t":"close"}"#);
    }
    /// Open the player inventory window.
    pub fn open_inventory(&self) {
        self.cmd(r#"{"t":"openInv"}"#);
    }
    /// Select a hotbar slot (0..8).
    pub fn select_slot(&self, slot: i32) {
        self.cmd(format!(r#"{{"t":"selectSlot","slot":{slot}}}"#));
    }
    /// Swing the arm.
    pub fn swing(&self) {
        self.cmd(r#"{"t":"swing"}"#);
    }
    /// Use the held item with no target.
    pub fn use_item(&self) {
        self.cmd(r#"{"t":"useItem"}"#);
    }
    /// Set the look. `silent` keeps the camera put and only overrides the
    /// server-visible rotation on the next movement packet (pre-event style).
    pub fn set_rotation(&self, yaw: f32, pitch: f32, silent: bool) {
        self.cmd(format!(
            r#"{{"t":"rotate","yaw":{yaw},"pitch":{pitch},"silent":{silent}}}"#
        ));
    }
    /// Clear a silent-look override.
    pub fn clear_rotation(&self) {
        self.cmd(r#"{"t":"clearRotate"}"#);
    }
}

/// Minimal JSON string escaper (no serde dependency in this crate).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The hook surface a native plugin implements. Mirrors the host's internal
/// `HostHooks`; structured arguments are JSON `RStr`s the host owns for the call.
/// Return `true` from `on_clientbound_packet` to drop the packet, and from
/// `on_input` to consume the key.
#[sabi_trait]
pub trait NativePlugin {
    fn id(&self) -> RString;
    fn on_load(&mut self, host: HostApi);
    fn on_clientbound_packet(&mut self, host: HostApi, packet: RStr<'_>) -> bool;
    fn on_event(&mut self, host: HostApi, event: RStr<'_>);
    fn on_tick(&mut self, host: HostApi);
    fn on_frame(&mut self, host: HostApi);
    fn on_input(&mut self, host: HostApi, input: RStr<'_>) -> bool;
    fn draw_hud(&mut self, host: HostApi, ctx: RStr<'_>);
    /// An outbound packet is about to be sent (pre-send hook); return `true` to
    /// drop it. Added in 0.2 with a default body, so older mods are unaffected
    /// and a mod overrides it only if it cares.
    fn on_serverbound_packet(&mut self, host: HostApi, packet: RStr<'_>) -> bool {
        let _ = (host, packet);
        false
    }
}

/// The owned, type-erased plugin object passed across the ABI.
pub type PluginObj = NativePlugin_TO<'static, RBox<()>>;

/// The root module a native mod exports. Prefix type, so fields can be appended
/// in later API versions without breaking older mods.
#[repr(C)]
#[derive(StableAbi)]
#[sabi(kind(Prefix(prefix_ref = ExtApiRef)))]
pub struct ExtApi {
    /// Construct the mod's plugin instance.
    #[sabi(last_prefix_field)]
    pub new_plugin: extern "C" fn() -> PluginObj,
}

impl RootModule for ExtApiRef {
    declare_root_module_statics! {ExtApiRef}
    const BASE_NAME: &'static str = "recraft_ext_mod";
    const NAME: &'static str = "recraft_ext_mod";
    const VERSION_STRINGS: VersionStrings = package_version_strings!();
}
