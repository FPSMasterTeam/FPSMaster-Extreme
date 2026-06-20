//! The screen system, modeled on vanilla's `GuiScreen`: at most one screen is
//! open at a time (`Option<Box<dyn GuiScreen>>`, vanilla `mc.currentScreen`);
//! with no screen open the player has gameplay input and a captured cursor.
//! Screens receive routed input, draw through [`UiFrame`] commands, and talk
//! back to the application through [`GuiAction`]s — they never touch the
//! network or window directly. The in-game HUD is not a screen; it is
//! [`ingame::GuiIngame`], drawn whenever a world is active.

pub mod accounts;
pub mod chat_screen;
pub mod controls;
pub mod demo_select;
pub mod edit_server;
pub mod enchant;
pub mod game_over;
pub mod ingame;
pub mod ingame_menu;
pub mod inventory;
pub mod language;
pub mod main_menu;
pub mod merchant;
pub mod mods;
pub mod multiplayer;
pub mod options;
pub mod progress;
pub mod resource_packs;
pub mod shaders;
pub mod widgets;

use recraft_protocol::v1_8_9::packets::ServerboundPacket;
use recraft_render::{GuiTexture, UiColor, UiFrame, UiRect};
use winit::event::KeyEvent;
use winit::keyboard::ModifiersState;

use crate::game::GameState;
use crate::gui::ingame::HudState;
use crate::settings::Settings;
use crate::text_input::TextInput;

/// One open screen. Implementations hold their own widgets and produce
/// [`GuiAction`]s; the main loop owns navigation and app side effects.
pub trait GuiScreen {
    /// Lay out and draw the screen (called every frame; layout is cheap and
    /// re-derives from the current window size, vanilla initGui style).
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx);

    fn mouse_clicked(&mut self, _x: f64, _y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        Vec::new()
    }

    /// Whether a left-click at (x, y) lands on an activatable button, so the
    /// host can play `gui.button.press` (vanilla plays it centrally in
    /// `GuiScreen.mouseClicked` for the hit button). Default: no buttons.
    fn clicks_button(&self, _x: f64, _y: f64) -> bool {
        false
    }

    /// A right mouse press (most screens ignore it; the inventory uses it for
    /// half-pickup / place-one / right paint-drag).
    fn mouse_right_clicked(&mut self, _x: f64, _y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        Vec::new()
    }

    /// A middle mouse press (the container screen uses it for the creative
    /// clone, vanilla mode 3).
    fn mouse_middle_clicked(&mut self, _x: f64, _y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        Vec::new()
    }

    /// A mouse-button release. `right` is true for the right button, so a
    /// screen can tell which button ends an in-progress gesture (paint-drag).
    fn mouse_released(
        &mut self,
        _x: f64,
        _y: f64,
        _right: bool,
        _ctx: &mut ScreenCtx,
    ) -> Vec<GuiAction> {
        Vec::new()
    }

    fn mouse_dragged(&mut self, _x: f64, _y: f64, _ctx: &mut ScreenCtx) {}

    fn mouse_scrolled(&mut self, _delta: f32) {}

    fn key_pressed(&mut self, _event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        Vec::new()
    }

    /// Per-frame upkeep (poll ping results, auto-close, …).
    fn update(&mut self, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        Vec::new()
    }

    /// The live chat input text when this screen is the chat overlay; the
    /// HUD's chat panel renders the box and backlog.
    fn chat_input(&self) -> Option<&str> {
        None
    }

    /// The chat overlay's editable buffer, for the HUD to render its content,
    /// caret and IME composition (and to record the caret area).
    fn chat_input_mut(&mut self) -> Option<&mut TextInput> {
        None
    }

    /// The text buffer currently accepting input on this screen, if any. The
    /// host routes IME events here, enables IME while one is present, and reads
    /// its caret area to place the candidate window.
    fn focused_text_input(&mut self) -> Option<&mut TextInput> {
        None
    }

    /// Whether the in-game HUD should keep rendering beneath this screen
    /// (pause/options/inventory/chat/death over a live world).
    fn draws_over_hud(&self) -> bool {
        false
    }

    /// Whether the world should be dimmed by a scrim (drawn before the HUD, so the
    /// HUD stays at full brightness on top of it) while this screen is open.
    /// Defaults to the same as `draws_over_hud`; chat and the death screen (which
    /// draws its own red scrim) override it.
    fn dims_world(&self) -> bool {
        self.draws_over_hud()
    }

    /// Whether this screen wants the panorama skybox background.
    fn wants_panorama(&self) -> bool {
        false
    }
}

