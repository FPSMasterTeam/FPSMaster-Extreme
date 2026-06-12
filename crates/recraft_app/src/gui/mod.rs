//! The screen system, modeled on vanilla's `GuiScreen`: at most one screen is
//! open at a time (`Option<Box<dyn GuiScreen>>`, vanilla `mc.currentScreen`);
//! with no screen open the player has gameplay input and a captured cursor.
//! Screens receive routed input, draw through [`UiFrame`] commands, and talk
//! back to the application through [`GuiAction`]s — they never touch the
//! network or window directly. The in-game HUD is not a screen; it is
//! [`ingame::GuiIngame`], drawn whenever a world is active.

pub mod accounts;
pub mod chat_screen;
pub mod edit_server;
pub mod game_over;
pub mod ingame;
pub mod ingame_menu;
pub mod inventory;
pub mod main_menu;
pub mod multiplayer;
pub mod options;
pub mod progress;
pub mod widgets;

use recraft_render::{GuiTexture, UiColor, UiFrame, UiRect};
use winit::event::KeyEvent;

use crate::game::GameState;
use crate::gui::ingame::HudState;
use crate::settings::Settings;

/// One open screen. Implementations hold their own widgets and produce
/// [`GuiAction`]s; the main loop owns navigation and app side effects.
pub trait GuiScreen {
    /// Lay out and draw the screen (called every frame; layout is cheap and
    /// re-derives from the current window size, vanilla initGui style).
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx);

    fn mouse_clicked(&mut self, _x: f64, _y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        Vec::new()
    }

    fn mouse_released(&mut self, _x: f64, _y: f64) {}

    fn mouse_dragged(&mut self, _x: f64, _y: f64, _ctx: &mut ScreenCtx) {}

    fn mouse_scrolled(&mut self, _delta: f32) {}

    fn key_pressed(&mut self, _event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        Vec::new()
    }

    /// Per-frame upkeep (poll ping results, auto-close, …).
    fn update(&mut self, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        Vec::new()
    }

    /// The live chat input when this screen is the chat overlay; the HUD's
    /// chat panel renders the box and backlog.
    fn chat_input(&self) -> Option<&str> {
        None
    }

    /// Whether the in-game HUD should keep rendering beneath this screen
    /// (pause/options/inventory/chat/death over a live world).
    fn draws_over_hud(&self) -> bool {
        false
    }
}

/// What a screen asks the application to do.
pub enum GuiAction {
    SetScreen(Box<dyn GuiScreen>),
    /// Close the screen and return to gameplay (re-captures the cursor).
    CloseScreen,
    StartDemo,
    Connect { host: String, port: u16 },
    QuitToTitle,
    Quit,
    SendChat(String),
    RequestRespawn,
    StartMicrosoftLogin,
    LoginWithToken(String),
    UseAccount(String),
    RemoveAccount(String),
    CopyActiveToken,
    /// Settings were edited; the renderer must apply the new vsync mode.
    SetVsync(bool),
}

/// Immutable per-frame data screens draw from.
pub struct DrawCtx<'a> {
    pub width: i32,
    pub height: i32,
    /// GUI pixel scale (vanilla gui scale, 2..4).
    pub scale: i32,
    pub mouse: (f64, f64),
    pub mouse_down: bool,
    pub chunk_count: usize,
    /// Whether a world is rendered behind the screen (scrim background)
    /// rather than the dirt menu background.
    pub in_world: bool,
    pub settings: &'a Settings,
    pub session_username: Option<&'a str>,
    pub accounts: &'a [AccountEntry],
    /// HUD data when a world session is active (inventory screen, HUD).
    pub hud: Option<&'a HudState<'a>>,
}

/// Mutable application state screens may edit directly during input handling
/// (vanilla screens reach through `mc.*` the same way).
pub struct ScreenCtx<'a> {
    pub game: &'a mut GameState,
    pub settings: &'a mut Settings,
    pub clipboard: Option<&'a mut arboard::Clipboard>,
}

/// A saved account row for the accounts screen.
#[derive(Debug, Clone)]
pub struct AccountEntry {
    pub username: String,
    pub uuid: String,
    /// Whether this is the currently signed-in account.
    pub active: bool,
}

/// GUI pixel scale shared by every screen and the HUD (same formula the
/// renderer rasterizes the UI buffer at, so all layout snaps to GUI pixels).
pub fn gui_scale(height: i32) -> i32 {
    recraft_render::gui_pixel_scale(height.max(1) as u32) as i32
}

// ─── Shared drawing helpers ──────────────────────────────────────────────────

pub(crate) const TEXT_WHITE: UiColor = UiColor::rgba(255, 255, 255, 255);
pub(crate) const TEXT_GRAY: UiColor = UiColor::rgba(160, 160, 160, 255);
pub(crate) const TEXT_YELLOW: UiColor = UiColor::rgba(255, 255, 85, 255);
pub(crate) const TEXT_GREEN: UiColor = UiColor::rgba(85, 255, 85, 255);

/// Vanilla `drawDefaultBackground`: the tiled dirt texture (tinted gray 64,
/// one repeat per 32 GUI px) on menus, or a translucent scrim over a world.
pub(crate) fn draw_default_background(ui: &mut UiFrame, ctx: &DrawCtx) {
    let full = UiRect::new(0, 0, ctx.width, ctx.height);
    if ctx.in_world {
        ui.rect(full, UiColor::rgba(16, 16, 16, 160));
    } else {
        ui.tiled_image(
            full,
            GuiTexture::OptionsBackground,
            32 * ctx.scale,
            UiColor::rgba(64, 64, 64, 255),
        );
    }
}

/// Centered shadowed text at a GUI-pixel y position.
pub(crate) fn draw_centered_text(
    ui: &mut UiFrame,
    width: i32,
    y: i32,
    scale: i32,
    color: UiColor,
    text: &str,
) {
    let w = recraft_render::text_width(text, scale);
    ui.text_shadowed((width - w) / 2, y, scale, color, text);
}

/// Truncate with an ellipsis to fit `max_width` screen px.
pub(crate) fn fit_text(text: &str, max_width: i32, scale: i32) -> String {
    if recraft_render::text_width(text, scale) <= max_width {
        return text.to_owned();
    }
    let ellipsis = "...";
    let mut out = String::new();
    for ch in text.chars() {
        let candidate = format!("{out}{ch}{ellipsis}");
        if recraft_render::text_width(&candidate, scale) > max_width {
            break;
        }
        out.push(ch);
    }
    out.push_str(ellipsis);
    out
}
