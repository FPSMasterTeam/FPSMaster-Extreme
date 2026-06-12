//! The title screen (vanilla `GuiMainMenu`): dirt background, big title,
//! the vanilla-textured button stack.

use recraft_render::UiFrame;

use super::accounts::GuiAccounts;
use super::multiplayer::GuiMultiplayer;
use super::widgets::GuiButton;
use super::{draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

#[derive(Default)]
pub struct GuiMainMenu {
    buttons: Vec<GuiButton>,
}

impl GuiMainMenu {
    pub fn new() -> Self {
        Self::default()
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let x = (ctx.width - 200 * s) / 2;
        let top = ctx.height / 4 + 36 * s;
        self.buttons = vec![
            GuiButton::at_px(x, top, 200 * s, s, "Singleplayer (Demo)"),
            GuiButton::at_px(x, top + 24 * s, 200 * s, s, "Multiplayer"),
            GuiButton::at_px(x, top + 48 * s, 200 * s, s, "Accounts"),
            GuiButton::at_px(x, top + 78 * s, 200 * s, s, "Quit Game"),
        ];
    }
}

impl GuiScreen for GuiMainMenu {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);

        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, ctx.height / 4 - 10 * s, 4 * s, super::TEXT_WHITE, "RECRAFT");
        draw_centered_text(
            ui,
            ctx.width,
            ctx.height / 4 + 26 * s,
            s,
            super::TEXT_YELLOW,
            "A Rust-native Minecraft 1.8.9 client",
        );

        let account = match ctx.session_username {
            Some(name) => format!("Signed in as §a{name}"),
            None => "§7Not signed in".to_owned(),
        };
        ui.text_shadowed(2 * s, ctx.height - 10 * s, s, super::TEXT_GRAY, "ReCraft 1.8.9");
        draw_centered_text(ui, ctx.width, ctx.height - 10 * s, s, super::TEXT_GRAY, &account);

        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.buttons.len() < 4 {
            return Vec::new(); // not laid out yet (no draw since the switch)
        }
        if self.buttons[0].clicked(x, y) {
            return vec![GuiAction::StartDemo];
        }
        if self.buttons[1].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiMultiplayer::new()))];
        }
        if self.buttons[2].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiAccounts::new()))];
        }
        if self.buttons[3].clicked(x, y) {
            return vec![GuiAction::Quit];
        }
        Vec::new()
    }
}