/// What a screen asks the application to do.
pub enum GuiAction {
    SetScreen(Box<dyn GuiScreen>),
    /// Close the screen and return to gameplay (re-captures the cursor).
    CloseScreen,
    StartDemo(crate::game::DemoKind),
    /// Launch a local Paper server and auto-connect once it's ready.
    StartSingleplayer,
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
    /// Apply the 3D-world render-resolution scale (recreates off-screen targets).
    SetRenderScale(f32),
    /// Apply the max chunk render distance (square cull around the camera).
    SetRenderDistance(u32),
    /// Toggle adaptive resolution (auto-scale to hold the GPU frame budget).
    SetAdaptiveResolution(bool),
    /// Toggle smooth lighting (OFF = flat + greedy meshing); re-meshes the world.
    SetSmoothLighting(bool),
    /// Toggle fancy graphics (sky gradient + transparent water).
    SetFancyGraphics(bool),
    /// Set the block-atlas mipmap level (0 = off .. MIPMAP_MAX).
    SetMipmapLevels(u32),
    /// Apply the resolution from settings, resizing the window/swapchain — the
    /// real present-cost lever on high-DPI screens.
    SetResolution,
    /// Apply the fullscreen state from settings.
    SetFullscreen,
    /// Shader-pack toggles: master lighting, sun shadows, specular.
    SetShaders(bool),
    SetShaderShadows(bool),
    SetShaderSpecular(bool),
    SetShaderFog(bool),
    SetShaderBloom(bool),
    SetBrightness(f32),
    SetVignette(bool),
    SetChromatic(bool),
    SetDof(bool),
    SetMotionBlur(bool),
    SetAutoExposure(bool),
    SetClouds(bool),
    SetVolumetricLight(bool),
    /// Reload the block atlas from a resource pack (or `None` for default).
    ReloadResourcePack(Option<std::path::PathBuf>),
    /// Persist the current settings to disk (emitted when options close).
    SaveSettings,
    /// Send a serverbound packet (inventory clicks, window close, …).
    SendPacket(ServerboundPacket),
    /// Open a URL from a chat component's `open_url` clickEvent in the OS browser.
    OpenUrl(String),
    /// Hot-reload all mods from the `mods/` directory (mod-management screen).
    ReloadMods,
    /// Enable or disable a loaded mod by id, persisting the choice.
    SetModEnabled(String, bool),
    /// Open the `mods/` directory in the OS file manager.
    OpenModsFolder,
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
    /// Whether the GPU panorama skybox is being rendered behind this screen.
    pub has_panorama: bool,
    pub settings: &'a Settings,
    pub session_username: Option<&'a str>,
    pub accounts: &'a [AccountEntry],
    /// HUD data when a world session is active (inventory screen, HUD).
    pub hud: Option<&'a HudState<'a>>,
    /// Snapshot of loaded mods for the mod-management screen.
    pub mods: &'a [recraft_ext::ModInfo],
}

