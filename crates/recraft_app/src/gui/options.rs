//! The options screen (vanilla `GuiOptions`): sensitivity and FPS sliders
//! drawn vanilla-style (disabled-button track + button-texture knob), the
//! vsync toggle button, opened from the pause menu.

use recraft_render::{text_width, GuiTexture, UiColor, UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::ingame_menu::GuiIngameMenu;
use super::main_menu::GuiMainMenu;
use super::widgets::{GuiButton, BUTTON_HEIGHT};
use super::{draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slider {
    Sensitivity,
    FpsCap,
    RenderScale,
    Brightness,
}

#[derive(Default)]
pub struct GuiOptions {
    sensitivity_rect: UiRect,
    fps_rect: UiRect,
    render_scale_rect: UiRect,
    brightness_rect: UiRect,
    vsync: Option<GuiButton>,
    graphics: Option<GuiButton>,
    mipmaps: Option<GuiButton>,
    resolution: Option<GuiButton>,
    fullscreen: Option<GuiButton>,
    shaders_btn: Option<GuiButton>,
    resource_packs_btn: Option<GuiButton>,
    done: Option<GuiButton>,
    dragging: Option<Slider>,
    /// Whether this was opened from the title screen (Done returns there) vs.
    /// the in-game pause menu.
    from_main_menu: bool,
}

impl GuiOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opened from the title screen — Done/ESC return to the main menu.
    pub fn from_main_menu() -> Self {
        Self {
            from_main_menu: true,
            ..Self::default()
        }
    }

    /// The screen to return to when leaving options.
    fn back_screen(&self) -> Box<dyn GuiScreen> {
        if self.from_main_menu {
            Box::new(GuiMainMenu::new())
        } else {
            Box::new(GuiIngameMenu::new())
        }
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let x = (ctx.width - 200 * s) / 2;
        let top = ctx.height / 4 - 24 * s;
        self.sensitivity_rect = UiRect::new(x, top, 200 * s, BUTTON_HEIGHT * s);
        self.vsync = Some(GuiButton::at_px(x, top + 24 * s, 200 * s, s, ""));
        self.fps_rect = UiRect::new(x, top + 48 * s, 200 * s, BUTTON_HEIGHT * s);
        self.render_scale_rect = UiRect::new(x, top + 72 * s, 200 * s, BUTTON_HEIGHT * s);
        self.brightness_rect = UiRect::new(x, top + 96 * s, 200 * s, BUTTON_HEIGHT * s);
        self.graphics = Some(GuiButton::at_px(x, top + 120 * s, 98 * s, s, ""));
        self.mipmaps = Some(GuiButton::at_px(x + 102 * s, top + 120 * s, 98 * s, s, ""));
        self.resolution = Some(GuiButton::at_px(x, top + 144 * s, 98 * s, s, ""));
        self.fullscreen = Some(GuiButton::at_px(x + 102 * s, top + 144 * s, 98 * s, s, ""));
        self.shaders_btn = Some(GuiButton::at_px(x, top + 168 * s, 200 * s, s, "Shaders..."));
        self.resource_packs_btn = Some(GuiButton::at_px(x, top + 192 * s, 200 * s, s, "Resource Packs..."));
        self.done = Some(GuiButton::at_px(x, top + 228 * s, 200 * s, s, "Done"));
    }

    fn slider_fraction(rect: UiRect, x: f64) -> f32 {
        if rect.width <= 0 {
            return 0.0;
        }
        (((x - rect.x as f64) / rect.width as f64) as f32).clamp(0.0, 1.0)
    }

    fn apply_drag(&mut self, x: f64, ctx: &mut ScreenCtx) {
        match self.dragging {
            Some(Slider::Sensitivity) => ctx
                .settings
                .set_sensitivity_from01(Self::slider_fraction(self.sensitivity_rect, x)),
            Some(Slider::FpsCap) => ctx
                .settings
                .set_fps_from01(Self::slider_fraction(self.fps_rect, x)),
            Some(Slider::RenderScale) => ctx
                .settings
                .set_render_scale_from01(Self::slider_fraction(self.render_scale_rect, x)),
            Some(Slider::Brightness) => ctx
                .settings
                .set_brightness_from01(Self::slider_fraction(self.brightness_rect, x)),
            None => {}
        }
    }
}

/// Draw a vanilla slider: disabled-button track plus an 8px-wide knob from
/// the idle button texture, with a centered label.
fn draw_slider(ui: &mut UiFrame, rect: UiRect, scale: i32, fraction: f32, label: &str) {
    ui.rect(rect, UiColor::rgba(50, 50, 50, 220));
    let width_gui = (rect.width / scale).clamp(2, 200);
    let half = width_gui / 2;
    ui.image(
        UiRect::new(rect.x, rect.y, half * scale, rect.height),
        GuiTexture::Widgets,
        0,
        46,
        half as u32,
        20,
    );
    ui.image(
        UiRect::new(rect.x + half * scale, rect.y, rect.width - half * scale, rect.height),
        GuiTexture::Widgets,
        (200 - (width_gui - half)) as u32,
        46,
        (width_gui - half) as u32,
        20,
    );
    // Knob: 8 GUI px wide, two 4px slices of the idle button texture.
    let knob_w = 8 * scale;
    let knob_x = rect.x
        + ((rect.width - knob_w) as f32 * fraction.clamp(0.0, 1.0)) as i32;
    ui.image(
        UiRect::new(knob_x, rect.y, 4 * scale, rect.height),
        GuiTexture::Widgets,
        0,
        66,
        4,
        20,
    );
    ui.image(
        UiRect::new(knob_x + 4 * scale, rect.y, 4 * scale, rect.height),
        GuiTexture::Widgets,
        196,
        66,
        4,
        20,
    );
    let text_x = rect.x + (rect.width - text_width(label, scale)) / 2;
    let text_y = rect.y + (rect.height - 8 * scale) / 2;
    ui.text_shadowed(text_x, text_y, scale, UiColor::rgba(224, 224, 224, 255), label);
}

impl GuiScreen for GuiOptions {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 15 * s, s, super::TEXT_WHITE, "Options");

        draw_slider(
            ui,
            self.sensitivity_rect,
            s,
            ctx.settings.sensitivity,
            &format!("Sensitivity: {:.0}%", ctx.settings.clone().sensitivity_percent()),
        );
        if let Some(vsync) = &mut self.vsync {
            vsync.label = format!("VSync: {}", if ctx.settings.vsync { "ON" } else { "OFF" });
            vsync.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
        draw_slider(
            ui,
            self.fps_rect,
            s,
            ctx.settings.clone().fps_fraction(),
            &format!("Max Framerate: {}", ctx.settings.clone().fps_label()),
        );
        draw_slider(
            ui,
            self.render_scale_rect,
            s,
            ctx.settings.clone().render_scale_fraction(),
            &format!("Render Scale: {}%", ctx.settings.clone().render_scale_percent()),
        );
        draw_slider(
            ui,
            self.brightness_rect,
            s,
            ctx.settings.clone().brightness_fraction(),
            &format!("Brightness: {}%", ctx.settings.clone().brightness_percent()),
        );
        if let Some(graphics) = &mut self.graphics {
            graphics.label = format!(
                "Graphics: {}",
                if ctx.settings.fancy_graphics { "Fancy" } else { "Fast" }
            );
            graphics.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
        if let Some(mipmaps) = &mut self.mipmaps {
            mipmaps.label = format!("Mipmaps: {}", ctx.settings.clone().mipmap_label());
            mipmaps.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
        if let Some(resolution) = &mut self.resolution {
            resolution.label = format!("Res: {}", ctx.settings.clone().resolution_label());
            resolution.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
        if let Some(fullscreen) = &mut self.fullscreen {
            fullscreen.label = format!(
                "Fullscreen: {}",
                if ctx.settings.fullscreen { "ON" } else { "OFF" }
            );
            fullscreen.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
        if let Some(b) = &self.shaders_btn {
            b.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
        if let Some(b) = &self.resource_packs_btn {
            b.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
        if let Some(done) = &self.done {
            done.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.sensitivity_rect.contains(x, y) {
            self.dragging = Some(Slider::Sensitivity);
            self.apply_drag(x, ctx);
            return Vec::new();
        }
        if self.fps_rect.contains(x, y) {
            self.dragging = Some(Slider::FpsCap);
            self.apply_drag(x, ctx);
            return Vec::new();
        }
        if self.render_scale_rect.contains(x, y) {
            self.dragging = Some(Slider::RenderScale);
            self.apply_drag(x, ctx);
            return Vec::new();
        }
        if self.brightness_rect.contains(x, y) {
            self.dragging = Some(Slider::Brightness);
            self.apply_drag(x, ctx);
            return Vec::new();
        }
        if self.vsync.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.vsync = !ctx.settings.vsync;
            return vec![GuiAction::SetVsync(ctx.settings.vsync)];
        }
        if self.graphics.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.fancy_graphics = !ctx.settings.fancy_graphics;
            return vec![GuiAction::SetFancyGraphics(ctx.settings.fancy_graphics)];
        }
        if self.mipmaps.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.cycle_mipmap_levels();
            return vec![GuiAction::SetMipmapLevels(ctx.settings.mipmap_levels)];
        }
        if self.resolution.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.cycle_resolution();
            return vec![GuiAction::SetResolution(ctx.settings.resolution)];
        }
        if self.fullscreen.as_ref().is_some_and(|b| b.clicked(x, y)) {
            ctx.settings.fullscreen = !ctx.settings.fullscreen;
            return vec![GuiAction::SetFullscreen(ctx.settings.fullscreen)];
        }
        if self.shaders_btn.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![
                GuiAction::SaveSettings,
                GuiAction::SetScreen(Box::new(super::shaders::GuiShaders::new(self.from_main_menu))),
            ];
        }
        if self.resource_packs_btn.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![
                GuiAction::SaveSettings,
                GuiAction::SetScreen(Box::new(
                    super::resource_packs::GuiResourcePacks::new(self.from_main_menu, ctx.settings),
                )),
            ];
        }
        if self.done.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
        }
        Vec::new()
    }

    fn mouse_dragged(&mut self, x: f64, _y: f64, ctx: &mut ScreenCtx) {
        self.apply_drag(x, ctx);
    }

    fn mouse_released(
        &mut self,
        _x: f64,
        _y: f64,
        _right: bool,
        ctx: &mut ScreenCtx,
    ) -> Vec<GuiAction> {
        // Render scale recreates off-screen targets, so apply it once on release
        // rather than on every drag tick; brightness just updates a uniform.
        let action = match self.dragging {
            Some(Slider::RenderScale) => vec![GuiAction::SetRenderScale(ctx.settings.render_scale)],
            Some(Slider::Brightness) => vec![GuiAction::SetBrightness(ctx.settings.brightness)],
            _ => Vec::new(),
        };
        self.dragging = None;
        action
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
        // Scrim over the world only when opened in-game. On the title screen the
        // options draw their own dirt background (vanilla shows dirt, not the
        // panorama, on sub-screens).
        !self.from_main_menu
    }
}
