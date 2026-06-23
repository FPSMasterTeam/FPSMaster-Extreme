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

/// FSR quality presets as render-scale factors: Off (native), then the standard
/// AMD ratios. "FSR" here is temporal upscaling — render at the scale, resolve to
/// display res via the TAA path — so picking a preset also forces TAA on.
const FSR_PRESETS: [f32; 4] = [1.0, 0.67, 0.59, 0.5];
const FSR_KEYS: [&str; 4] = [
    "recraft.perf.fsr.off",
    "recraft.perf.fsr.quality",
    "recraft.perf.fsr.balanced",
    "recraft.perf.fsr.performance",
];

/// Which preset the current render scale matches (within a tolerance), if any.
fn fsr_index(scale: f32) -> Option<usize> {
    FSR_PRESETS.iter().position(|&p| (p - scale).abs() < 0.02)
}

fn fsr_label(scale: f32) -> String {
    match fsr_index(scale) {
        Some(i) => tr(FSR_KEYS[i]),
        None => tr("recraft.perf.fsr.custom"),
    }
}

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
        // FSR = temporal upscaling (render scale preset + the TAA resolve).
        self.fsr = Some(GuiButton::at_px(x, row(1), cw, s, ""));
        // DLSS isn't implemented (NVIDIA / Vulkan / Windows only) — placeholder.
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
        draw(
            &mut self.fsr,
            format!("{}: {}", tr("recraft.perf.fsr"), fsr_label(st.render_scale)),
        );
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
        if self.fsr.as_ref().is_some_and(|b| b.clicked(x, y)) {
            // Cycle Off -> Quality -> Balanced -> Performance. A custom slider
            // scale counts as "off" so the first click snaps to Quality.
            let next = (fsr_index(ctx.settings.render_scale).unwrap_or(0) + 1) % FSR_PRESETS.len();
            let scale = FSR_PRESETS[next];
            ctx.settings.render_scale = scale;
            let mut actions = vec![GuiAction::SetRenderScale(scale)];
            // A non-native preset upscales, which needs the temporal resolve on.
            if next != 0 && !ctx.settings.taa {
                ctx.settings.taa = true;
                actions.push(GuiAction::SetTaa(true));
            }
            actions.push(GuiAction::SaveSettings);
            return actions;
        }
        // DLSS is a disabled placeholder; its button never reports clicked.
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
