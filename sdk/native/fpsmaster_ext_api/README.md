# fpsmaster_ext_api

The stable plugin ABI for **fpsmaster** native (`cdylib`) extensions.

A native fpsmaster mod compiles against this crate (plus `abi_stable`) and exports a
root module the host loads with an `abi_stable` layout + version check. This crate
depends only on `abi_stable` — never on fpsmaster internals — so the host can
refactor and obfuscate everything behind it while native mods keep loading.

## Usage

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
fpsmaster_ext_api = "0.2"
abi_stable = "0.11"   # the macros expand to `::abi_stable` paths
```

```rust
use fpsmaster_ext_api::prelude::*;

#[derive(Default)]
struct MyMod;

impl NativePlugin for MyMod {
    fn id(&self) -> RString { RString::from("my.native.mod") }
    fn on_load(&mut self, host: HostApi) { host.log("loaded"); }
    fn on_clientbound_packet(&mut self, _h: HostApi, _p: RStr<'_>) -> bool { false }
    fn on_event(&mut self, _h: HostApi, _e: RStr<'_>) {}
    fn on_tick(&mut self, _h: HostApi) {}
    fn on_frame(&mut self, _h: HostApi) {}
    fn on_input(&mut self, _h: HostApi, _i: RStr<'_>) -> bool { false }
    fn draw_hud(&mut self, host: HostApi, _ctx: RStr<'_>) {
        host.hud_text(2, 60, "my native mod", 0xffff_ffff);
    }
}

#[export_root_module]
fn get_root_module() -> ExtApiRef { ExtApi { new_plugin }.leak_into_prefix() }

#[sabi_extern_fn]
fn new_plugin() -> PluginObj { NativePlugin_TO::from_value(MyMod::default(), TD_Opaque) }
```

The host talks to a mod through `HostApi` (the `cmd` / `query` / `hud` / `geometry`
function pointers) using a small JSON protocol — the same vocabulary as the fpsmaster
JS extension layer.

The full SDK (JS + native, the JSON schema, typings, worked examples, a starter
template) lives in the fpsmaster repo's `sdk/` folder.

## License

MIT OR Apache-2.0