/// Mutable application state screens may edit directly during input handling
/// (vanilla screens reach through `mc.*` the same way).
pub struct ScreenCtx<'a> {
    pub game: &'a mut GameState,
    pub settings: &'a mut Settings,
    pub clipboard: Option<&'a mut arboard::Clipboard>,
    /// Modifier keys currently held (for shortcuts like Ctrl/Cmd+V).
    pub modifiers: ModifiersState,
    /// Current cursor position in physical pixels (for slot hit-testing on
    /// keyboard shortcuts like number-keys and Q-drop).
    pub mouse: (f64, f64),
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
pub fn gui_scale(width: i32, height: i32) -> i32 {
    recraft_render::gui_pixel_scale(width.max(1) as u32, height.max(1) as u32) as i32
}

// ─── Shared drawing helpers ──────────────────────────────────────────────────

pub(crate) const TEXT_WHITE: UiColor = UiColor::rgba(255, 255, 255, 255);
pub(crate) const TEXT_GRAY: UiColor = UiColor::rgba(160, 160, 160, 255);
pub(crate) const TEXT_YELLOW: UiColor = UiColor::rgba(255, 255, 85, 255);
pub(crate) const TEXT_GREEN: UiColor = UiColor::rgba(85, 255, 85, 255);

/// Vanilla `drawDefaultBackground`: the tiled dirt texture (tinted gray 64,
/// one repeat per 32 GUI px) on menus, or a translucent scrim over a world.
pub(crate) fn draw_default_background(ui: &mut UiFrame, ctx: &DrawCtx) {
    // In-world overlay screens are dimmed centrally by the world scrim drawn before
    // the HUD (see `draw_world_scrim`), so the HUD stays visible on top. Only the
    // title-screen variant needs the tiled dirt background here.
    if !ctx.in_world {
        let full = UiRect::new(0, 0, ctx.width, ctx.height);
        ui.tiled_image(
            full,
            GuiTexture::OptionsBackground,
            32 * ctx.scale,
            UiColor::rgba(64, 64, 64, 255),
        );
    }
}

/// The dim scrim drawn over the world behind an in-game overlay screen. Drawn
/// before the HUD so the HUD (hotbar / status bars / chat / scoreboard) stays at
/// full brightness on top of it.
pub(crate) fn draw_world_scrim(ui: &mut UiFrame, width: i32, height: i32) {
    ui.rect(UiRect::new(0, 0, width, height), UiColor::rgba(16, 16, 16, 160));
}

