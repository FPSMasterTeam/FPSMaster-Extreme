//! [`ExtManager`] — the single source of truth: it owns the loaded plugins, the
//! event-dispatch fan-out, and the command queue. The host (`recraft_app`) holds
//! one `ExtManager`, calls a `dispatch_*` method at each seam, and drains
//! [`ExtManager::take_commands`] each tick.
//!
//! Every plugin is a `Box<dyn HostHooks>` (demo, JS, or native), so dispatch is
//! uniform. Hook calls are wrapped in `catch_unwind` so a panicking mod disables
//! itself instead of taking down the host (this covers Rust-side panics — a JS
//! exception is already a `Result` in the bridge; a native mod has no sandbox).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use crate::command::ExtCommand;
use crate::event::{ExtEvent, Verdict};
use crate::host::{HookCtx, HostHooks};
use crate::hud::{HudCtx, HudDraw};
use crate::input::InputEvent;
use crate::js;
use crate::manifest::{self, Capability, ModManifest, Tier};
use crate::packet::PacketView;
use crate::view::ReadViews;

struct LoadedPlugin {
    id: String,
    enabled: bool,
    loaded: bool,
    /// Loaded from the `mods/` directory (vs registered in-process). Only these
    /// are dropped/recreated on a hot reload.
    from_dir: bool,
    /// Display metadata captured at load (from the manifest for directory mods).
    meta: PluginMeta,
    hooks: Box<dyn HostHooks>,
}

/// A mod's manifest-derived display metadata. In-process registrations have no
/// manifest, so their fields are placeholders.
#[derive(Clone)]
struct PluginMeta {
    name: Option<String>,
    description: Option<String>,
    version: String,
    tier: Option<Tier>,
    capabilities: Vec<Capability>,
}

/// A snapshot of one mod's metadata and state, for the management UI.
#[derive(Debug, Clone)]
pub struct ModInfo {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: String,
    pub tier: Option<Tier>,
    pub capabilities: Vec<Capability>,
    pub enabled: bool,
    /// Loaded from the `mods/` directory (vs an in-process registration).
    pub from_dir: bool,
}

#[derive(Default)]
pub struct ExtManager {
    plugins: Vec<LoadedPlugin>,
    commands: Vec<ExtCommand>,
    js: Option<js::JsRuntime>,
}

