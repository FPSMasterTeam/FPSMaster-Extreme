//! The ESC pause menu over a live world (vanilla `GuiIngameMenu`).

use recraft_render::UiFrame;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::mods::GuiModList;
use super::options::GuiOptions;
use super::widgets::GuiButton;
use super::{draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

#[derive(Default)]
pub struct GuiIngameMenu {
    buttons: Vec<GuiButton>,
}

impl GuiIngameMenu {
    pub fn new() -> Self {
        Self::default()
    }
}

impl GuiScreen for GuiIngameMenu {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        let s = ctx.scale;
        let x = (ctx.width - 200 * s) / 2;
        let top = ctx.height / 4 + 24 * s;
        self.buttons = vec![
            GuiButton::at_px(x, top, 200 * s, s, "Back to Game"),
            GuiButton::at_px(x, top + 24 * s, 200 * s, s, "Options..."),
            GuiButton::at_px(x, top + 48 * s, 200 * s, s, "Mods..."),
            // Vanilla separates the leave button from the rest with a gap.
            GuiButton::at_px(x, top + 96 * s, 200 * s, s, "Quit to Title"),
        ];
        draw_default_background(ui, ctx);
        draw_centered_text(ui, ctx.width, ctx.height / 4, s, super::TEXT_WHITE, "Game Menu");
        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.buttons.len() < 4 {
            return Vec::new();
        }
        if self.buttons[0].clicked(x, y) {
            return vec![GuiAction::CloseScreen];
        }
        if self.buttons[1].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiOptions::new()))];
        }
        if self.buttons[2].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiModList::new(false)))];
        }
        if self.buttons[3].clicked(x, y) {
            return vec![GuiAction::QuitToTitle];
        }
        Vec::new()
    }

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
        {
            return vec![GuiAction::CloseScreen];
        }
        Vec::new()
    }

    fn draws_over_hud(&self) -> bool {
        true
    }
}
