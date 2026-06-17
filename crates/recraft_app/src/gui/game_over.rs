//! The death screen (vanilla `GuiGameOver`): red scrim, respawn / quit.

use recraft_render::{UiColor, UiFrame, UiRect};

use super::widgets::GuiButton;
use super::{draw_centered_text, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

#[derive(Default)]
pub struct GuiGameOver {
    buttons: Vec<GuiButton>,
}

impl GuiGameOver {
    pub fn new() -> Self {
        Self::default()
    }
}

impl GuiScreen for GuiGameOver {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        let s = ctx.scale;
        let x = (ctx.width - 200 * s) / 2;
        let top = ctx.height / 4 + 48 * s;
        self.buttons = vec![
            GuiButton::at_px(x, top, 200 * s, s, "Respawn"),
            GuiButton::at_px(x, top + 24 * s, 200 * s, s, "Title Screen"),
        ];
        ui.rect(
            UiRect::new(0, 0, ctx.width, ctx.height),
            UiColor::rgba(96, 16, 16, 130),
        );
        draw_centered_text(
            ui,
            ctx.width,
            ctx.height / 4,
            2 * s,
            super::TEXT_WHITE,
            "You Died!",
        );
        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.buttons.len() < 2 {
            return Vec::new();
        }
        if self.buttons[0].clicked(x, y) {
            return vec![GuiAction::RequestRespawn];
        }
        if self.buttons[1].clicked(x, y) {
            return vec![GuiAction::QuitToTitle];
        }
        Vec::new()
    }

    fn update(&mut self, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        // Leave the screen as soon as we're alive again (respawn confirmed or
        // the server revived us).
        if !ctx.game.is_dead() {
            return vec![GuiAction::CloseScreen];
        }
        Vec::new()
    }

    fn draws_over_hud(&self) -> bool {
        true
    }

    // The death screen draws its own red scrim, so skip the central one.
    fn dims_world(&self) -> bool {
        false
    }
}
