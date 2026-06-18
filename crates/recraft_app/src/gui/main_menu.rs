//! The title screen (vanilla `GuiMainMenu`): dirt background, title logo,
//! the vanilla-textured button stack, version + copyright.

use recraft_render::{GuiTexture, UiColor, UiFrame, UiRect};

use super::accounts::GuiAccounts;
use super::demo_select::GuiDemoSelect;
use super::mods::GuiModList;
use super::multiplayer::GuiMultiplayer;
use super::options::GuiOptions;
use super::widgets::GuiButton;
use super::{draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

#[derive(Default)]
pub struct GuiMainMenu {
    buttons: Vec<GuiButton>,
}

impl GuiMainMenu {
    pub fn new() -> Self {
        Self::default()
    }

    /// Vanilla `initGui` + `addSingleplayerMultiplayerButtons`.
    /// All coordinates are in screen pixels (GUI px × scale).
    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let cx = ctx.width / 2;
        // j = height/4 + 48 (vanilla)
        let j = ctx.height / 4 + 48 * s;

        self.buttons = vec![
            // [0] Singleplayer: (cx-100, j), 200×20
            GuiButton::at_px(cx - 100 * s, j, 200 * s, s, "Singleplayer"),
            // [1] Multiplayer: (cx-100, j+24), 200×20
            GuiButton::at_px(cx - 100 * s, j + 24 * s, 200 * s, s, "Multiplayer"),
            // [2] Accounts: (cx-100, j+48), 200×20 (our equivalent of Realms)
            GuiButton::at_px(cx - 100 * s, j + 48 * s, 200 * s, s, "Accounts"),
            // [3] Demo World: (cx-100, j+72), 200×20
            GuiButton::at_px(cx - 100 * s, j + 72 * s, 200 * s, s, "Demo World"),
            // [4] Mods: (cx-100, j+96), 200×20 (recraft addition)
            GuiButton::at_px(cx - 100 * s, j + 96 * s, 200 * s, s, "Mods..."),
            // Bottom split row, vanilla-style: Options + Quit side by side.
            // [5] Options: (cx-100, j+120), 98×20
            GuiButton::at_px(cx - 100 * s, j + 120 * s, 98 * s, s, "Options..."),
            // [6] Quit: (cx+2, j+120), 98×20
            GuiButton::at_px(cx + 2 * s, j + 120 * s, 98 * s, s, "Quit Game"),
        ];
    }
}

impl GuiScreen for GuiMainMenu {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);

        if !ctx.has_panorama {
            // Fallback when panorama textures aren't available: dirt + gradients.
            draw_default_background(ui, ctx);
            let full = UiRect::new(0, 0, ctx.width, ctx.height);
            ui.gradient_rect(
                full,
                UiColor::rgba(255, 255, 255, 128),
                UiColor::rgba(255, 255, 255, 0),
            );
            ui.gradient_rect(
                full,
                UiColor::rgba(0, 0, 0, 0),
                UiColor::rgba(0, 0, 0, 128),
            );
        }

        let s = ctx.scale;

        // Custom MINECRAFT logo (single image, 1364×214). Drawn 256 GUI px wide,
        // aspect-preserved (~40 GUI px tall), centered horizontally near the top.
        const LOGO_SRC_W: u32 = 1364;
        const LOGO_SRC_H: u32 = 214;
        let logo_w = 256 * s;
        let logo_h = logo_w * LOGO_SRC_H as i32 / LOGO_SRC_W as i32;
        let logo_x = (ctx.width - logo_w) / 2;
        let logo_y = 30 * s;
        ui.image(
            UiRect::new(logo_x, logo_y, logo_w, logo_h),
            GuiTexture::Title,
            0,
            0,
            LOGO_SRC_W,
            LOGO_SRC_H,
        );

        // Buttons
        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }

        // Version string: bottom-left, white with shadow.
        ui.text_shadowed(2 * s, ctx.height - 10 * s, s, super::TEXT_WHITE, "ReCraft 1.8.9");

        // Account status: bottom-left, just above the version line.
        let account = match ctx.session_username {
            Some(name) => format!("Signed in as §a{name}"),
            None => "§7Not signed in".to_owned(),
        };
        ui.text_shadowed(
            2 * s,
            ctx.height - 20 * s,
            s,
            super::TEXT_GRAY,
            &account,
        );
    }

    fn wants_panorama(&self) -> bool {
        true
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.buttons.len() < 7 {
            return Vec::new();
        }
        if self.buttons[0].clicked(x, y) {
            return vec![GuiAction::StartSingleplayer];
        }
        if self.buttons[1].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiMultiplayer::new()))];
        }
        if self.buttons[2].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiAccounts::new()))];
        }
        if self.buttons[3].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiDemoSelect::new()))];
        }
        if self.buttons[4].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiModList::new(true)))];
        }
        if self.buttons[5].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiOptions::from_main_menu()))];
        }
        if self.buttons[6].clicked(x, y) {
            return vec![GuiAction::Quit];
        }
        Vec::new()
    }
}
