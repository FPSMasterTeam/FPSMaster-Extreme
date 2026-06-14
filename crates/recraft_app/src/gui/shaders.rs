//! The Shaders settings screen: a dedicated entry (opened from Options) toggling
//! the shader-pack lighting and the post-process effects. Two columns: lighting
//! on the left, post / world effects on the right.

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::options::GuiOptions;
use super::widgets::GuiButton;
use super::{draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

#[derive(Default)]
pub struct GuiShaders {
    // Left column: lighting.
    shaders: Option<GuiButton>,
    shadows: Option<GuiButton>,
    specular: Option<GuiButton>,
    fog: Option<GuiButton>,
    bloom: Option<GuiButton>,
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
        self.fog = Some(GuiButton::at_px(x, row(3), cw, s, "").disabled(off));
        self.bloom = Some(GuiButton::at_px(x, row(4), cw, s, "").disabled(off));
        self.vignette = Some(GuiButton::at_px(xr, row(0), cw, s, "").disabled(off));
        self.chromatic = Some(GuiButton::at_px(xr, row(1), cw, s, "").disabled(off));
        self.dof = Some(GuiButton::at_px(xr, row(2), cw, s, "").disabled(off));
        self.motion_blur = Some(GuiButton::at_px(xr, row(3), cw, s, "").disabled(off));
        self.auto_exposure = Some(GuiButton::at_px(xr, row(4), cw, s, "").disabled(off));
        self.clouds = Some(GuiButton::at_px(xr, row(5), cw, s, "").disabled(off));
        self.done = Some(GuiButton::at_px(x, row(6) + 12 * s, 200 * s, s, "Done"));
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        "ON"
    } else {
        "OFF"
    }
}

impl GuiScreen for GuiShaders {
    fn draw(&mut self, ui: &mut recraft_render::UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 15 * s, s, super::TEXT_WHITE, "Shaders");
        let st = ctx.settings;

        let mut draw = |btn: &mut Option<GuiButton>, label: String| {
            if let Some(b) = btn {
                b.label = label;
                b.draw(ui, s, ctx.mouse, ctx.mouse_down);
            }
        };
        draw(&mut self.shaders, format!("Shaders: {}", on_off(st.shaders)));
        draw(&mut self.shadows, format!("Shadows: {}", on_off(st.shader_shadows)));
        draw(&mut self.specular, format!("Specular: {}", on_off(st.shader_specular)));
        draw(&mut self.fog, format!("Fog: {}", on_off(st.shader_fog)));
        draw(&mut self.bloom, format!("Bloom: {}", on_off(st.shader_bloom)));
        draw(&mut self.vignette, format!("Vignette: {}", on_off(st.post_vignette)));
        draw(&mut self.chromatic, format!("Chroma: {}", on_off(st.post_chromatic)));
        draw(&mut self.dof, format!("DoF: {}", on_off(st.post_dof)));
        draw(&mut self.motion_blur, format!("Motion: {}", on_off(st.post_motion_blur)));
        draw(&mut self.auto_exposure, format!("AutoExp: {}", on_off(st.post_auto_exposure)));
        draw(&mut self.clouds, format!("Clouds: {}", on_off(st.volumetric_clouds)));
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
