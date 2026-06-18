//! The internal plugin prototype ([`HostHooks`]) and the per-call context
//! ([`HookCtx`]) every hook receives.
//!
//! `HostHooks` is the single interface the host dispatches to. Every extension —
//! the in-tree demo mod, each JS mod (via the rquickjs bridge), and each native
//! mod (via the abi_stable adapter) — is a `Box<dyn HostHooks>`. The native
//! `NativePlugin` trait in `recraft_ext_api` is the stable, ABI-safe mirror of
//! this; the loader adapts it into a `HostHooks`.

use crate::command::{EntityFilter, ExtCommand, LogLevel, RenderPreset};
use crate::event::{ExtEvent, Verdict};
use crate::hud::{HudCtx, HudDraw};
use crate::input::InputEvent;
use crate::packet::{PacketBuild, PacketView};
use crate::view::{BlockView, EntityView, PlayerView, ReadViews};

/// Context handed to every hook: live read-views in, a command sink out.
pub struct HookCtx<'a> {
    views: &'a dyn ReadViews,
    commands: &'a mut Vec<ExtCommand>,
    mod_id: &'a str,
}

impl<'a> HookCtx<'a> {
    pub(crate) fn new(
        views: &'a dyn ReadViews,
        commands: &'a mut Vec<ExtCommand>,
        mod_id: &'a str,
    ) -> Self {
        Self {
            views,
            commands,
            mod_id,
        }
    }

    pub fn mod_id(&self) -> &str {
        self.mod_id
    }

    /// The live read-views and command sink behind this context, for bridges
    /// (e.g. the JS runtime) that expose them through a thread-local during a
    /// synchronous dispatch.
    pub(crate) fn raw_parts(&mut self) -> (&dyn ReadViews, &mut Vec<ExtCommand>) {
        (self.views, &mut *self.commands)
    }

    // ---- read-views ----
    pub fn player(&self) -> PlayerView {
        self.views.player()
    }
    pub fn block_at(&self, x: i32, y: i32, z: i32) -> BlockView {
        self.views.block_at(x, y, z)
    }
    pub fn entities(&self) -> Vec<EntityView> {
        self.views.entities()
    }
    pub fn entity(&self, id: i32) -> Option<EntityView> {
        self.views.entity(id)
    }
    pub fn world_time(&self) -> i64 {
        self.views.world_time()
    }
    pub fn dimension(&self) -> i32 {
        self.views.dimension()
    }
    pub fn loaded_chunk_count(&self) -> usize {
        self.views.loaded_chunk_count()
    }

    // ---- commands ----
    pub fn push(&mut self, cmd: ExtCommand) {
        self.commands.push(cmd);
    }
    pub fn send_chat(&mut self, message: impl Into<String>) {
        self.commands.push(ExtCommand::Chat(message.into()));
    }
    pub fn send_packet(&mut self, packet: PacketBuild) {
        self.commands.push(ExtCommand::SendServerbound(packet));
    }
    pub fn log(&mut self, level: LogLevel, msg: impl Into<String>) {
        self.commands.push(ExtCommand::Log(level, msg.into()));
    }
    pub fn info(&mut self, msg: impl Into<String>) {
        self.log(LogLevel::Info, msg);
    }
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_particle(
        &mut self,
        kind: i32,
        x: f64,
        y: f64,
        z: f64,
        ox: f32,
        oy: f32,
        oz: f32,
        speed: f32,
        count: i32,
    ) {
        self.commands.push(ExtCommand::SpawnParticle {
            kind,
            x,
            y,
            z,
            ox,
            oy,
            oz,
            speed,
            count,
        });
    }
    pub fn play_sound(&mut self, event: impl Into<String>, x: f64, y: f64, z: f64) {
        self.commands.push(ExtCommand::PlaySound {
            event: event.into(),
            x,
            y,
            z,
            volume: 1.0,
            pitch: 1.0,
        });
    }
    pub fn render(&mut self, preset: RenderPreset) {
        self.commands.push(ExtCommand::Render(preset));
    }
    pub fn set_block_tint(&mut self, block_id: u16, meta: Option<u8>, color: [f32; 3]) {
        self.render(RenderPreset::BlockTint {
            block_id,
            meta,
            color,
        });
    }
    pub fn entity_box(&mut self, kind: Option<EntityFilter>, color: [f32; 3], enabled: bool) {
        self.render(RenderPreset::EntityBox {
            kind,
            color,
            enabled,
        });
    }
}

/// The hook surface a host dispatches to. Default impls make every hook
/// optional, so a mod overrides only what it needs.
pub trait HostHooks {
    /// Stable id of this mod (matches its `mod.toml` `id`).
    fn id(&self) -> &str;

    /// Called once after the mod is loaded and registered.
    fn on_load(&mut self, _ctx: &mut HookCtx) {}

    /// A clientbound packet arrived (main thread, before the host applies it).
    fn on_clientbound_packet(&mut self, _packet: &PacketView, _ctx: &mut HookCtx) -> Verdict {
        Verdict::Pass
    }

    /// An outbound packet is about to be sent.
    fn on_serverbound_packet(&mut self, _packet: &PacketView, _ctx: &mut HookCtx) -> Verdict {
        Verdict::Pass
    }

    /// A derived, already-projected game-state notification.
    fn on_event(&mut self, _event: &ExtEvent, _ctx: &mut HookCtx) {}

    /// Post-tick (20 Hz).
    fn on_tick(&mut self, _ctx: &mut HookCtx) {}

    /// Per frame (mainly for HUD/animation bookkeeping).
    fn on_frame(&mut self, _ctx: &mut HookCtx) {}

    /// Raw input; return `true` to consume (suppress default handling).
    fn on_input(&mut self, _input: &InputEvent, _ctx: &mut HookCtx) -> bool {
        false
    }

    /// Append HUD draw commands for this frame.
    fn draw_hud(&mut self, _hud: &mut HudDraw, _ctx: &HudCtx, _views: &dyn ReadViews) {}
}
