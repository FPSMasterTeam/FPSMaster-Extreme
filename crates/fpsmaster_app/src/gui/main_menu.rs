//! The title screen (vanilla `GuiMainMenu`): dirt background, title logo,
//! the vanilla-textured button stack, version + copyright.

use fpsmaster_render::{GuiTexture, UiColor, UiFrame, UiRect};

use crate::i18n::tr;

use super::mods::GuiModList;
use super::multiplayer::GuiMultiplayer;
use super::options::GuiOptions;
use super::widgets::GuiButton;
use super::world_select::GuiSelectWorld;
use super::{draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

#[derive(Default)]
pub struct GuiMainMenu {
    buttons: Vec<GuiButton>,
    /// A random line from the vanilla `texts/splashes.txt`, chosen once when the
    /// title screen is (re)opened — vanilla re-rolls it in `GuiMainMenu.initGui`.
    splash: Option<String>,
}

impl GuiMainMenu {
    pub fn new() -> Self {
        Self {
            splash: pick_splash(),
            ..Self::default()
        }
    }

    /// Vanilla `initGui` + `addSingleplayerMultiplayerButtons`.
    /// All coordinates are in screen pixels (GUI px × scale).
    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let cx = ctx.width / 2;
        // j = height/4 + 48 (vanilla)
        let j = ctx.height / 4 + 48 * s;

        self.buttons = vec![
            // [0] Singleplayer: (cx-100, j), 200×20 — opens the world-select screen.
            GuiButton::at_px(cx - 100 * s, j, 200 * s, s, tr("menu.singleplayer")),
            // [1] Multiplayer: (cx-100, j+24), 200×20
            GuiButton::at_px(cx - 100 * s, j + 24 * s, 200 * s, s, tr("menu.multiplayer")),
            // [2] Mods: (cx-100, j+48), 200×20 (fpsmaster addition)
            GuiButton::at_px(cx - 100 * s, j + 48 * s, 200 * s, s, tr("fpsmaster.menu.mods")),
            // Bottom split row, vanilla-style: Options + Quit side by side.
            // [3] Options: (cx-100, j+72), 98×20
            GuiButton::at_px(cx - 100 * s, j + 72 * s, 98 * s, s, tr("menu.options")),
            // [4] Quit: (cx+2, j+72), 98×20
            GuiButton::at_px(cx + 2 * s, j + 72 * s, 98 * s, s, tr("menu.quit")),
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
        let cx = ctx.width / 2;

        // Vanilla MINECRAFT logo from gui/title/minecraft.png: two 155×44 halves
        // (rows at v=0 and v=45 of the 256×256 sheet) blitted side by side, exactly
        // like vanilla `GuiMainMenu` (`j = width/2 - 137`, logo at y=30).
        let logo_y = 30 * s;
        let j = cx - 137 * s;
        ui.image(UiRect::new(j, logo_y, 155 * s, 44 * s), GuiTexture::Title, 0, 0, 155, 44);
        ui.image(
            UiRect::new(j + 155 * s, logo_y, 155 * s, 44 * s),
            GuiTexture::Title,
            0,
            45,
            155,
            44,
        );

        // Splash text from the vanilla texts/splashes.txt, yellow with a shadow near
        // the logo's lower-right (vanilla anchors it at width/2+90, tilted ~20° and
        // pulsing — the tilt/pulse need rotated, fractionally-scaled glyphs the UI
        // layer has no primitive for, so it renders upright at the GUI scale).
        if let Some(splash) = &self.splash {
            let splash_w = fpsmaster_render::text_width(splash, s);
            let splash_x = (cx + 88 * s - splash_w / 2)
                .min(ctx.width - splash_w - 2 * s)
                .max(2 * s);
            let splash_y = logo_y + 34 * s;
            ui.text_shadowed(splash_x, splash_y, s, super::TEXT_YELLOW, splash);
        }

        // Brand subtitle under the logo: "FPSMaster Extreme", centered.
        {
            let brand = crate::version::PRODUCT_NAME;
            let brand_w = fpsmaster_render::text_width(brand, s);
            let brand_x = (ctx.width - brand_w) / 2;
            let brand_y = logo_y + 46 * s;
            ui.text_shadowed(brand_x, brand_y, s, super::TEXT_YELLOW, brand);
        }

        // Buttons
        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }

        // Version string: bottom-left, white with shadow.
        ui.text_shadowed(
            2 * s,
            ctx.height - 10 * s,
            s,
            super::TEXT_WHITE,
            crate::version::title(),
        );
    }

    fn wants_panorama(&self) -> bool {
        true
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.buttons.len() < 5 {
            return Vec::new();
        }
        if self.buttons[0].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiSelectWorld::new()))];
        }
        if self.buttons[1].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiMultiplayer::new()))];
        }
        if self.buttons[2].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiModList::new(true)))];
        }
        if self.buttons[3].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiOptions::from_main_menu()))];
        }
        if self.buttons[4].clicked(x, y) {
            return vec![GuiAction::Quit];
        }
        Vec::new()
    }
}

/// The vanilla splash file, one splash per line, under the active assets.
const SPLASHES_ASSET: &str = "assets/minecraft/texts/splashes.txt";

/// Pick a random splash line from the vanilla [`SPLASHES_ASSET`], or `None` when
/// the asset is unavailable (no vanilla assets extracted). Blank lines are
/// dropped; the choice is seeded from the wall clock so it varies per visit,
/// matching vanilla's per-`initGui` re-roll.
fn pick_splash() -> Option<String> {
    let text = fpsmaster_render::read_asset_string(SPLASHES_ASSET)?;
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some(lines[(seed % lines.len() as u128) as usize].to_owned())
}
