//! Native extension backend (`abi_stable` + `cdylib`).
//!
//! A native mod exports a `recraft_ext_api::ExtApi` root module. The loader
//! resolves it with `abi_stable`'s `load_from_file`, which runs a full
//! type-layout and version check and rejects a mismatch — the safety net behind
//! the manifest `api` semver. The plugin object is then wrapped in a
//! [`NativeAdapter`] that drives it through the same JSON [`crate::bridge`]
//! protocol the JS backend uses; the only difference is the transport
//! (`extern "C"` function pointers instead of QuickJS globals).

use std::path::Path;

use abi_stable::{
    library::RootModule,
    std_types::{RStr, RString},
};
use recraft_ext_api::{ExtApiRef, HostApi, PluginObj};

use crate::bridge::{self, cur};
use crate::event::Verdict;
use crate::host::{HookCtx, HostHooks};
use crate::hud::{HudCtx, HudDraw};
use crate::input::InputEvent;
use crate::packet::PacketView;
use crate::view::ReadViews;

// Host entry points handed to a native plugin. They read the same thread-local
// dispatch context the adapter sets around each hook call.
extern "C" fn host_cmd(json: RString) {
    bridge::handle_cmd(json.as_str());
}
extern "C" fn host_query(json: RString) -> RString {
    RString::from(bridge::handle_query(json.as_str()))
}
extern "C" fn host_hud(json: RString) {
    bridge::handle_hud(json.as_str());
}

fn host_api() -> HostApi {
    HostApi {
        cmd: host_cmd,
        query: host_query,
        hud: host_hud,
    }
}

/// A loaded native mod: the type-erased plugin object plus a `HostHooks` shim
/// that bridges the host's dispatch to the plugin's `abi_stable` methods.
pub struct NativeAdapter {
    id: String,
    plugin: PluginObj,
}

impl NativeAdapter {
    /// Load and instantiate a native mod from a dynamic library path. The
    /// `abi_stable` layout + version check happens inside `load_from_file`.
    pub fn load(path: &Path) -> Result<NativeAdapter, NativeError> {
        let root = ExtApiRef::load_from_file(path).map_err(|e| NativeError::Load(e.to_string()))?;
        let plugin = root.new_plugin()();
        let id = plugin.id().as_str().to_string();
        Ok(NativeAdapter { id, plugin })
    }
}

impl HostHooks for NativeAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn on_load(&mut self, ctx: &mut HookCtx) {
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.plugin.on_load(host_api());
    }

    fn on_clientbound_packet(&mut self, packet: &PacketView, ctx: &mut HookCtx) -> Verdict {
        let json = bridge::packet_view_json(packet);
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        if self
            .plugin
            .on_clientbound_packet(host_api(), RStr::from(json.as_str()))
        {
            Verdict::Drop
        } else {
            Verdict::Pass
        }
    }

    fn on_event(&mut self, event: &crate::event::ExtEvent, ctx: &mut HookCtx) {
        let json = bridge::event_json(event);
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.plugin.on_event(host_api(), RStr::from(json.as_str()));
    }

    fn on_tick(&mut self, ctx: &mut HookCtx) {
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.plugin.on_tick(host_api());
    }

    fn on_frame(&mut self, ctx: &mut HookCtx) {
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.plugin.on_frame(host_api());
    }

    fn on_input(&mut self, input: &InputEvent, ctx: &mut HookCtx) -> bool {
        let json = bridge::input_json(input);
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.plugin.on_input(host_api(), RStr::from(json.as_str()))
    }

    fn draw_hud(&mut self, hud: &mut HudDraw, ctx: &HudCtx, views: &dyn ReadViews) {
        let json = bridge::hud_ctx_json(ctx);
        let _v = cur::ViewsGuard::enter(views);
        let _h = cur::HudGuard::enter(hud);
        self.plugin.draw_hud(host_api(), RStr::from(json.as_str()));
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    #[error("{0}")]
    Load(String),
}
