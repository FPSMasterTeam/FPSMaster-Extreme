//! The Performance settings screen: temporal anti-aliasing, temporal upscalers
//! and ray tracing. TAA, FSR (render-scale presets) and hardware ray tracing
//! (sun shadows + RTAO) drive the renderer. DLSS drives it too when built with the
//! `dlss` feature; otherwise the toggle persists and falls back to FSR/TAA.

use recraft_render::UiRect;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::options::{draw_slider, slider_fraction, GuiVideoSettings};
use super::widgets::{GuiButton, BUTTON_HEIGHT};
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

/// Ray-tracing quality presets (placeholder — no RT path yet). Indices match
/// `Settings::rt_quality` (Low/Medium/High).
const RT_QUALITY_KEYS: [&str; 4] = [
    "recraft.perf.rt.low",
    "recraft.perf.rt.medium",
    "recraft.perf.rt.high",
    "recraft.perf.rt.pathtraced",
];

fn rt_quality_label(q: u32) -> String {
    // Level 3 is the experimental full path tracer — not localised (advanced control).
    if q >= 3 {
        return "Path Traced".to_string();
    }
    tr(RT_QUALITY_KEYS[(q as usize).min(RT_QUALITY_KEYS.len() - 1)])
}

/// DLSS quality slider stops (index = `Settings::dlss_quality`), labelled by the
/// pixel-area upscale factor and the DLSS mode each maps to. 9x (UltraPerformance) is
/// DLSS's hardware ceiling — it can't render below 1/3 linear res. Not localised — an
/// advanced/NVIDIA-specific control.
const DLSS_QUALITY_STOPS: [&str; 4] =
    ["1x (DLAA)", "2x (Quality)", "4x (Performance)", "9x (UltraPerf)"];

fn dlss_quality_label(q: u32) -> &'static str {
    DLSS_QUALITY_STOPS[(q as usize).min(DLSS_QUALITY_STOPS.len() - 1)]
}

#[derive(Default)]
pub struct GuiPerformance {
    taa: Option<GuiButton>,
    fsr: Option<GuiButton>,
    dlss: Option<GuiButton>,
    dlss_quality_rect: UiRect,
    rt: Option<GuiButton>,
    rt_quality: Option<GuiButton>,
    done: Option<GuiButton>,
    dragging_dlss_quality: bool,
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
        // DLSS toggle + quality (renderer path gated behind the `dlss` build feature).
        self.dlss = Some(GuiButton::at_px(x, row(2), cw, s, ""));
        self.dlss_quality_rect = UiRect::new(x, row(3), cw, BUTTON_HEIGHT * s);
        self.rt = Some(GuiButton::at_px(x, row(4), cw, s, ""));
        self.rt_quality = Some(GuiButton::at_px(x, row(5), cw, s, ""));
        self.done = Some(GuiButton::at_px(x, row(6) + 12 * s, cw, s, tr("gui.done")));
    }
}

fn on_off(b: bool) -> String {
    tr(if b { "options.on" } else { "options.off" })
}

impl GuiScreen for GuiPerformance {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.dlss_quality_rect.contains(x, y)
            || [
                self.taa.as_ref(),
                self.fsr.as_ref(),
                self.dlss.as_ref(),
                self.rt.as_ref(),
                self.rt_quality.as_ref(),
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
        // In a non-DLSS build the upscaler isn't available, so show the build hint
        // instead of an on/off the toggle can't honour.
        let dlss_value = if cfg!(feature = "dlss") {
            on_off(st.dlss)
        } else {
            "--features dlss".to_string()
        };
        draw(&mut self.dlss, format!("{}: {}", tr("recraft.perf.dlss"), dlss_value));
        draw(&mut self.rt, format!("{}: {}", tr("recraft.perf.rt"), on_off(st.ray_tracing)));
        draw(
            &mut self.rt_quality,
            format!("{}: {}", tr("recraft.perf.rt.quality"), rt_quality_label(st.rt_quality)),
        );
        // The closure above mutably borrows `ui`; draw the slider after its last use.
        draw_slider(
            ui,
            self.dlss_quality_rect,
            s,
            st.clone().dlss_quality_fraction(),
            &format!("DLSS {}: {}", tr("recraft.perf.rt.quality"), dlss_quality_label(st.dlss_quality)),
        );
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
            // FSR and DLSS are competing upscalers; picking an FSR preset turns DLSS off.
            if next != 0 && ctx.settings.dlss {
                ctx.settings.dlss = false;
                actions.push(GuiAction::SetDlss(false));
            }
            actions.push(GuiAction::SaveSettings);
            return actions;
        }
        // DLSS is a temporal upscaler — mutually exclusive with FSR + TAA, so enabling
        // it disables them (the renderer path is gated behind the `dlss` build feature).
        if self.dlss.as_ref().is_some_and(|b| b.clicked(x, y)) {
            // No DLSS in this build (needs `--features dlss`): make the toggle inert so
            // it doesn't confusingly drop FSR/TAA with no upscaler behind it.
            if !cfg!(feature = "dlss") {
                return Vec::new();
            }
            ctx.settings.dlss = !ctx.settings.dlss;
            let mut actions = vec![GuiAction::SetDlss(ctx.settings.dlss)];
            if ctx.settings.dlss {
                if ctx.settings.taa {
                    ctx.settings.taa = false;
                    actions.push(GuiAction::SetTaa(false));
                }
                if (ctx.settings.render_scale - 1.0).abs() > f32::EPSILON {
                    ctx.settings.render_scale = 1.0;
                    actions.push(GuiAction::SetRenderScale(1.0));
                }
            }
            actions.push(GuiAction::SaveSettings);
            return actions;
        }
        // DLSS quality slider (1x DLAA … 9x UltraPerformance). Inert without the dlss
        // build. Like render scale, the renderer rebuild happens on release, not per tick.
        if self.dlss_quality_rect.contains(x, y) {
            if !cfg!(feature = "dlss") {
                return Vec::new();
            }
            self.dragging_dlss_quality = true;
            ctx.settings
                .set_dlss_quality_from01(slider_fraction(self.dlss_quality_rect, x));
            return Vec::new();
        }
        // Ray tracing (shadows + RTAO) is orthogonal to the upscalers — it can run
        // alongside TAA, which also denoises the soft-shadow / RTAO samples.
        if self.rt.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.ray_tracing = !ctx.settings.ray_tracing;
            return vec![
                GuiAction::SetRayTracing(ctx.settings.ray_tracing),
                GuiAction::SaveSettings,
            ];
        }
        if self.rt_quality.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.rt_quality = (ctx.settings.rt_quality + 1) % RT_QUALITY_KEYS.len() as u32;
            return vec![
                GuiAction::SetRtQuality(ctx.settings.rt_quality),
                GuiAction::SaveSettings,
            ];
        }
        if self.done.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
        }
        Vec::new()
    }

    fn mouse_dragged(&mut self, x: f64, _y: f64, ctx: &mut ScreenCtx) {
        if self.dragging_dlss_quality {
            ctx.settings
                .set_dlss_quality_from01(slider_fraction(self.dlss_quality_rect, x));
        }
    }

    fn mouse_released(
        &mut self,
        _x: f64,
        _y: f64,
        _right: bool,
        ctx: &mut ScreenCtx,
    ) -> Vec<GuiAction> {
        if !self.dragging_dlss_quality {
            return Vec::new();
        }
        // Applying drops + rebuilds the DLSS context, so commit once on release.
        self.dragging_dlss_quality = false;
        vec![
            GuiAction::SetDlssQuality(ctx.settings.dlss_quality),
            GuiAction::SaveSettings,
        ]
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
