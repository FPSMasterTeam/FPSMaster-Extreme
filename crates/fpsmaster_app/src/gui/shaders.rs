//! The Shaders settings screen: a dedicated entry (opened from Options) toggling
//! the shader-pack lighting and the post-process effects. Two columns: lighting
//! on the left, post / world effects on the right.

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::options::GuiOptions;
use super::widgets::GuiButton;
use super::{draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};
use crate::i18n::tr;

#[derive(Default)]
pub struct GuiShaders {
    // Left column: lighting.
    shaders: Option<GuiButton>,
    shadows: Option<GuiButton>,
    specular: Option<GuiButton>,
    fog: Option<GuiButton>,
    bloom: Option<GuiButton>,
    volumetric: Option<GuiButton>,
    // Right column: post / world.
    vignette: Option<GuiButton>,
    chromatic: Option<GuiButton>,
    dof: Option<GuiButton>,
    motion_blur: Option<GuiButton>,
    auto_exposure: Option<GuiButton>,
    clouds: Option<GuiButton>,
    done: Option<GuiButton>,
    from_main_menu: bool,
}

impl GuiShaders {
    pub fn new(from_main_menu: bool) -> Self {
        Self {
            from_main_menu,
            ..Self::default()
        }
    }

    fn back_screen(&self) -> Box<dyn GuiScreen> {
        if self.from_main_menu {
            Box::new(GuiOptions::from_main_menu())
        } else {
            Box::new(GuiOptions::new())
        }
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let x = (ctx.width - 200 * s) / 2;
        let xr = x + 102 * s;
        let cw = 98 * s;
        let top = ctx.height / 4 - 8 * s;
        let row = |i: i32| top + i * 24 * s;
        // Sub-effects are unavailable while the master shader toggle is off.
        let off = !ctx.settings.shaders;
        self.shaders = Some(GuiButton::at_px(x, row(0), cw, s, ""));
        self.shadows = Some(GuiButton::at_px(x, row(1), cw, s, "").disabled(off));
        self.specular = Some(GuiButton::at_px(x, row(2), cw, s, "").disabled(off));
        // Fog is a plain distance fade to the horizon — it works with shaders off
        // (and is the knob that makes a low render distance look acceptable), so
        // it stays enabled even when the master shader toggle is off.
        self.fog = Some(GuiButton::at_px(x, row(3), cw, s, ""));
        self.bloom = Some(GuiButton::at_px(x, row(4), cw, s, "").disabled(off));
        self.volumetric = Some(GuiButton::at_px(x, row(5), cw, s, "").disabled(off));
        self.vignette = Some(GuiButton::at_px(xr, row(0), cw, s, "").disabled(off));
        self.chromatic = Some(GuiButton::at_px(xr, row(1), cw, s, "").disabled(off));
        self.dof = Some(GuiButton::at_px(xr, row(2), cw, s, "").disabled(off));
        self.motion_blur = Some(GuiButton::at_px(xr, row(3), cw, s, "").disabled(off));
        self.auto_exposure = Some(GuiButton::at_px(xr, row(4), cw, s, "").disabled(off));
        self.clouds = Some(GuiButton::at_px(xr, row(5), cw, s, "").disabled(off));
        self.done = Some(GuiButton::at_px(x, row(6) + 12 * s, 200 * s, s, tr("gui.done")));
    }
}

fn on_off(b: bool) -> String {
    tr(if b { "options.on" } else { "options.off" })
}

impl GuiScreen for GuiShaders {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        [
            self.shaders.as_ref(),
            self.shadows.as_ref(),
            self.specular.as_ref(),
            self.fog.as_ref(),
            self.bloom.as_ref(),
            self.volumetric.as_ref(),
            self.vignette.as_ref(),
            self.chromatic.as_ref(),
            self.dof.as_ref(),
            self.motion_blur.as_ref(),
            self.auto_exposure.as_ref(),
            self.clouds.as_ref(),
            self.done.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut fpsmaster_render::UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 15 * s, s, super::TEXT_WHITE, &tr("fpsmaster.shaders.title"));
        let st = ctx.settings;

