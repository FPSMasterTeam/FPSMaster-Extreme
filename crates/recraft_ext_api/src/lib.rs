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
    std_types::{RBox, RStr, RString},
    StableAbi,
};

/// The native API semver, mirrored from the manifest `api` requirement. The
/// `abi_stable` layout check is the second, stronger safety net.
pub const API_VERSION: (u32, u32, u32) = (0, 1, 0);

/// Everything a native mod needs in scope to declare its plugin and root module.
/// `use recraft_ext_api::prelude::*;` and implement [`NativePlugin`].
pub mod prelude {
    pub use crate::{ExtApi, ExtApiRef, HostApi, NativePlugin, NativePlugin_TO, PluginObj};
    pub use abi_stable::{
        export_root_module,
        prefix_type::PrefixTypeTrait,
        sabi_extern_fn,
        sabi_trait::TD_Opaque,
        std_types::{RBox, RStr, RString},
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
    /// Statically tint a block id (all metas). `color` is packed `0xRRGGBBAA`.
    pub fn set_block_tint(&self, id: u16, color: u32) {
        self.cmd(format!(
            r#"{{"t":"render","r":"blockTint","id":{id},"meta":-1,"color":{color}}}"#
        ));
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
