//! recraft extension host.
//!
//! This crate is the single source of truth for the extension system: an event
//! bus + command queue ([`ExtManager`]) over a uniform plugin interface
//! ([`HostHooks`]), plus the stable projection types ([`PacketView`],
//! [`PlayerView`], [`ExtCommand`], …) and `mod.toml` handling.
//!
//! It is intentionally decoupled from the rest of recraft (no core/protocol/
//! render/app dependency). The host (`recraft_app`) performs every projection
//! into and out of these plain types. That decoupling is what makes the same
//! vocabulary usable by the JS bridge and — once mirrored in `recraft_ext_api` —
//! across the native ABI boundary.

pub(crate) mod bridge;
pub mod command;
pub mod dev;
pub mod event;
pub mod host;
pub mod hud;
pub mod input;
pub mod js;
pub mod manager;
pub mod manifest;
pub mod native;
pub mod packet;
pub mod view;

pub use command::{EntityFilter, ExtCommand, LogLevel, RenderPreset};
pub use event::{ExtEvent, Verdict};
pub use host::{HookCtx, HostHooks};
pub use hud::{HudCmd, HudCtx, HudDraw, TexHandle};
pub use input::InputEvent;
pub use manager::ExtManager;
pub use manifest::{
    load_order, Capability, ManifestError, ModManifest, Tier, JS_API_VERSION, NATIVE_API_VERSION,
};
pub use packet::{PacketBuild, PacketType, PacketView};
pub use view::{BlockView, EntityKindView, EntityView, PlayerView, ReadViews};

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed read-views for unit tests.
    struct MockViews;
    impl ReadViews for MockViews {
        fn player(&self) -> PlayerView {
            PlayerView {
                x: 10.0,
                y: 64.0,
                z: -20.0,
                yaw: 90.0,
                pitch: 0.0,
                vx: 0.0,
                vy: 0.0,
                vz: 0.0,
                on_ground: true,
                health: 20.0,
                food: 20,
                sneaking: false,
                sprinting: false,
            }
        }
        fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockView {
            BlockView { id: 1, meta: 0 }
        }
        fn entities(&self) -> Vec<EntityView> {
            Vec::new()
        }
        fn entity(&self, _id: i32) -> Option<EntityView> {
            None
        }
        fn world_time(&self) -> i64 {
            6000
        }
        fn dimension(&self) -> i32 {
            0
        }
        fn loaded_chunk_count(&self) -> usize {
            9
        }
    }

    #[test]
    fn manifest_parses_and_checks_api() {
        let src = r#"
            id = "coords_hud"
            version = "1.0.0"
            tier = "js"
            api = "^0.1"
            entry = "main.js"
            capabilities = ["hud", "read_player"]
        "#;
        let m = ModManifest::parse(src).unwrap();
        assert_eq!(m.id, "coords_hud");
        assert_eq!(m.tier, Tier::Js);
        assert!(m.capabilities.contains(&Capability::Hud));
        assert!(m.api_compatible());
    }

    #[test]
    fn api_requirement_semantics() {
        use manifest::api_requirement_satisfied;
        assert!(api_requirement_satisfied("^0.1", (0, 1, 0)));
        assert!(api_requirement_satisfied("^0.1.0", (0, 1, 5)));
        assert!(!api_requirement_satisfied("^0.2", (0, 1, 0)));
        assert!(!api_requirement_satisfied("^1.0", (0, 1, 0)));
        assert!(api_requirement_satisfied("^1.2", (1, 4, 0)));
        assert!(!api_requirement_satisfied("^1.2", (1, 1, 0)));
    }

    #[test]
    fn load_order_respects_dependencies() {
        let mk = |id: &str, deps: &[&str]| ModManifest {
            id: id.to_string(),
            version: "1.0.0".into(),
            tier: Tier::Js,
            api: "^0.1".into(),
            entry: "main.js".into(),
            depends: deps.iter().map(|s| s.to_string()).collect(),
            capabilities: vec![],
            name: None,
            description: None,
        };
        let mods = vec![mk("a", &["b"]), mk("b", &["c"]), mk("c", &[])];
        let order = load_order(&mods).unwrap();
        let pos = |id: &str| order.iter().position(|&i| mods[i].id == id).unwrap();
        assert!(pos("c") < pos("b"));
        assert!(pos("b") < pos("a"));
    }

    #[test]
    fn load_order_detects_cycle() {
        let mk = |id: &str, deps: &[&str]| ModManifest {
            id: id.to_string(),
            version: "1.0.0".into(),
            tier: Tier::Js,
            api: "^0.1".into(),
            entry: "main.js".into(),
            depends: deps.iter().map(|s| s.to_string()).collect(),
            capabilities: vec![],
            name: None,
            description: None,
        };
        let mods = vec![mk("a", &["b"]), mk("b", &["a"])];
        assert!(matches!(
            load_order(&mods),
            Err(ManifestError::DependencyCycle(_))
        ));
    }

    #[test]
    fn demo_mod_exercises_command_and_packet_seams() {
        let mut mgr = ExtManager::new();
        mgr.register(Box::new(dev::DemoMod::new()));
        let views = MockViews;

        // Tick enough to trip the one-shot greeting chat.
        for _ in 0..40 {
            mgr.dispatch_tick(&views);
        }
        let cmds = mgr.take_commands();
        let chat = cmds.iter().any(|c| matches!(c, ExtCommand::Chat(s) if s.contains("demo online")));
        assert!(chat, "demo mod should enqueue a Chat command after 40 ticks");

        // A clientbound chat packet should produce a Log command and Pass.
        let verdict = mgr.dispatch_clientbound_packet(
            &PacketView::Chat {
                text: "hello".into(),
                position: 0,
                json: "{}".into(),
            },
            &views,
        );
        assert_eq!(verdict, Verdict::Pass);
        let cmds = mgr.take_commands();
        assert!(cmds
            .iter()
            .any(|c| matches!(c, ExtCommand::Log(_, s) if s.contains("saw chat packet"))));
    }

    #[test]
    fn demo_mod_consumes_custom_keybind() {
        let mut mgr = ExtManager::new();
        mgr.register(Box::new(dev::DemoMod::new()));
        let views = MockViews;
        assert!(mgr.dispatch_input(&InputEvent::new("F6", true), &views));
        assert!(!mgr.dispatch_input(&InputEvent::new("KeyW", true), &views));
    }

    #[test]
    fn demo_mod_draws_hud_from_views() {
        let mut mgr = ExtManager::new();
        mgr.register(Box::new(dev::DemoMod::new()));
        let views = MockViews;
        let mut hud = HudDraw::new();
        let ctx = HudCtx {
            width: 320,
            height: 240,
            scale: 2,
            screen_open: false,
        };
        mgr.draw_hud(&mut hud, &ctx, &views);
        assert!(matches!(hud.commands().first(), Some(HudCmd::Text { .. })));
    }

    fn js_mod(source: &str) -> ExtManager {
        let mut mgr = ExtManager::new();
        let rt = js::JsRuntime::new().expect("js runtime");
        let plugin = rt.load("test.mod", source).expect("load js mod");
        mgr.register(Box::new(plugin));
        // The plugin's context keeps the runtime alive, so dropping `rt` is fine.
        mgr
    }

    #[test]
    fn js_tick_reads_views_and_enqueues_chat() {
        let mut mgr = js_mod("recraft.onTick(() => recraft.sendChat('x=' + recraft.player().x));");
        mgr.dispatch_tick(&MockViews);
        let cmds = mgr.take_commands();
        assert!(
            cmds.iter()
                .any(|c| matches!(c, ExtCommand::Chat(s) if s == "x=10")),
            "expected Chat('x=10'), got {cmds:?}"
        );
    }

    #[test]
    fn js_block_query_reaches_views() {
        let mut mgr = js_mod(
            "recraft.onTick(() => { const b = recraft.blockAt(1,2,3); recraft.log('id', b.id); });",
        );
        mgr.dispatch_tick(&MockViews);
        let cmds = mgr.take_commands();
        assert!(cmds
            .iter()
            .any(|c| matches!(c, ExtCommand::Log(_, s) if s.contains("id 1"))));
    }

    #[test]
    fn js_packet_handler_can_drop() {
        let mut mgr = js_mod("recraft.onPacket('ChatMessage', (p) => false);");
        let verdict = mgr.dispatch_clientbound_packet(
            &PacketView::Chat {
                text: "hi".into(),
                position: 0,
                json: "{}".into(),
            },
            &MockViews,
        );
        assert_eq!(verdict, Verdict::Drop);
    }

    #[test]
    fn js_input_handler_consumes() {
        let mut mgr = js_mod("recraft.onKey((e) => e.key === 'F6' && e.pressed);");
        assert!(mgr.dispatch_input(&InputEvent::new("F6", true), &MockViews));
        assert!(!mgr.dispatch_input(&InputEvent::new("KeyW", true), &MockViews));
    }

    #[test]
    fn js_draw_hud_records_commands() {
        let mut mgr = js_mod("recraft.drawHud((ctx) => hud.text(2, 2, 'p=' + recraft.player().z));");
        let mut hud = HudDraw::new();
        let ctx = HudCtx {
            width: 320,
            height: 240,
            scale: 2,
            screen_open: false,
        };
        mgr.draw_hud(&mut hud, &ctx, &MockViews);
        assert!(matches!(
            hud.commands().first(),
            Some(HudCmd::Text { text, .. }) if text == "p=-20"
        ));
    }

    #[test]
    fn js_handler_exception_is_isolated() {
        // First handler throws; second still runs and enqueues its command.
        let mut mgr = js_mod(
            "recraft.onTick(() => { throw new Error('boom'); });\
             recraft.onTick(() => recraft.sendChat('survived'));",
        );
        mgr.dispatch_tick(&MockViews);
        let cmds = mgr.take_commands();
        assert!(cmds
            .iter()
            .any(|c| matches!(c, ExtCommand::Chat(s) if s == "survived")));
    }

    #[test]
    fn js_register_block_enqueues_command() {
        let mut mgr = js_mod(
            "recraft.onLoad(() => recraft.registerBlock(300, { texture: 'stone', luminance: 7, tint: [1,0,0] }));",
        );
        mgr.dispatch_tick(&MockViews); // runs the pending on_load
        let cmds = mgr.take_commands();
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                ExtCommand::RegisterBlock { id: 300, luminance: 7, texture, .. } if texture == "stone"
            )),
            "registerBlock should enqueue a RegisterBlock command: {cmds:?}"
        );
    }

    #[test]
    fn js_syntax_error_rejected_at_load() {
        let rt = js::JsRuntime::new().unwrap();
        assert!(rt.load("bad", "this is ) not valid javascript (").is_err());
    }

    #[test]
    fn native_mod_loads_and_runs() {
        // Loads the example cdylib (built separately). Skips if not present so a
        // bare `cargo test -p recraft_ext` doesn't fail; run after
        // `cargo build -p recraft_native_example`.
        let (prefix, ext) = if cfg!(target_os = "windows") {
            ("", "dll")
        } else if cfg!(target_os = "macos") {
            ("lib", "dylib")
        } else {
            ("lib", "so")
        };
        let name = format!("{prefix}recraft_native_example.{ext}");
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        // Prefer debug (what `cargo test --workspace` rebuilds, so it matches the
        // current ABI layout); fall back to release. A stale cdylib built against
        // an older recraft_ext_api layout would be rejected by abi_stable — that
        // is the version safety net working, but rebuild the example to test.
        let lib = [base.join("debug").join(&name), base.join("release").join(&name)]
            .into_iter()
            .find(|p| p.exists());
        let Some(lib) = lib else {
            eprintln!("skipping native_mod_loads_and_runs: build recraft_native_example first");
            return;
        };
        let adapter = native::NativeAdapter::load(&lib).expect("load native mod");
        assert_eq!(adapter.id(), "recraft.native.example");
        let mut mgr = ExtManager::new();
        mgr.register(Box::new(adapter));
        for _ in 0..40 {
            mgr.dispatch_tick(&MockViews);
        }
        let cmds = mgr.take_commands();
        assert!(
            cmds.iter()
                .any(|c| matches!(c, ExtCommand::Chat(s) if s == "native mod online")),
            "native mod should chat after 40 ticks: {cmds:?}"
        );
        // on_load submitted native render-hook geometry (a triangle).
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                ExtCommand::SubmitGeometry { vertices, indices }
                    if vertices.len() == 3 && indices.len() == 3
            )),
            "native mod should submit render geometry on load"
        );
    }

    #[test]
    fn native_loader_rejects_invalid_library() {
        // abi_stable's load_from_file rejects a missing/invalid/mismatched lib —
        // the same path that rejects an api/layout-version mismatch.
        assert!(native::NativeAdapter::load(std::path::Path::new("/nonexistent/none.dylib")).is_err());
    }

    #[test]
    fn shipped_example_mods_load() {
        // The repo's example mods must parse + eval cleanly.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods");
        if !root.is_dir() {
            return; // not in the repo layout (e.g. packaged crate) — skip
        }
        let mut mgr = ExtManager::new();
        let loaded = mgr.load_mods(&root);
        for id in ["coords_hud", "chat_alert", "block_tint"] {
            assert!(loaded.contains(&id.to_string()), "example mod '{id}' failed to load");
        }
        // Exercise their hooks once to ensure the dispatchers run without error.
        mgr.dispatch_tick(&MockViews);
        let mut hud = HudDraw::new();
        mgr.draw_hud(
            &mut hud,
            &HudCtx {
                width: 320,
                height: 240,
                scale: 2,
                screen_open: false,
            },
            &MockViews,
        );
        assert!(!hud.commands().is_empty(), "coords_hud should draw something");
    }

    #[test]
    fn load_mods_from_dir_in_dependency_order() {
        // Build a temp mods/ tree with two mods (b depends on a) and load them.
        let dir = std::env::temp_dir().join(format!("recraft_ext_modtest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mk = |id: &str, depends: &str, body: &str| {
            let md = dir.join(id);
            std::fs::create_dir_all(&md).unwrap();
            std::fs::write(
                md.join("mod.toml"),
                format!(
                    "id = \"{id}\"\nversion = \"1.0.0\"\ntier = \"js\"\napi = \"^0.1\"\nentry = \"main.js\"\ndepends = [{depends}]\n"
                ),
            )
            .unwrap();
            std::fs::write(md.join("main.js"), body).unwrap();
        };
        mk("alpha", "", "recraft.onTick(() => recraft.sendChat('alpha'));");
        mk(
            "beta",
            "\"alpha\"",
            "recraft.onTick(() => recraft.sendChat('beta'));",
        );

        let mut mgr = ExtManager::new();
        let loaded = mgr.load_mods(&dir);
        assert_eq!(loaded.len(), 2, "both mods load");
        assert!(
            loaded.iter().position(|i| i == "alpha")
                < loaded.iter().position(|i| i == "beta"),
            "dependency 'alpha' must load before 'beta': {loaded:?}"
        );

        mgr.dispatch_tick(&MockViews);
        let chats: Vec<_> = mgr
            .take_commands()
            .into_iter()
            .filter_map(|c| match c {
                ExtCommand::Chat(s) => Some(s),
                _ => None,
            })
            .collect();
        assert!(chats.contains(&"alpha".to_string()));
        assert!(chats.contains(&"beta".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