        let mut draw = |btn: &mut Option<GuiButton>, label: String| {
            if let Some(b) = btn {
                b.label = label;
                b.draw(ui, s, ctx.mouse, ctx.mouse_down);
            }
        };
        let entry = |key: &str, on: bool| format!("{}: {}", tr(key), on_off(on));
        draw(&mut self.shaders, entry("fpsmaster.shaders.shaders", st.shaders));
        draw(&mut self.shadows, entry("fpsmaster.shaders.shadows", st.shader_shadows));
        draw(&mut self.specular, entry("fpsmaster.shaders.specular", st.shader_specular));
        draw(&mut self.fog, entry("fpsmaster.shaders.fog", st.shader_fog));
        draw(&mut self.bloom, entry("fpsmaster.shaders.bloom", st.shader_bloom));
        draw(&mut self.volumetric, entry("fpsmaster.shaders.volLight", st.volumetric_light));
        draw(&mut self.vignette, entry("fpsmaster.shaders.vignette", st.post_vignette));
        draw(&mut self.chromatic, entry("fpsmaster.shaders.chroma", st.post_chromatic));
        draw(&mut self.dof, entry("fpsmaster.shaders.dof", st.post_dof));
        draw(&mut self.motion_blur, entry("fpsmaster.shaders.motion", st.post_motion_blur));
        draw(&mut self.auto_exposure, entry("fpsmaster.shaders.autoExp", st.post_auto_exposure));
        draw(&mut self.clouds, entry("fpsmaster.shaders.clouds", st.volumetric_clouds));
        if let Some(b) = &self.done {
            b.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.shaders.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.shaders = !ctx.settings.shaders;
            return vec![GuiAction::SetShaders(ctx.settings.shaders)];
        }
        if self.shadows.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.shader_shadows = !ctx.settings.shader_shadows;
            return vec![GuiAction::SetShaderShadows(ctx.settings.shader_shadows)];
        }
        if self.specular.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.shader_specular = !ctx.settings.shader_specular;
            return vec![GuiAction::SetShaderSpecular(ctx.settings.shader_specular)];
        }
        if self.fog.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.shader_fog = !ctx.settings.shader_fog;
            return vec![GuiAction::SetShaderFog(ctx.settings.shader_fog)];
        }
        if self.bloom.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.shader_bloom = !ctx.settings.shader_bloom;
            return vec![GuiAction::SetShaderBloom(ctx.settings.shader_bloom)];
        }
        if self.volumetric.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.volumetric_light = !ctx.settings.volumetric_light;
            return vec![GuiAction::SetVolumetricLight(ctx.settings.volumetric_light)];
        }
        if self.vignette.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.post_vignette = !ctx.settings.post_vignette;
            return vec![GuiAction::SetVignette(ctx.settings.post_vignette)];
        }
        if self.chromatic.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.post_chromatic = !ctx.settings.post_chromatic;
            return vec![GuiAction::SetChromatic(ctx.settings.post_chromatic)];
        }
        if self.dof.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.post_dof = !ctx.settings.post_dof;
            return vec![GuiAction::SetDof(ctx.settings.post_dof)];
        }
        if self.motion_blur.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.post_motion_blur = !ctx.settings.post_motion_blur;
            return vec![GuiAction::SetMotionBlur(ctx.settings.post_motion_blur)];
        }
        if self.auto_exposure.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.post_auto_exposure = !ctx.settings.post_auto_exposure;
            return vec![GuiAction::SetAutoExposure(ctx.settings.post_auto_exposure)];
        }
        if self.clouds.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.volumetric_clouds = !ctx.settings.volumetric_clouds;
            return vec![GuiAction::SetClouds(ctx.settings.volumetric_clouds)];
        }
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
