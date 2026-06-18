//! In-tree demo mod: a Rust `HostHooks` impl that exercises all four seams.
//! `recraft_app` registers it behind the `RECRAFT_EXT_DEMO` env var so the wiring
//! can be validated end-to-end before any JS/native mod exists. It is also the
//! clearest worked example of the hook surface.

use crate::command::LogLevel;
use crate::event::Verdict;
use crate::host::{HookCtx, HostHooks};
use crate::hud::{HudCtx, HudDraw};
use crate::input::InputEvent;
use crate::packet::PacketView;
use crate::view::ReadViews;

#[derive(Debug, Default)]
pub struct DemoMod {
    greeted: bool,
    ticks: u64,
    pub clientbound_seen: u64,
    pub chat_seen: u64,
}

impl DemoMod {
    pub fn new() -> Self {
        Self::default()
    }
}

impl HostHooks for DemoMod {
    fn id(&self) -> &str {
        "recraft.demo"
    }

    fn on_load(&mut self, ctx: &mut HookCtx) {
        ctx.info("demo mod loaded — validating four seams (hud/packet/chat/input)");
    }

    // Seam 2: clientbound packet observation.
    fn on_clientbound_packet(&mut self, packet: &PacketView, ctx: &mut HookCtx) -> Verdict {
        self.clientbound_seen += 1;
        if let PacketView::Chat { text, .. } = packet {
            self.chat_seen += 1;
            ctx.log(LogLevel::Info, format!("[demo] saw chat packet: {text}"));
        }
        Verdict::Pass
    }

    // Seam 3 (command): send one chat once we are well into the world.
    fn on_tick(&mut self, ctx: &mut HookCtx) {
        self.ticks += 1;
        if !self.greeted && self.ticks >= 40 {
            self.greeted = true;
            ctx.send_chat("recraft extension demo online");
        }
    }

    // Seam: custom keybind (consume F6).
    fn on_input(&mut self, input: &InputEvent, ctx: &mut HookCtx) -> bool {
        if input.key == "F6" && input.pressed {
            ctx.info("[demo] F6 consumed by demo mod");
            return true;
        }
        false
    }

    // Seam 1: HUD draw.
    fn draw_hud(&mut self, hud: &mut HudDraw, _ctx: &HudCtx, views: &dyn ReadViews) {
        let p = views.player();
        hud.text(
            4,
            4,
            2,
            0xFFFF55FF,
            format!("demo xyz {:.1} {:.1} {:.1}", p.x, p.y, p.z),
        );
    }
}