/// The item tooltip box (vanilla `GuiUtils.drawHoveringText`): the dark
/// `0xF0100010` fill with a vertical purple-gradient border, anchored to the
/// right/below the cursor and clamped to the screen. `mx`/`my` and all returned
/// geometry are physical pixels (the GUI-px constants are scaled by `scale`).
pub(crate) fn draw_tooltip(
    ui: &mut UiFrame,
    lines: &[String],
    mx: i32,
    my: i32,
    screen_w: i32,
    screen_h: i32,
    scale: i32,
) {
    if lines.is_empty() {
        return;
    }
    const BG: UiColor = UiColor::rgba(16, 0, 16, 240); // 0xF0100010
    const BORDER_TOP: UiColor = UiColor::rgba(80, 0, 255, 80); // 0x505000FF
    const BORDER_BOTTOM: UiColor = UiColor::rgba(40, 0, 127, 80); // 0x5028007F
    const WHITE: UiColor = UiColor::rgba(255, 255, 255, 255);

    let s = scale;
    let text_w = lines
        .iter()
        .map(|l| recraft_render::text_width(l, s))
        .max()
        .unwrap_or(0);
    let n = lines.len() as i32;
    let mut height = 8 * s;
    if n > 1 {
        height += (2 + (n - 1) * 10) * s;
    }
    let mut x = mx + 12 * s;
    let mut y = my - 12 * s;
    if x + text_w > screen_w {
        x -= 28 * s + text_w;
    }
    if y + height + 6 * s > screen_h {
        y = screen_h - height - 6 * s;
    }

    // Dark fill: a center box plus the four 1-px surrounding strips (vanilla
    // draws them separately to inset the border by one pixel).
    grad(ui, x - 3 * s, y - 4 * s, x + text_w + 3 * s, y - 3 * s, BG, BG);
    grad(ui, x - 3 * s, y + height + 3 * s, x + text_w + 3 * s, y + height + 4 * s, BG, BG);
    grad(ui, x - 3 * s, y - 3 * s, x + text_w + 3 * s, y + height + 3 * s, BG, BG);
    grad(ui, x - 4 * s, y - 3 * s, x - 3 * s, y + height + 3 * s, BG, BG);
    grad(ui, x + text_w + 3 * s, y - 3 * s, x + text_w + 4 * s, y + height + 3 * s, BG, BG);
    // Purple border: gradient down the left/right edges, solid top/bottom.
    grad(ui, x - 3 * s, y - 3 * s + s, x - 3 * s + s, y + height + 3 * s - s, BORDER_TOP, BORDER_BOTTOM);
    grad(ui, x + text_w + 2 * s, y - 3 * s + s, x + text_w + 3 * s, y + height + 3 * s - s, BORDER_TOP, BORDER_BOTTOM);
    grad(ui, x - 3 * s, y - 3 * s, x + text_w + 3 * s, y - 3 * s + s, BORDER_TOP, BORDER_TOP);
    grad(ui, x - 3 * s, y + height + 2 * s, x + text_w + 3 * s, y + height + 3 * s, BORDER_BOTTOM, BORDER_BOTTOM);

    let mut line_y = y;
    for (i, line) in lines.iter().enumerate() {
        ui.overlay_text_shadowed(x, line_y, s, WHITE, line.clone());
        if i == 0 {
            line_y += 2 * s;
        }
        line_y += 10 * s;
    }
}

