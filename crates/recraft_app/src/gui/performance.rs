//! The Performance settings screen: temporal anti-aliasing and (future) temporal
//! upscalers. TAA is functional; FSR and DLSS are placeholders for upscalers that
//! aren't implemented yet (DLSS is NVIDIA/Vulkan/Windows-only), shown disabled so
//! the planned feature set is visible.

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::options::GuiVideoSettings;
use super::widgets::GuiButton;
use super::{draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};
use crate::i18n::tr;

#[derive(Default)]
pub struct GuiPerformance {
    taa: Option<GuiButton>,
    fsr: Option<GuiButton>,
    dlss: Option<GuiButton>,
    done: Option<GuiButton>,
    from_main_menu: bool,
}

impl GuiPerformance {
    pub fn new(from_main_menu: bool) -> Self {
        Self {
            from_main_menu,
            ..Self::default()
        }
    }

    fn back_screen(&self) -> Box<dyn GuiScreen> {
        Box::new(GuiVideoSettings::new(self.from_main_menu))
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let x = (ctx.width - 200 * s) / 2;
        let cw = 200 * s;
        let top = ctx.height / 4;
        let row = |i: i32| top + i * 24 * s;
        self.taa = Some(GuiButton::at_px(x, row(0), cw, s, ""));
        // Not implemented yet — disabled placeholders for the planned upscalers.
        self.fsr = Some(GuiButton::at_px(x, row(1), cw, s, "").disabled(true));
        self.dlss = Some(GuiButton::at_px(x, row(2), cw, s, "").disabled(true));
        self.done = Some(GuiButton::at_px(x, row(3) + 12 * s, cw, s, tr("gui.done")));
    }
}

fn on_off(b: bool) -> String {
    tr(if b { "options.on" } else { "options.off" })
}

impl GuiScreen for GuiPerformance {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        [
            self.taa.as_ref(),
            self.fsr.as_ref(),
            self.dlss.as_ref(),
            self.done.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut recraft_render::UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 15 * s, s, super::TEXT_WHITE, &tr("recraft.perf.title"));
        let st = ctx.settings;
        let soon = tr("recraft.perf.unavailable");

        let mut draw = |btn: &mut Option<GuiButton>, label: String| {
            if let Some(b) = btn {
                b.label = label;
                b.draw(ui, s, ctx.mouse, ctx.mouse_down);
            }
        };
        draw(
            &mut self.taa,
            format!("{}: {}", tr("recraft.perf.taa"), on_off(st.taa)),
        );
        draw(&mut self.fsr, format!("{}: {}", tr("recraft.perf.fsr"), soon));
        draw(&mut self.dlss, format!("{}: {}", tr("recraft.perf.dlss"), soon));
        if let Some(b) = &self.done {
            b.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.taa.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.taa = !ctx.settings.taa;
            return vec![GuiAction::SetTaa(ctx.settings.taa)];
        }
        // FSR / DLSS are disabled placeholders; their buttons never report clicked.
        if self.done.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
        }
        Vec::new()
    }

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
        {
            return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
        }
        Vec::new()
    }

    fn draws_over_hud(&self) -> bool {
        !self.from_main_menu
    }
}
