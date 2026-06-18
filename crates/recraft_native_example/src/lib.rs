//! Example native (`cdylib`) recraft mod.
//!
//! Native mods get the full hook surface (same JSON command/query/HUD protocol
//! as JS) plus, unlike JS, unrestricted native code — direct CPU work, their own
//! threads/state, and (Phase 3) raw render hooks. Build with `cargo build` to
//! produce `librecraft_native_example.{dylib,so,dll}`, drop it next to a
//! `mod.toml` in `mods/`, and the host loads it after an `abi_stable` layout +
//! version check.

use recraft_ext_api::prelude::*;

#[derive(Default)]
struct ExampleMod {
    ticks: u64,
    greeted: bool,
}

impl NativePlugin for ExampleMod {
    fn id(&self) -> RString {
        RString::from("recraft.native.example")
    }

    fn on_load(&mut self, host: HostApi) {
        host.log("native example mod loaded");
    }

    fn on_clientbound_packet(&mut self, _host: HostApi, _packet: RStr<'_>) -> bool {
        false // Pass
    }

    fn on_event(&mut self, _host: HostApi, _event: RStr<'_>) {}

    fn on_tick(&mut self, host: HostApi) {
        self.ticks += 1;
        if !self.greeted && self.ticks >= 40 {
            self.greeted = true;
            host.send_chat("native mod online");
        }
    }

    fn on_frame(&mut self, _host: HostApi) {}

    fn on_input(&mut self, _host: HostApi, _input: RStr<'_>) -> bool {
        false
    }

    fn draw_hud(&mut self, host: HostApi, _ctx: RStr<'_>) {
        // Reads the live player view, then draws a label (white, gui-pixel coords).
        let _player = host.player_json();
        host.hud_text(2, 48, "native mod hud", 0xffff_ffff);
    }
}

#[export_root_module]
fn get_root_module() -> ExtApiRef {
    ExtApi { new_plugin }.leak_into_prefix()
}

#[sabi_extern_fn]
fn new_plugin() -> PluginObj {
    NativePlugin_TO::from_value(ExampleMod::default(), TD_Opaque)
}
