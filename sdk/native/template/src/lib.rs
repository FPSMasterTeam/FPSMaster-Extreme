//! Minimal fpsmaster native mod. `cargo build --release`, then drop the resulting
//! cdylib + a `mod.toml` into a fpsmaster `mods/<id>/` folder. See ../README.md.

use fpsmaster_ext_api::prelude::*;

#[derive(Default)]
struct MyMod {
    ticks: u64,
}

impl NativePlugin for MyMod {
    fn id(&self) -> RString {
        RString::from("my.native.mod")
    }

    fn on_load(&mut self, host: HostApi) {
        host.log("my native mod loaded");
    }

    // Return `true` to drop the packet.
    fn on_clientbound_packet(&mut self, _host: HostApi, _packet: RStr<'_>) -> bool {
        false
    }

    fn on_event(&mut self, _host: HostApi, _event: RStr<'_>) {}

    fn on_tick(&mut self, host: HostApi) {
        self.ticks += 1;
        if self.ticks == 40 {
            host.send_chat("hello from my native mod");
        }
    }

    fn on_frame(&mut self, _host: HostApi) {}

    // Return `true` to consume the key.
    fn on_input(&mut self, _host: HostApi, _input: RStr<'_>) -> bool {
        false
    }

    fn draw_hud(&mut self, host: HostApi, _ctx: RStr<'_>) {
        host.hud_text(2, 60, "my native mod", 0xffff_ffff);
    }
}

#[export_root_module]
fn get_root_module() -> ExtApiRef {
    ExtApi { new_plugin }.leak_into_prefix()
}

#[sabi_extern_fn]
fn new_plugin() -> PluginObj {
    NativePlugin_TO::from_value(MyMod::default(), TD_Opaque)
}
