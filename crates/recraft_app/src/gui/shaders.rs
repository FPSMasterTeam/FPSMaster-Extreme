//! The Shaders settings screen: a dedicated entry (opened from Options) toggling
//! the shader-pack lighting and its sub-effects.

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::options::GuiOptions;
use super::widgets::GuiButton;
use super::{draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

#[derive(Default)]
pub struct GuiShaders {
    shaders: Option<GuiButton>,
    shadows: Option<GuiButton>,
    specular: Option<GuiButton>,
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
        let top = ctx.height / 4;
        self.shaders = Some(GuiButton::at_px(x, top, 200 * s, s, ""));
        self.shadows = Some(GuiButton::at_px(x, top + 24 * s, 200 * s, s, ""));
        self.specular = Some(GuiButton::at_px(x, top + 48 * s, 200 * s, s, ""));
        self.done = Some(GuiButton::at_px(x, top + 84 * s, 200 * s, s, "Done"));
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

        if let Some(b) = &mut self.shaders {
            b.label = format!("Shaders: {}", on_off(ctx.settings.shaders));
            b.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
        if let Some(b) = &mut self.shadows {
            b.label = format!("Sun Shadows: {}", on_off(ctx.settings.shader_shadows));
            b.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
        if let Some(b) = &mut self.specular {
            b.label = format!("Specular: {}", on_off(ctx.settings.shader_specular));
            b.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
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