impl ExtManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an in-process plugin (e.g. the demo mod). Its `on_load` runs on
    /// the next dispatch that carries read-views. These survive a hot reload.
    pub fn register(&mut self, hooks: Box<dyn HostHooks>) {
        let meta = PluginMeta {
            name: None,
            description: None,
            version: "in-process".to_string(),
            tier: None,
            capabilities: Vec::new(),
        };
        self.push_plugin(hooks, false, meta);
    }

    fn push_plugin(&mut self, hooks: Box<dyn HostHooks>, from_dir: bool, meta: PluginMeta) {
        let id = hooks.id().to_string();
        log::info!("[ext] registered mod '{id}'");
        self.plugins.push(LoadedPlugin {
            id,
            enabled: true,
            loaded: false,
            from_dir,
            meta,
            hooks,
        });
    }

    /// Hot-reload directory mods: drop everything loaded from `mods/` (and the JS
    /// runtime), then reload from disk. In-process registrations are kept.
    pub fn reload_mods(&mut self, dir: &Path) -> Vec<String> {
        self.plugins.retain(|p| !p.from_dir);
        self.js = None; // drops all JS contexts; load_mods recreates the runtime
        log::info!("[ext] hot-reloading mods from {}", dir.display());
        self.load_mods(dir)
    }

    /// Discover and load every mod under `dir` (`<dir>/<id>/mod.toml` + entry),
    /// in dependency order. JS mods are instantiated now; native mods are loaded
    /// by [`ExtManager::load_native`] (wired in Phase 2). Returns the ids loaded.
    pub fn load_mods(&mut self, dir: &Path) -> Vec<String> {
        let discovered = match js::discover_mods(dir) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("[ext] mod discovery in {} failed: {e}", dir.display());
                return Vec::new();
            }
        };
        if discovered.is_empty() {
            return Vec::new();
        }
        let manifests: Vec<ModManifest> = discovered.iter().map(|(m, _)| m.clone()).collect();
        let order = match manifest::load_order(&manifests) {
            Ok(o) => o,
            Err(e) => {
                log::error!("[ext] resolving load order failed: {e}");
                return Vec::new();
            }
        };

        let mut loaded = Vec::new();
        for idx in order {
            let (manifest, path) = &discovered[idx];
            if let Err(e) = manifest.check_api() {
                log::error!("[ext] {e}");
                continue;
            }
            log_capabilities(manifest);
            match manifest.tier {
                Tier::Js => {
                    let entry = path.join(&manifest.entry);
                    let source = match std::fs::read_to_string(&entry) {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!("[ext] reading {} failed: {e}", entry.display());
                            continue;
                        }
                    };
                    let rt = match self.js_runtime() {
                        Some(rt) => rt,
                        None => continue,
                    };
                    match rt.load(&manifest.id, &source, path) {
                        Ok(plugin) => {
                            self.push_plugin(Box::new(plugin), true, plugin_meta(manifest));
                            loaded.push(manifest.id.clone());
                        }
                        Err(e) => log::error!("[ext] loading mod '{}' failed: {e}", manifest.id),
                    }
                }
                Tier::Native => {
                    let entry = path.join(&manifest.entry);
                    match crate::native::NativeAdapter::load(&entry) {
                        Ok(adapter) => {
                            self.push_plugin(Box::new(adapter), true, plugin_meta(manifest));
                            loaded.push(manifest.id.clone());
                        }
                        Err(e) => {
                            log::error!("[ext] loading native mod '{}' failed: {e}", manifest.id)
                        }
                    }
                }
            }
        }
        loaded
    }

    fn js_runtime(&mut self) -> Option<&js::JsRuntime> {
        if self.js.is_none() {
            match js::JsRuntime::new() {
                Ok(rt) => self.js = Some(rt),
                Err(e) => {
                    log::error!("[ext] could not start the JS runtime: {e}");
                    return None;
                }
            }
        }
        self.js.as_ref()
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// A snapshot of every loaded mod (enabled or not) for the management UI.
    pub fn mods(&self) -> Vec<ModInfo> {
        self.plugins
            .iter()
            .map(|p| ModInfo {
                id: p.id.clone(),
                name: p.meta.name.clone(),
                description: p.meta.description.clone(),
                version: p.meta.version.clone(),
                tier: p.meta.tier,
                capabilities: p.meta.capabilities.clone(),
                enabled: p.enabled,
                from_dir: p.from_dir,
            })
            .collect()
    }

    /// Enable or disable a loaded mod by id. Disabling pauses all hook dispatch
    /// for it; enabling resumes (and runs a pending `on_load` if it never ran).
    /// Returns `false` if no mod has that id.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        for p in self.plugins.iter_mut() {
            if p.id == id {
                p.enabled = enabled;
                return true;
            }
        }
        false
    }

    pub fn enabled_ids(&self) -> Vec<&str> {
        self.plugins
            .iter()
            .filter(|p| p.enabled)
            .map(|p| p.id.as_str())
            .collect()
    }

    /// Drain commands accumulated since the last call. The host applies these on
    /// the main thread.
    pub fn take_commands(&mut self) -> Vec<ExtCommand> {
        std::mem::take(&mut self.commands)
    }

    /// Run `on_load` for any plugin that hasn't been loaded yet.
    fn run_pending_loads(&mut self, views: &dyn ReadViews) {
        let Self {
            plugins, commands, ..
        } = self;
        for p in plugins.iter_mut() {
            if p.loaded || !p.enabled {
                continue;
            }
            p.loaded = true;
            let LoadedPlugin { id, hooks, .. } = p;
            let mut ctx = HookCtx::new(views, commands, id);
            if catch_unwind(AssertUnwindSafe(|| hooks.on_load(&mut ctx))).is_err() {
                p.enabled = false;
                log::error!("[ext] mod '{}' panicked in on_load — disabled", p.id);
            }
        }
    }

    pub fn dispatch_clientbound_packet(
        &mut self,
        packet: &PacketView,
        views: &dyn ReadViews,
    ) -> Verdict {
        self.run_pending_loads(views);
        let Self {
            plugins, commands, ..
        } = self;
        let mut verdict = Verdict::Pass;
        for p in plugins.iter_mut().filter(|p| p.enabled) {
            let LoadedPlugin { id, hooks, .. } = p;
            let mut ctx = HookCtx::new(views, commands, id);
            match catch_unwind(AssertUnwindSafe(|| {
                hooks.on_clientbound_packet(packet, &mut ctx)
            })) {
                Ok(v) => verdict = verdict.merge(v),
                Err(_) => disable(&mut p.enabled, &p.id, "on_clientbound_packet"),
            }
        }
        verdict
    }

    pub fn dispatch_serverbound_packet(
        &mut self,
        packet: &PacketView,
        views: &dyn ReadViews,
    ) -> Verdict {
        let Self {
            plugins, commands, ..
        } = self;
        let mut verdict = Verdict::Pass;
        for p in plugins.iter_mut().filter(|p| p.enabled) {
            let LoadedPlugin { id, hooks, .. } = p;
            let mut ctx = HookCtx::new(views, commands, id);
            match catch_unwind(AssertUnwindSafe(|| {
                hooks.on_serverbound_packet(packet, &mut ctx)
            })) {
                Ok(v) => verdict = verdict.merge(v),
                Err(_) => disable(&mut p.enabled, &p.id, "on_serverbound_packet"),
            }
        }
        verdict
    }

    pub fn dispatch_event(&mut self, event: &ExtEvent, views: &dyn ReadViews) {
        let Self {
            plugins, commands, ..
        } = self;
        for p in plugins.iter_mut().filter(|p| p.enabled) {
            let LoadedPlugin { id, hooks, .. } = p;
            let mut ctx = HookCtx::new(views, commands, id);
            if catch_unwind(AssertUnwindSafe(|| hooks.on_event(event, &mut ctx))).is_err() {
                disable(&mut p.enabled, &p.id, "on_event");
            }
        }
    }

    pub fn dispatch_tick(&mut self, views: &dyn ReadViews) {
        self.run_pending_loads(views);
        let Self {
            plugins, commands, ..
        } = self;
        for p in plugins.iter_mut().filter(|p| p.enabled) {
            let LoadedPlugin { id, hooks, .. } = p;
            let mut ctx = HookCtx::new(views, commands, id);
            if catch_unwind(AssertUnwindSafe(|| hooks.on_tick(&mut ctx))).is_err() {
                disable(&mut p.enabled, &p.id, "on_tick");
            }
        }
    }

    pub fn dispatch_frame(&mut self, views: &dyn ReadViews) {
        let Self {
            plugins, commands, ..
        } = self;
        for p in plugins.iter_mut().filter(|p| p.enabled) {
            let LoadedPlugin { id, hooks, .. } = p;
            let mut ctx = HookCtx::new(views, commands, id);
            if catch_unwind(AssertUnwindSafe(|| hooks.on_frame(&mut ctx))).is_err() {
                disable(&mut p.enabled, &p.id, "on_frame");
            }
        }
    }

    /// Returns `true` if any mod consumed the input.
    pub fn dispatch_input(&mut self, input: &InputEvent, views: &dyn ReadViews) -> bool {
        let Self {
            plugins, commands, ..
        } = self;
        let mut consumed = false;
        for p in plugins.iter_mut().filter(|p| p.enabled) {
            let LoadedPlugin { id, hooks, .. } = p;
            let mut ctx = HookCtx::new(views, commands, id);
            match catch_unwind(AssertUnwindSafe(|| hooks.on_input(input, &mut ctx))) {
                Ok(true) => consumed = true,
                Ok(false) => {}
                Err(_) => disable(&mut p.enabled, &p.id, "on_input"),
            }
        }
        consumed
    }

    pub fn draw_hud(&mut self, hud: &mut HudDraw, ctx: &HudCtx, views: &dyn ReadViews) {
        for p in self.plugins.iter_mut().filter(|p| p.enabled) {
            let hooks = &mut p.hooks;
            if catch_unwind(AssertUnwindSafe(|| hooks.draw_hud(hud, ctx, views))).is_err() {
                disable(&mut p.enabled, &p.id, "draw_hud");
            }
        }
    }
}

fn plugin_meta(manifest: &ModManifest) -> PluginMeta {
    PluginMeta {
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        version: manifest.version.clone(),
        tier: Some(manifest.tier),
        capabilities: manifest.capabilities.clone(),
    }
}

fn disable(enabled: &mut bool, id: &str, hook: &str) {
    *enabled = false;
    log::error!("[ext] mod '{id}' panicked in {hook} — disabled");
}

/// Log a mod's declared capabilities at load (the "declare + confirm" trust
/// model — there is no sandbox, so this is informational). Sensitive ones are
/// flagged so a host front-end could gate them.
fn log_capabilities(manifest: &ModManifest) {
    if manifest.capabilities.is_empty() {
        return;
    }
    let sensitive: Vec<_> = manifest
        .capabilities
        .iter()
        .filter(|c| c.is_sensitive())
        .collect();
    log::info!(
        "[ext] mod '{}' declares capabilities {:?}{}",
        manifest.id,
        manifest.capabilities,
        if sensitive.is_empty() {
            String::new()
        } else {
            format!(" (sensitive: {sensitive:?})")
        }
    );
}