/// Push an overlay gradient given corner coordinates (vanilla
/// `drawGradientRect(left, top, right, bottom, …)`).
fn grad(ui: &mut UiFrame, left: i32, top: i32, right: i32, bottom: i32, c0: UiColor, c1: UiColor) {
    ui.overlay_gradient_rect(UiRect::new(left, top, right - left, bottom - top), c0, c1);
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

/// Vanilla `GuiSlot`: a centered, scrollable list rendered over the dirt
/// background with the top/bottom gradient shadow lines and a right-side
/// scrollbar. Screens compute the geometry, draw their own row content through
/// the per-row rect this hands back, and read the hit-tested rects for input.
pub(crate) struct ListView {
    /// Inner-content left edge (screen px), and the list pixel width.
    pub left: i32,
    pub width: i32,
    /// Vertical bounds of the scrollable viewport (screen px).
    pub top: i32,
    pub bottom: i32,
    /// One row's full height in screen px (vanilla 36 GUI px including padding).
    pub row_height: i32,
    pub scale: i32,
}

impl ListView {
    /// Standard vanilla list geometry: full window width minus margins, from
    /// y=32 GUI px down to `bottom_margin` GUI px above the window bottom.
    pub fn new(ctx: &DrawCtx, list_width_gui: i32, row_height_gui: i32, bottom_margin_gui: i32) -> Self {
        let s = ctx.scale;
        Self {
            left: (ctx.width - list_width_gui * s) / 2,
            width: list_width_gui * s,
            top: 32 * s,
            bottom: ctx.height - bottom_margin_gui * s,
            row_height: row_height_gui * s,
            scale: s,
        }
    }

    /// How many whole rows fit in the viewport.
    pub fn visible_rows(&self) -> i32 {
        ((self.bottom - self.top) / self.row_height).max(1)
    }

    /// The screen-px rect of the `visible_index`-th visible row (0-based from
    /// the top of the viewport), inset by the inter-row gap.
    pub fn row_rect(&self, visible_index: i32) -> UiRect {
        let s = self.scale;
        UiRect::new(
            self.left,
            self.top + visible_index * self.row_height,
            self.width,
            self.row_height - 4 * s,
        )
    }

    /// Draw the vanilla list background: dark dirt tile clipped to the viewport,
    /// then the top and bottom 4-px gradient shadow lines.
    pub fn draw_background(&self, ui: &mut UiFrame) {
        let s = self.scale;
        let viewport = UiRect::new(self.left, self.top, self.width, self.bottom - self.top);
        // Vanilla GuiSlot tints the dirt darker (32,32,32) inside the list.
        ui.tiled_image(viewport, GuiTexture::OptionsBackground, 32 * s, UiColor::rgba(32, 32, 32, 255));
        // Top shadow: opaque black fading down over 4 GUI px.
        ui.gradient_rect(
            UiRect::new(self.left, self.top, self.width, 4 * s),
            UiColor::rgba(0, 0, 0, 255),
            UiColor::rgba(0, 0, 0, 0),
        );
        // Bottom shadow: transparent fading to opaque black over 4 GUI px.
        ui.gradient_rect(
            UiRect::new(self.left, self.bottom - 4 * s, self.width, 4 * s),
            UiColor::rgba(0, 0, 0, 0),
            UiColor::rgba(0, 0, 0, 255),
        );
    }

    /// Draw the vanilla selection box (outer gray + inner black border) around
    /// a row rect.
    pub fn draw_selection(&self, ui: &mut UiFrame, rect: UiRect) {
        let s = self.scale;
        let outer = UiColor::rgba(128, 128, 128, 255);
        let inner = UiColor::rgba(0, 0, 0, 255);
        // Outer gray frame (1 GUI px).
        let o = UiRect::new(rect.x - 2 * s, rect.y - 2 * s, rect.width + 4 * s, rect.height + 4 * s);
        ui.rect(UiRect::new(o.x, o.y, o.width, s), outer);
        ui.rect(UiRect::new(o.x, o.y + o.height - s, o.width, s), outer);
        ui.rect(UiRect::new(o.x, o.y, s, o.height), outer);
        ui.rect(UiRect::new(o.x + o.width - s, o.y, s, o.height), outer);
        // Inner black frame (1 GUI px), just inside the gray.
        let i = UiRect::new(o.x + s, o.y + s, o.width - 2 * s, o.height - 2 * s);
        ui.rect(UiRect::new(i.x, i.y, i.width, s), inner);
        ui.rect(UiRect::new(i.x, i.y + i.height - s, i.width, s), inner);
        ui.rect(UiRect::new(i.x, i.y, s, i.height), inner);
        ui.rect(UiRect::new(i.x + i.width - s, i.y, s, i.height), inner);
    }

    /// Draw the vanilla scrollbar on the right of the list when content
    /// overflows. `scroll`/`max_scroll` are in whole-row units.
    pub fn draw_scrollbar(&self, ui: &mut UiFrame, scroll: i32, max_scroll: i32, total_rows: i32) {
        if max_scroll <= 0 {
            return;
        }
        let s = self.scale;
        let bar_x = self.left + self.width + 2 * s;
        let bar_w = 6 * s;
        let track_h = self.bottom - self.top;
        // Track (black).
        ui.rect(UiRect::new(bar_x, self.top, bar_w, track_h), UiColor::rgba(0, 0, 0, 255));
        // Thumb height proportional to the visible fraction.
        let visible = self.visible_rows();
        let thumb_h = ((track_h * visible / total_rows.max(1)).max(8 * s)).min(track_h);
        let thumb_y = self.top + (track_h - thumb_h) * scroll / max_scroll;
        ui.rect(UiRect::new(bar_x, thumb_y, bar_w, thumb_h), UiColor::rgba(128, 128, 128, 255));
        // Light edge on the top/left of the thumb.
        ui.rect(UiRect::new(bar_x, thumb_y, bar_w - s, thumb_h - s), UiColor::rgba(192, 192, 192, 255));
    }
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
