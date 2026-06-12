use std::time::Instant;

use recraft_protocol::v1_8_9::packets::SlotItem;
use recraft_render::{text_width, GuiTexture, UiColor, UiFrame, UiRect};
use winit::dpi::PhysicalSize;

use crate::chat::{self, ChatState};
use crate::scoreboard::Scoreboard;

#[derive(Debug, Clone)]
pub enum AppScreen {
    MainMenu,
    /// Account-management screen (add via Microsoft / refresh token, copy token).
    Accounts,
    /// Paste-a-refresh-token screen.
    AddAccountToken {
        /// The token being entered (typed or pasted).
        input: String,
    },
    /// Waiting for Microsoft device-code login to complete.
    Authenticating {
        user_code: String,
        verification_uri: String,
    },
    /// In-progress auth step after the code/token is accepted (Xbox/XSTS/…).
    AuthProgress {
        message: String,
    },
    /// Server-address input screen reached from MULTIPLAYER.
    ServerSelect {
        /// Current typed address (host or host:port).
        input: String,
    },
    Connecting {
        host: String,
        port: u16,
    },
    LoadingWorld {
        host: String,
        port: u16,
    },
    InGame,
    /// Chat box open over the running game (T or '/'); the world keeps
    /// ticking, movement keys are released, the cursor is visible.
    Chat {
        input: String,
    },
    /// ESC pause menu shown over the (frozen) world with the cursor released.
    Paused,
    /// Game-settings sub-screen reached from the pause menu.
    Settings,
    /// Inventory ("E") screen shown over the world with the cursor released.
    Inventory,
    /// Death screen with a respawn button, shown when health reaches 0.
    Dead,
    Error {
        message: String,
    },
}

/// A saved account row for the accounts screen.
#[derive(Debug, Clone)]
pub struct AccountEntry {
    pub username: String,
    /// Whether this is the currently signed-in account.
    pub active: bool,
}

/// HUD/inventory state passed to `build_ui` for the in-game overlays.
#[derive(Debug, Clone, Copy)]
pub struct HudState<'a> {
    pub health: f32,
    pub food: i32,
    pub xp_bar: f32,
    pub xp_level: i32,
    pub selected_slot: i32,
    pub hotbar: &'a [Option<SlotItem>],
    pub inventory: &'a [Option<SlotItem>],
    pub chat: &'a ChatState,
    pub scoreboard: &'a Scoreboard,
}

/// User-adjustable options edited from the in-game settings screen.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Mouse sensitivity slider position in 0..=1 (0.5 == vanilla default).
    pub sensitivity: f32,
    /// Whether vertical sync (Fifo present mode) is enabled.
    pub vsync: bool,
    /// Frame-rate cap; `FPS_MAX` means unlimited.
    pub fps_cap: u32,
}

const FPS_MIN: u32 = 30;
const FPS_MAX: u32 = 260;
const FPS_STEP: u32 = 10;

impl Default for Settings {
    fn default() -> Self {
        Self {
            sensitivity: 0.5,
            vsync: true,
            fps_cap: 120,
        }
    }
}

impl Settings {
    /// Degrees of view rotation per pixel of mouse motion. Reproduces the
    /// vanilla curve so 0.5 maps to the long-standing 0.15 default.
    pub fn mouse_factor(self) -> f32 {
        let f = self.sensitivity * 0.6 + 0.2;
        f * f * f * 8.0 * 0.15
    }

    /// Sensitivity shown to the player as a 0..=200% value, vanilla-style.
    pub fn sensitivity_percent(self) -> f32 {
        self.sensitivity * 200.0
    }

    /// The active frame cap, or `None` when the slider is at "unlimited".
    pub fn fps_limit(self) -> Option<u32> {
        if self.fps_cap >= FPS_MAX {
            None
        } else {
            Some(self.fps_cap)
        }
    }

    pub fn fps_label(self) -> String {
        match self.fps_limit() {
            None => "UNLIMITED".to_owned(),
            Some(cap) => format!("{cap} FPS"),
        }
    }

    /// FPS slider fill fraction in 0..=1.
    fn fps_fraction(self) -> f32 {
        (self.fps_cap - FPS_MIN) as f32 / (FPS_MAX - FPS_MIN) as f32
    }

    pub fn set_sensitivity_from01(&mut self, value: f32) {
        self.sensitivity = value.clamp(0.0, 1.0);
    }

    pub fn set_fps_from01(&mut self, value: f32) {
        let span = (FPS_MAX - FPS_MIN) as f32;
        let raw = FPS_MIN as f32 + value.clamp(0.0, 1.0) * span;
        let stepped = (raw / FPS_STEP as f32).round() as u32 * FPS_STEP;
        self.fps_cap = stepped.clamp(FPS_MIN, FPS_MAX);
    }
}

// ─── Button / control layout structs ────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct MenuButtons {
    pub login: UiRect,
    pub demo: UiRect,
    pub multiplayer: UiRect,
    pub quit: UiRect,
}

#[derive(Debug, Clone, Copy)]
pub struct ServerSelectButtons {
    pub join: UiRect,
    pub back: UiRect,
    /// The text-field rectangle (for click-hit-testing / rendering).
    pub input_field: UiRect,
}

/// Per-account row buttons on the accounts screen.
#[derive(Debug, Clone, Copy)]
pub struct AccountRow {
    pub use_btn: UiRect,
    pub remove_btn: UiRect,
}

/// Accounts-screen buttons: the bottom actions plus one row per account.
#[derive(Debug, Clone)]
pub struct AccountButtons {
    pub add_microsoft: UiRect,
    pub add_token: UiRect,
    /// Copy the latest (active) refresh token to the clipboard.
    pub copy_token: UiRect,
    pub back: UiRect,
    pub rows: Vec<AccountRow>,
}

#[derive(Debug, Clone, Copy)]
pub struct AddTokenButtons {
    pub add: UiRect,
    pub back: UiRect,
    pub input_field: UiRect,
}

#[derive(Debug, Clone, Copy)]
pub struct ErrorButtons {
    pub back: UiRect,
}

#[derive(Debug, Clone, Copy)]
pub struct PauseButtons {
    pub resume: UiRect,
    pub settings: UiRect,
    pub quit: UiRect,
}

#[derive(Debug, Clone, Copy)]
pub struct DeadButtons {
    pub respawn: UiRect,
    pub title: UiRect,
}

/// Identifies which settings slider a click/drag is targeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSlider {
    Sensitivity,
    FpsCap,
}

#[derive(Debug, Clone, Copy)]
pub struct SettingsControls {
    /// Sensitivity slider track.
    pub sensitivity: UiRect,
    /// VSync toggle button.
    pub vsync: UiRect,
    /// Frame-rate-cap slider track.
    pub fps_cap: UiRect,
    /// "Done" button returning to the pause menu.
    pub done: UiRect,
}

#[derive(Debug)]
pub struct FpsCounter {
    frames: u32,
    last_sample: Instant,
    fps: f32,
}

impl FpsCounter {
    pub fn new(now: Instant) -> Self {
        Self {
            frames: 0,
            last_sample: now,
            fps: 0.0,
        }
    }

    pub fn tick(&mut self, now: Instant) {
        self.frames += 1;
        let elapsed = (now - self.last_sample).as_secs_f32();
        if elapsed >= 0.5 {
            self.fps = self.frames as f32 / elapsed;
            self.frames = 0;
            self.last_sample = now;
        }
    }

    pub fn fps(&self) -> f32 {
        self.fps
    }
}

pub fn build_ui(
    screen: &AppScreen,
    size: PhysicalSize<u32>,
    fps: f32,
    chunk_count: usize,
    settings: &Settings,
    hud: HudState,
    // The signed-in username, if any.
    session_username: Option<&str>,
    // Saved accounts (for the accounts screen).
    accounts: &[AccountEntry],
) -> UiFrame {
    let mut ui = UiFrame::new();
    let width = size.width as i32;
    let height = size.height as i32;

    match screen {
        AppScreen::MainMenu => draw_main_menu(&mut ui, width, height, session_username),
        AppScreen::Accounts => draw_accounts(&mut ui, width, height, accounts),
        AppScreen::AddAccountToken { input } => draw_add_token(&mut ui, width, height, input),
        AppScreen::Authenticating {
            user_code,
            verification_uri,
        } => draw_authenticating(&mut ui, width, height, user_code, verification_uri),
        AppScreen::AuthProgress { message } => {
            draw_loading(&mut ui, width, height, "SIGNING IN", message)
        }
        AppScreen::ServerSelect { input } => draw_server_select(&mut ui, width, height, input),
        AppScreen::Connecting { host, port } => draw_loading(
            &mut ui,
            width,
            height,
            "CONNECTING",
            &format!("{}:{}", host, port),
        ),
        AppScreen::LoadingWorld { host, port } => draw_loading(
            &mut ui,
            width,
            height,
            "LOADING WORLD",
            &format!("{}:{}  ·  {} chunks loaded", host, port, chunk_count),
        ),
        AppScreen::InGame => draw_game_hud(&mut ui, width, height, fps, chunk_count, &hud, None),
        AppScreen::Chat { input } => {
            draw_game_hud(&mut ui, width, height, fps, chunk_count, &hud, Some(input))
        }
        AppScreen::Paused => draw_pause_menu(&mut ui, width, height),
        AppScreen::Settings => draw_settings(&mut ui, width, height, settings),
        AppScreen::Inventory => {
            // Keep the in-game HUD visible behind the inventory window.
            draw_game_hud(&mut ui, width, height, fps, chunk_count, &hud, None);
            draw_inventory(&mut ui, width, height, &hud);
        }
        AppScreen::Dead => draw_death_screen(&mut ui, width, height),
        AppScreen::Error { message } => draw_error(&mut ui, width, height, message),
    }

    // The in-game HUD (and inventory, which draws it too) already include the
    // FPS panel; everything else gets it added here.
    if !matches!(
        screen,
        AppScreen::InGame | AppScreen::Inventory | AppScreen::Chat { .. }
    ) {
        draw_fps_panel(&mut ui, fps, chunk_count);
    }

    ui
}

pub fn menu_buttons(size: PhysicalSize<u32>) -> MenuButtons {
    let layout = menu_layout(size.width as i32, size.height as i32, 4);
    MenuButtons {
        login: UiRect::new(
            layout.button_x,
            layout.button_y,
            layout.button_width,
            BUTTON_HEIGHT,
        ),
        demo: UiRect::new(
            layout.button_x,
            layout.button_y + BUTTON_STEP,
            layout.button_width,
            BUTTON_HEIGHT,
        ),
        multiplayer: UiRect::new(
            layout.button_x,
            layout.button_y + BUTTON_STEP * 2,
            layout.button_width,
            BUTTON_HEIGHT,
        ),
        quit: UiRect::new(
            layout.button_x,
            layout.button_y + BUTTON_STEP * 3,
            layout.button_width,
            BUTTON_HEIGHT,
        ),
    }
}

pub fn server_select_buttons(size: PhysicalSize<u32>) -> ServerSelectButtons {
    let layout = centered_panel_layout(size.width as i32, size.height as i32, 560, 340);
    let content_x = layout.rect.x + 48;
    let content_w = layout.rect.width - 96;
    let field_y = layout.rect.y + 120;
    let btn_y = field_y + CONTROL_HEIGHT + 24;
    let half = (content_w - 16) / 2;
    ServerSelectButtons {
        input_field: UiRect::new(content_x, field_y, content_w, CONTROL_HEIGHT),
        join: UiRect::new(content_x, btn_y, half, BUTTON_HEIGHT),
        back: UiRect::new(content_x + half + 16, btn_y, half, BUTTON_HEIGHT),
    }
}

pub fn account_buttons(size: PhysicalSize<u32>, account_count: usize) -> AccountButtons {
    let layout = centered_panel_layout(size.width as i32, size.height as i32, 580, 460);
    let content_x = layout.rect.x + 36;
    let content_w = layout.rect.width - 72;
    let row_h = 36;
    let list_y = layout.rect.y + 78;
    // One row per account: [username area] [USE] [REMOVE].
    let btn_w = 84;
    let mut rows = Vec::with_capacity(account_count);
    for i in 0..account_count {
        let y = list_y + i as i32 * (row_h + 6);
        rows.push(AccountRow {
            use_btn: UiRect::new(content_x + content_w - btn_w * 2 - 8, y, btn_w, row_h),
            remove_btn: UiRect::new(content_x + content_w - btn_w, y, btn_w, row_h),
        });
    }
    // Bottom action row.
    let bottom_y = layout.rect.y + layout.rect.height - BUTTON_HEIGHT - 24;
    let third = (content_w - 24) / 3;
    AccountButtons {
        add_microsoft: UiRect::new(content_x, bottom_y - BUTTON_STEP, content_w, BUTTON_HEIGHT),
        add_token: UiRect::new(content_x, bottom_y, third, BUTTON_HEIGHT),
        copy_token: UiRect::new(content_x + third + 12, bottom_y, third, BUTTON_HEIGHT),
        back: UiRect::new(content_x + 2 * (third + 12), bottom_y, third, BUTTON_HEIGHT),
        rows,
    }
}

pub fn add_token_buttons(size: PhysicalSize<u32>) -> AddTokenButtons {
    let layout = centered_panel_layout(size.width as i32, size.height as i32, 620, 320);
    let content_x = layout.rect.x + 48;
    let content_w = layout.rect.width - 96;
    let field_y = layout.rect.y + 120;
    let btn_y = field_y + CONTROL_HEIGHT + 24;
    let half = (content_w - 16) / 2;
    AddTokenButtons {
        input_field: UiRect::new(content_x, field_y, content_w, CONTROL_HEIGHT),
        add: UiRect::new(content_x, btn_y, half, BUTTON_HEIGHT),
        back: UiRect::new(content_x + half + 16, btn_y, half, BUTTON_HEIGHT),
    }
}

pub fn error_buttons(size: PhysicalSize<u32>) -> ErrorButtons {
    let layout = centered_panel_layout(size.width as i32, size.height as i32, 520, 220);
    ErrorButtons {
        back: UiRect::new(
            layout.rect.x + 48,
            layout.rect.y + layout.rect.height - 64,
            layout.rect.width - 96,
            BUTTON_HEIGHT,
        ),
    }
}

pub fn pause_buttons(size: PhysicalSize<u32>) -> PauseButtons {
    let layout = menu_layout(size.width as i32, size.height as i32, 3);
    PauseButtons {
        resume: UiRect::new(
            layout.button_x,
            layout.button_y,
            layout.button_width,
            BUTTON_HEIGHT,
        ),
        settings: UiRect::new(
            layout.button_x,
            layout.button_y + BUTTON_STEP,
            layout.button_width,
            BUTTON_HEIGHT,
        ),
        quit: UiRect::new(
            layout.button_x,
            layout.button_y + BUTTON_STEP * 2,
            layout.button_width,
            BUTTON_HEIGHT,
        ),
    }
}

pub fn dead_buttons(size: PhysicalSize<u32>) -> DeadButtons {
    let layout = centered_panel_layout(size.width as i32, size.height as i32, 420, 220);
    let button_width = (layout.rect.width - 96).max(220);
    let button_x = layout.rect.x + (layout.rect.width - button_width) / 2;
    let button_y = layout.rect.y + 96;
    DeadButtons {
        respawn: UiRect::new(button_x, button_y, button_width, BUTTON_HEIGHT),
        title: UiRect::new(
            button_x,
            button_y + BUTTON_STEP,
            button_width,
            BUTTON_HEIGHT,
        ),
    }
}

pub fn settings_controls(size: PhysicalSize<u32>) -> SettingsControls {
    let layout = centered_panel_layout(size.width as i32, size.height as i32, 560, 440);
    let content_x = layout.rect.x + 40;
    let content_width = layout.rect.width - 80;
    // Each row reserves space for a label above its control.
    let row = |index: i32| layout.rect.y + 100 + index * ROW_STEP + LABEL_GAP;
    SettingsControls {
        sensitivity: UiRect::new(content_x, row(0), content_width, CONTROL_HEIGHT),
        vsync: UiRect::new(content_x, row(1), content_width, CONTROL_HEIGHT),
        fps_cap: UiRect::new(content_x, row(2), content_width, CONTROL_HEIGHT),
        done: UiRect::new(
            content_x,
            layout.rect.y + layout.rect.height - BUTTON_HEIGHT - 28,
            content_width,
            BUTTON_HEIGHT,
        ),
    }
}

/// Map a horizontal cursor position over a slider `track` to a 0..=1 fraction.
pub fn slider_fraction(track: UiRect, cursor_x: f64) -> f32 {
    if track.width <= 0 {
        return 0.0;
    }
    (((cursor_x - track.x as f64) / track.width as f64) as f32).clamp(0.0, 1.0)
}

// ─── Screen draw functions ───────────────────────────────────────────────────

fn draw_pause_menu(ui: &mut UiFrame, width: i32, height: i32) {
    draw_screen_scrim(ui, width, height);
    let layout = menu_layout(width, height, 3);
    draw_panel(ui, layout.rect);

    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 32, layout.rect.width, 32),
        4,
        WHITE,
        "GAME PAUSED",
    );

    let buttons = pause_buttons(PhysicalSize::new(width as u32, height as u32));
    draw_button(ui, buttons.resume, "RESUME");
    draw_button(ui, buttons.settings, "GAME SETTINGS");
    draw_button(ui, buttons.quit, "QUIT TO TITLE");

    ui.text_centered(
        UiRect::new(
            layout.rect.x + 16,
            layout.rect.y + layout.rect.height - 38,
            layout.rect.width - 32,
            14,
        ),
        1,
        MUTED,
        "ESC RESUME   CTRL SPRINT   SHIFT SNEAK",
    );
}

fn draw_settings(ui: &mut UiFrame, width: i32, height: i32, settings: &Settings) {
    draw_screen_scrim(ui, width, height);
    let layout = centered_panel_layout(width, height, 560, 440);
    draw_panel(ui, layout.rect);

    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 30, layout.rect.width, 28),
        3,
        WHITE,
        "OPTIONS",
    );

    let controls = settings_controls(PhysicalSize::new(width as u32, height as u32));

    draw_field_label(
        ui,
        controls.sensitivity,
        "SENSITIVITY",
        &format!("{:.0}%", settings.sensitivity_percent()),
    );
    draw_slider(ui, controls.sensitivity, settings.sensitivity);

    draw_field_label(ui, controls.vsync, "VERTICAL SYNC", "");
    draw_button(
        ui,
        controls.vsync,
        if settings.vsync { "ON" } else { "OFF" },
    );

    draw_field_label(ui, controls.fps_cap, "MAX FRAMERATE", &settings.fps_label());
    draw_slider(ui, controls.fps_cap, settings.fps_fraction());

    draw_button(ui, controls.done, "DONE");
}

fn draw_main_menu(ui: &mut UiFrame, width: i32, height: i32, session_username: Option<&str>) {
    draw_screen_scrim(ui, width, height);
    let layout = menu_layout(width, height, 4);
    draw_panel(ui, layout.rect);

    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 28, layout.rect.width, 32),
        4,
        WHITE,
        "RECRAFT",
    );
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 76, layout.rect.width, 20),
        2,
        MUTED,
        "RUST NATIVE MINECRAFT 1.8.9 CLIENT",
    );

    // Account status line
    let account_label = match session_username {
        Some(name) => format!("Signed in as: {name}"),
        None => "Not signed in".to_owned(),
    };
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 106, layout.rect.width, 16),
        1,
        if session_username.is_some() {
            ACCENT
        } else {
            MUTED
        },
        &account_label,
    );

    let buttons = menu_buttons(PhysicalSize::new(width as u32, height as u32));
    let login_label = if session_username.is_some() {
        "ACCOUNTS (SIGNED IN)"
    } else {
        "ACCOUNTS"
    };
    draw_button_colored(ui, buttons.login, login_label, MS_BLUE);
    draw_button(ui, buttons.demo, "SINGLEPLAYER DEMO");
    draw_button(ui, buttons.multiplayer, "MULTIPLAYER");
    draw_button(ui, buttons.quit, "QUIT");

    ui.text_centered(
        UiRect::new(
            layout.rect.x + 16,
            layout.rect.y + layout.rect.height - 38,
            layout.rect.width - 32,
            14,
        ),
        1,
        MUTED,
        "WASD MOVE   MOUSE LOOK   ESC RELEASE CURSOR",
    );
}

fn draw_authenticating(
    ui: &mut UiFrame,
    width: i32,
    height: i32,
    user_code: &str,
    verification_uri: &str,
) {
    draw_screen_scrim(ui, width, height);
    let layout = centered_panel_layout(width, height, 560, 280);
    draw_panel(ui, layout.rect);

    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 30, layout.rect.width, 28),
        3,
        WHITE,
        "MICROSOFT LOGIN",
    );
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 80, layout.rect.width, 20),
        2,
        MUTED,
        "Open your browser and go to:",
    );
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 110, layout.rect.width, 20),
        2,
        WHITE,
        verification_uri,
    );
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 148, layout.rect.width, 20),
        2,
        MUTED,
        "Enter code:",
    );
    // Large prominent code display
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 174, layout.rect.width, 32),
        4,
        ACCENT,
        user_code,
    );
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 224, layout.rect.width, 16),
        1,
        MUTED,
        "Waiting for login... (close this window to cancel)",
    );
}

fn draw_server_select(ui: &mut UiFrame, width: i32, height: i32, input: &str) {
    draw_screen_scrim(ui, width, height);
    let layout = centered_panel_layout(width, height, 560, 340);
    draw_panel(ui, layout.rect);

    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 30, layout.rect.width, 28),
        3,
        WHITE,
        "JOIN SERVER",
    );
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 78, layout.rect.width, 18),
        2,
        MUTED,
        "Enter server address (host or host:port):",
    );

    let btns = server_select_buttons(PhysicalSize::new(width as u32, height as u32));

    // Text input field
    ui.rect(btns.input_field, BLACK_120);
    stroke_rect(ui, btns.input_field, WHITE_DIM);
    // Show typed text + blinking caret
    let display = format!("{input}_");
    let text_x = btns.input_field.x + 8;
    let text_y = btns.input_field.y + (btns.input_field.height - 16) / 2;
    ui.text(
        text_x,
        text_y,
        2,
        WHITE,
        fit_text(&display, btns.input_field.width - 16, 2),
    );

    draw_button(ui, btns.join, "JOIN SERVER");
    draw_button(ui, btns.back, "BACK");

    ui.text_centered(
        UiRect::new(
            layout.rect.x + 16,
            layout.rect.y + layout.rect.height - 38,
            layout.rect.width - 32,
            14,
        ),
        1,
        MUTED,
        "ENTER TO JOIN   ESC TO GO BACK",
    );
}

fn draw_accounts(ui: &mut UiFrame, width: i32, height: i32, accounts: &[AccountEntry]) {
    draw_screen_scrim(ui, width, height);
    let layout = centered_panel_layout(width, height, 580, 460);
    draw_panel(ui, layout.rect);
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 26, layout.rect.width, 28),
        3,
        WHITE,
        "ACCOUNTS",
    );

    let size = PhysicalSize::new(width as u32, height as u32);
    let btns = account_buttons(size, accounts.len());

    if accounts.is_empty() {
        ui.text_centered(
            UiRect::new(layout.rect.x, layout.rect.y + 110, layout.rect.width, 18),
            2,
            MUTED,
            "No accounts yet — add one below.",
        );
    }
    let name_x = layout.rect.x + 36;
    for (acc, row) in accounts.iter().zip(&btns.rows) {
        let name_rect = UiRect::new(
            name_x,
            row.use_btn.y,
            row.use_btn.x - name_x - 8,
            row.use_btn.height,
        );
        ui.rect(name_rect, if acc.active { ACCENT } else { BLACK_120 });
        stroke_rect(ui, name_rect, WHITE_DIM);
        let label = if acc.active {
            format!("{} (active)", acc.username)
        } else {
            acc.username.clone()
        };
        ui.text(
            name_x + 10,
            row.use_btn.y + (row.use_btn.height - 16) / 2,
            2,
            WHITE,
            fit_text(&label, name_rect.width - 20, 2),
        );
        draw_button(ui, row.use_btn, "USE");
        draw_button(ui, row.remove_btn, "DEL");
    }

    draw_button(ui, btns.add_microsoft, "ADD WITH MICROSOFT");
    draw_button(ui, btns.add_token, "ADD TOKEN");
    draw_button(ui, btns.copy_token, "COPY TOKEN");
    draw_button(ui, btns.back, "BACK");
}

fn draw_add_token(ui: &mut UiFrame, width: i32, height: i32, input: &str) {
    draw_screen_scrim(ui, width, height);
    let layout = centered_panel_layout(width, height, 620, 320);
    draw_panel(ui, layout.rect);
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 30, layout.rect.width, 28),
        3,
        WHITE,
        "ADD WITH REFRESH TOKEN",
    );
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 78, layout.rect.width, 18),
        2,
        MUTED,
        "Paste a Microsoft refresh token (Cmd/Ctrl+V):",
    );

    let btns = add_token_buttons(PhysicalSize::new(width as u32, height as u32));
    ui.rect(btns.input_field, BLACK_120);
    stroke_rect(ui, btns.input_field, WHITE_DIM);
    // The token is long and sensitive: show a length preview, not the raw value.
    let preview = if input.is_empty() {
        "_".to_owned()
    } else {
        format!("{} characters entered", input.chars().count())
    };
    ui.text(
        btns.input_field.x + 8,
        btns.input_field.y + (btns.input_field.height - 16) / 2,
        2,
        WHITE,
        fit_text(&preview, btns.input_field.width - 16, 2),
    );

    draw_button(ui, btns.add, "ADD ACCOUNT");
    draw_button(ui, btns.back, "BACK");
    ui.text_centered(
        UiRect::new(
            layout.rect.x + 16,
            layout.rect.y + layout.rect.height - 38,
            layout.rect.width - 32,
            14,
        ),
        1,
        MUTED,
        "CMD/CTRL+V PASTE   ENTER ADD   ESC BACK",
    );
}

fn draw_loading(ui: &mut UiFrame, width: i32, height: i32, title: &str, detail: &str) {
    draw_screen_scrim(ui, width, height);
    let layout = centered_panel_layout(width, height, 440, 150);
    draw_panel(ui, layout.rect);
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 34, layout.rect.width, 26),
        3,
        WHITE,
        title,
    );
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 84, layout.rect.width, 18),
        2,
        MUTED,
        detail,
    );
}

fn draw_error(ui: &mut UiFrame, width: i32, height: i32, message: &str) {
    draw_screen_scrim(ui, width, height);
    let layout = centered_panel_layout(width, height, 520, 220);
    draw_panel(ui, layout.rect);
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 34, layout.rect.width, 26),
        3,
        WHITE,
        "CONNECTION FAILED",
    );
    ui.text_centered(
        UiRect::new(
            layout.rect.x + 24,
            layout.rect.y + 86,
            layout.rect.width - 48,
            18,
        ),
        1,
        MUTED,
        fit_text(message, layout.rect.width - 64, 1),
    );
    let buttons = error_buttons(PhysicalSize::new(width as u32, height as u32));
    draw_button(ui, buttons.back, "BACK TO MENU");
}

fn draw_game_hud(
    ui: &mut UiFrame,
    width: i32,
    height: i32,
    fps: f32,
    chunk_count: usize,
    hud: &HudState,
    chat_input: Option<&str>,
) {
    draw_fps_panel(ui, fps, chunk_count);

    let center_x = width / 2;
    let center_y = height / 2;
    ui.rect(UiRect::new(center_x - 8, center_y - 1, 17, 2), WHITE_DIM);
    ui.rect(UiRect::new(center_x - 1, center_y - 8, 2, 17), WHITE_DIM);

    draw_status_bars(ui, width, height, hud);
    draw_hotbar(ui, width, height, hud);
    draw_action_bar(ui, width, height, hud);
    draw_sidebar(ui, width, height, hud);
    draw_chat(ui, width, height, hud, chat_input);
}

/// GUI pixel scale shared by the HUD overlays (hotbar, chat, sidebar).
fn gui_scale(height: i32) -> i32 {
    (height / 240).clamp(2, 4)
}

/// Vanilla chat text color (pure white) with a 0..1 fade multiplied into the
/// alpha; the renderer's font pass parses any `§` codes in the string itself.
fn faded_white(alpha: f32) -> UiColor {
    UiColor::rgba(255, 255, 255, (255.0 * alpha.clamp(0.0, 1.0)) as u8)
}

/// Drop characters from the front until the text fits `max_width` px — keeps
/// the tail of an overflowing chat input visible.
fn trim_to_tail(text: &str, max_width: i32, scale: i32) -> String {
    if text_width(text, scale) <= max_width {
        return text.to_owned();
    }
    for (index, _) in text.char_indices() {
        if text_width(&text[index..], scale) <= max_width {
            return text[index..].to_owned();
        }
    }
    String::new()
}

/// The chat panel: recent lines above the hotbar (fading when closed, full
/// backlog when open) plus the input bar when the chat is open.
fn draw_chat(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState, input: Option<&str>) {
    let scale = gui_scale(height);
    let open = input.is_some();
    let now = Instant::now();

    let line_height = 10 * scale;
    let pad = 2 * scale;
    let chat_width = (CHAT_WIDTH_GUI * scale).min(width - 8 * scale);
    let wrap_width = chat_width - 2 * pad;
    let x = 2 * scale;

    // Input bar pinned to the bottom edge.
    let mut bottom = height - 2 * scale;
    if let Some(input) = input {
        let bar = UiRect::new(
            x,
            bottom - line_height - 2 * pad,
            chat_width,
            line_height + 2 * pad,
        );
        ui.rect(bar, BLACK_170);
        // Show the tail of the input when it overflows the bar.
        let visible = trim_to_tail(&format!("{input}_"), wrap_width, scale);
        ui.text_shadowed(
            bar.x + pad,
            bar.y + pad + scale,
            scale,
            faded_white(1.0),
            visible,
        );
        bottom = bar.y - 2 * scale;
    } else {
        bottom -= 42 * scale; // sit above the hotbar + status rows when closed
    }

    // Wrapped rows, newest at the bottom, capped like vanilla (10 closed / 20 open).
    let max_rows = if open { 20 } else { 10 };
    let mut rows: Vec<(String, f32)> = Vec::new();
    for (text, alpha) in hud.chat.visible_lines(now, open) {
        let wrapped = chat::wrap_legacy(text, wrap_width, scale);
        // Within one message the first wrapped row is drawn topmost.
        for row in wrapped.into_iter().rev() {
            rows.push((row, alpha));
            if rows.len() >= max_rows {
                break;
            }
        }
        if rows.len() >= max_rows {
            break;
        }
    }
    for (row, alpha) in rows {
        let rect = UiRect::new(x, bottom - line_height, chat_width, line_height);
        ui.rect(rect, UiColor::rgba(0, 0, 0, (127.0 * alpha) as u8));
        ui.text_shadowed(rect.x + pad, rect.y + scale, scale, faded_white(alpha), row);
        bottom = rect.y;
    }
}

/// The action-bar text (chat position 2) centered above the hotbar.
fn draw_action_bar(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let Some((text, alpha)) = hud.chat.action_bar(Instant::now()) else {
        return;
    };
    let scale = gui_scale(height);
    let text_w = text_width(text, scale);
    let layout = hotbar_layout(width, height);
    ui.text_shadowed(
        (width - text_w) / 2,
        layout.y0 - 30 * scale,
        scale,
        faded_white(alpha),
        text,
    );
}

/// The scoreboard sidebar on the right edge, vanilla-style: title row, then
/// up to 15 score rows (highest first) with red score numbers.
fn draw_sidebar(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let Some(view) = hud.scoreboard.sidebar_view() else {
        return;
    };
    if view.rows.is_empty() {
        return;
    }
    let scale = gui_scale(height);
    let line_height = 10 * scale;
    let pad = 2 * scale;

    // Width: the widest of the title and each "text ... score" row
    // (text_width already ignores the § codes).
    let mut panel_width = text_width(&view.title, scale);
    for (text, score) in &view.rows {
        let row_w = text_width(text, scale) + 4 * scale + text_width(&score.to_string(), scale);
        panel_width = panel_width.max(row_w);
    }
    let panel_width = (panel_width + 2 * pad).min(width / 2);

    let total_height = (view.rows.len() as i32 + 1) * line_height;
    let x0 = width - panel_width - 2 * scale;
    let mut y = (height - total_height) / 2;

    // Title row (slightly darker, text centered). Vanilla draws the sidebar
    // text without a shadow.
    ui.rect(
        UiRect::new(x0, y, panel_width, line_height),
        UiColor::rgba(0, 0, 0, 96),
    );
    let title_w = text_width(&view.title, scale);
    ui.text(
        x0 + (panel_width - title_w) / 2,
        y + scale,
        scale,
        faded_white(1.0),
        view.title.clone(),
    );
    y += line_height;

    for (text, score) in &view.rows {
        ui.rect(
            UiRect::new(x0, y, panel_width, line_height),
            UiColor::rgba(0, 0, 0, 80),
        );
        ui.text(x0 + pad, y + scale, scale, faded_white(1.0), text.clone());
        let score_text = score.to_string();
        let score_w = text_width(&score_text, scale);
        ui.text(
            x0 + panel_width - score_w - pad,
            y + scale,
            scale,
            SCORE_RED,
            score_text,
        );
        y += line_height;
    }
}

/// Geometry of the GUI-scaled hotbar so both the background blit and the item
/// icons line up.
struct HotbarLayout {
    scale: i32,
    x0: i32,
    y0: i32,
    width: i32,
    height: i32,
}

fn hotbar_layout(width: i32, height: i32) -> HotbarLayout {
    let scale = (height / 240).clamp(2, 4);
    let hotbar_w = HOTBAR_WIDTH * scale;
    let hotbar_h = HOTBAR_HEIGHT * scale;
    HotbarLayout {
        scale,
        x0: (width - hotbar_w) / 2,
        y0: height - hotbar_h - 2 * scale,
        width: hotbar_w,
        height: hotbar_h,
    }
}

/// Draw the vanilla hotbar (gui/widgets.png): the 182×22 background, the 24×22
/// selection box over the active slot, and each hotbar item's icon + count.
fn draw_hotbar(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let layout = hotbar_layout(width, height);
    let (scale, x0, y0) = (layout.scale, layout.x0, layout.y0);

    // Fallback strip so the hotbar still reads if widgets.png is missing.
    ui.rect(UiRect::new(x0, y0, layout.width, layout.height), BLACK_120);
    ui.image(
        UiRect::new(x0, y0, layout.width, layout.height),
        GuiTexture::Widgets,
        0,
        0,
        HOTBAR_WIDTH as u32,
        HOTBAR_HEIGHT as u32,
    );

    for (i, item) in hud.hotbar.iter().enumerate() {
        if let Some(item) = item {
            let cell = UiRect::new(
                x0 + (3 + i as i32 * SLOT_PITCH) * scale,
                y0 + 3 * scale,
                16 * scale,
                16 * scale,
            );
            draw_item_icon(ui, cell, *item, scale.max(2));
        }
    }

    let slot = hud.selected_slot.clamp(0, 8);
    let sel_x = x0 + slot * SLOT_PITCH * scale - scale;
    let sel_y = y0 - scale;
    ui.image(
        UiRect::new(sel_x, sel_y, SELECTOR_SIZE * scale, HOTBAR_HEIGHT * scale),
        GuiTexture::Widgets,
        0,
        HOTBAR_HEIGHT as u32,
        SELECTOR_SIZE as u32,
        HOTBAR_HEIGHT as u32,
    );
}

/// Health hearts, hunger haunches and the experience bar, all blitted from the
/// vanilla `gui/icons.png` sprite sheet (9×9 heart/haunch icons; the 182×5 XP
/// bar). Missing icons.png simply blits nothing.
fn draw_status_bars(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let layout = hotbar_layout(width, height);
    let scale = layout.scale;
    let icon = 9 * scale;
    let step = 8 * scale; // hearts/haunches overlap by 1px → 8px pitch

    // XP bar sits directly above the hotbar; the heart/hunger row above that.
    let xp_h = 5 * scale;
    let xp_y = layout.y0 - xp_h - 2 * scale;
    let row_y = xp_y - icon - scale;

    // Experience bar (full hotbar width) with a centered level number.
    ui.image(
        UiRect::new(layout.x0, xp_y, layout.width, xp_h),
        GuiTexture::Icons,
        0,
        64,
        182,
        5,
    );
    let frac = hud.xp_bar.clamp(0.0, 1.0);
    if frac > 0.0 {
        let fill_w = (layout.width as f32 * frac) as i32;
        let src_w = (182.0 * frac).round().clamp(1.0, 182.0) as u32;
        ui.image(
            UiRect::new(layout.x0, xp_y, fill_w, xp_h),
            GuiTexture::Icons,
            0,
            69,
            src_w,
            5,
        );
    }
    if hud.xp_level > 0 {
        let label = format!("{}", hud.xp_level);
        let lw = text_width(&label, scale);
        ui.text_shadowed(
            layout.x0 + (layout.width - lw) / 2,
            xp_y - 9 * scale,
            scale,
            XP_GREEN,
            label,
        );
    }

    // Health hearts, left-aligned over the left half.
    let hp = hud.health.round().clamp(0.0, 20.0) as i32;
    for i in 0..10 {
        let dst = UiRect::new(layout.x0 + i * step, row_y, icon, icon);
        ui.image(dst, GuiTexture::Icons, 16, 0, 9, 9); // empty container
        match hp - i * 2 {
            n if n >= 2 => ui.image(dst, GuiTexture::Icons, 52, 0, 9, 9), // full
            1 => ui.image(dst, GuiTexture::Icons, 61, 0, 9, 9),           // half
            _ => {}
        }
    }

    // Hunger haunches, right-aligned (drawn right→left like vanilla).
    let food = hud.food.clamp(0, 20);
    let right = layout.x0 + layout.width - icon;
    for i in 0..10 {
        let dst = UiRect::new(right - i * step, row_y, icon, icon);
        ui.image(dst, GuiTexture::Icons, 16, 27, 9, 9); // empty container
        match food - i * 2 {
            n if n >= 2 => ui.image(dst, GuiTexture::Icons, 52, 27, 9, 9), // full
            1 => ui.image(dst, GuiTexture::Icons, 61, 27, 9, 9),           // half
            _ => {}
        }
    }
}

/// Draw the inventory screen: a panel with crafting/armor and the 3×9 main grid
/// plus the 1×9 hotbar row, each occupied slot showing an item swatch + count.
fn draw_inventory(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    draw_screen_scrim(ui, width, height);
    // The vanilla survival inventory window is 176×166; GUI-scale it to the
    // window and place item icons at the texture's slot coordinates.
    let scale = (height / 240).clamp(2, 4);
    let pw = 176 * scale;
    let ph = 166 * scale;
    let px = (width - pw) / 2;
    let py = (height - ph) / 2;
    // Dark fallback panel behind the texture (so it reads if inventory.png is missing).
    ui.rect(UiRect::new(px, py, pw, ph), BLACK_210);
    ui.image(
        UiRect::new(px, py, pw, ph),
        GuiTexture::Inventory,
        0,
        0,
        176,
        166,
    );

    let icon = 16 * scale;
    for (slot, sx, sy) in inventory_slot_positions() {
        if let Some(Some(item)) = hud.inventory.get(slot) {
            let cell = UiRect::new(px + sx * scale, py + sy * scale, icon, icon);
            draw_item_icon(ui, cell, *item, scale.max(2));
        }
    }
}

/// (inventory slot index, texture x, texture y) for each slot of the survival
/// inventory window, in `inventory.png` (176×166) pixel coordinates.
fn inventory_slot_positions() -> Vec<(usize, i32, i32)> {
    let mut slots = Vec::with_capacity(45);
    // Armor (5..9): left column.
    for i in 0..4 {
        slots.push((5 + i, 8, 8 + i as i32 * 18));
    }
    // Crafting 2×2 (1..5) and result (0).
    for (i, (x, y)) in [(98, 18), (116, 18), (98, 36), (116, 36)]
        .into_iter()
        .enumerate()
    {
        slots.push((1 + i, x, y));
    }
    slots.push((0, 154, 28));
    // Main inventory (9..36): 3×9 grid.
    for i in 0..27usize {
        slots.push((9 + i, 8 + (i % 9) as i32 * 18, 84 + (i / 9) as i32 * 18));
    }
    // Hotbar (36..45).
    for i in 0..9usize {
        slots.push((36 + i, 8 + i as i32 * 18, 142));
    }
    slots
}

/// The death overlay with a respawn button.
fn draw_death_screen(ui: &mut UiFrame, width: i32, height: i32) {
    ui.rect(UiRect::new(0, 0, width, height), DEATH_SCRIM);
    let layout = centered_panel_layout(width, height, 420, 220);
    draw_panel(ui, layout.rect);
    ui.text_centered(
        UiRect::new(layout.rect.x, layout.rect.y + 28, layout.rect.width, 40),
        5,
        HEALTH_RED,
        "YOU DIED!",
    );
    let buttons = dead_buttons(PhysicalSize::new(width as u32, height as u32));
    draw_button(ui, buttons.respawn, "RESPAWN");
    draw_button(ui, buttons.title, "QUIT TO TITLE");
}

/// Draw an item's thumbnail (real block texture for block items) plus its stack
/// count in the bottom-right.
fn draw_item_icon(ui: &mut UiFrame, rect: UiRect, item: SlotItem, text_scale: i32) {
    ui.item_icon(rect, item.id);
    if item.count > 1 {
        let label = format!("{}", item.count);
        let w = text_width(&label, text_scale);
        ui.text_shadowed(
            rect.x + rect.width - w - text_scale,
            rect.y + rect.height - 8 * text_scale,
            text_scale,
            faded_white(1.0),
            label,
        );
    }
}

fn draw_fps_panel(ui: &mut UiFrame, fps: f32, chunk_count: usize) {
    let fps_text = format!("FPS {:>3.0}", fps);
    let chunks_text = format!("CHUNKS {}", chunk_count);
    let width = text_width(&fps_text, 2).max(text_width(&chunks_text, 1)) + 24;
    ui.rect(UiRect::new(12, 12, width, 58), BLACK_170);
    ui.text(24, 24, 2, WHITE, fps_text);
    ui.text(24, 50, 1, MUTED, chunks_text);
}

fn draw_screen_scrim(ui: &mut UiFrame, width: i32, height: i32) {
    ui.rect(
        UiRect::new(0, 0, width, height),
        UiColor::rgba(8, 14, 18, 150),
    );
}

fn draw_panel(ui: &mut UiFrame, rect: UiRect) {
    ui.rect(rect, BLACK_210);
    stroke_rect(ui, rect, WHITE_DIM);
}

fn draw_button(ui: &mut UiFrame, rect: UiRect, label: &str) {
    ui.rect(rect, UiColor::rgba(43, 49, 55, 225));
    stroke_rect(ui, rect, WHITE_DIM);
    ui.text_centered(rect, 2, WHITE, fit_text(label, rect.width - 24, 2));
}

fn draw_button_colored(ui: &mut UiFrame, rect: UiRect, label: &str, bg: UiColor) {
    ui.rect(rect, bg);
    stroke_rect(ui, rect, WHITE_DIM);
    ui.text_centered(rect, 2, WHITE, fit_text(label, rect.width - 24, 2));
}

/// Draw a control's left-aligned label and an optional right-aligned value just
/// above the control rect.
fn draw_field_label(ui: &mut UiFrame, control: UiRect, label: &str, value: &str) {
    let y = control.y - LABEL_GAP + 2;
    ui.text(control.x, y, 2, WHITE, label);
    if !value.is_empty() {
        let value_width = text_width(value, 2);
        ui.text(control.x + control.width - value_width, y, 2, MUTED, value);
    }
}

/// Draw a horizontal slider: track, filled portion up to `fraction`, and a knob.
fn draw_slider(ui: &mut UiFrame, track: UiRect, fraction: f32) {
    let fraction = fraction.clamp(0.0, 1.0);
    ui.rect(track, BLACK_120);
    let fill_width = (track.width as f32 * fraction) as i32;
    if fill_width > 0 {
        ui.rect(
            UiRect::new(track.x, track.y, fill_width, track.height),
            ACCENT,
        );
    }
    stroke_rect(ui, track, WHITE_DIM);
    let knob_width = 10;
    let knob_x = track.x + (fill_width - knob_width / 2).clamp(0, track.width - knob_width);
    ui.rect(
        UiRect::new(knob_x, track.y - 3, knob_width, track.height + 6),
        WHITE,
    );
}

fn stroke_rect(ui: &mut UiFrame, rect: UiRect, color: UiColor) {
    ui.rect(UiRect::new(rect.x, rect.y, rect.width, 2), color);
    ui.rect(
        UiRect::new(rect.x, rect.y + rect.height - 2, rect.width, 2),
        color,
    );
    ui.rect(UiRect::new(rect.x, rect.y, 2, rect.height), color);
    ui.rect(
        UiRect::new(rect.x + rect.width - 2, rect.y, 2, rect.height),
        color,
    );
}

#[derive(Debug, Clone, Copy)]
struct Layout {
    rect: UiRect,
    button_x: i32,
    button_y: i32,
    button_width: i32,
}

/// Layout for a centered menu panel sized to accommodate `button_count` buttons.
fn menu_layout(width: i32, height: i32, button_count: i32) -> Layout {
    let panel_h = 200 + button_count * BUTTON_STEP + 40;
    let panel = centered_panel_layout(width, height, 560, panel_h);
    let button_width = (panel.rect.width - 96).max(220);
    let total_button_height = BUTTON_STEP * (button_count - 1) + BUTTON_HEIGHT;
    let button_y = panel.rect.y + panel.rect.height - total_button_height - 52;
    Layout {
        rect: panel.rect,
        button_x: panel.rect.x + (panel.rect.width - button_width) / 2,
        button_y,
        button_width,
    }
}

fn centered_panel_layout(
    width: i32,
    height: i32,
    preferred_width: i32,
    preferred_height: i32,
) -> Layout {
    let margin = 24;
    let panel_width = (width - margin * 2).max(280).min(preferred_width);
    let panel_height = (height - margin * 2).max(180).min(preferred_height);
    let x = (width - panel_width) / 2;
    let y = (height - panel_height) / 2;
    Layout {
        rect: UiRect::new(x, y, panel_width, panel_height),
        button_x: x + 48,
        button_y: y + 120,
        button_width: panel_width - 96,
    }
}

fn fit_text(text: &str, max_width: i32, scale: i32) -> String {
    if text_width(text, scale) <= max_width {
        return text.to_owned();
    }
    let ellipsis = "...";
    let mut out = String::new();
    for ch in text.chars() {
        let candidate = format!("{out}{ch}{ellipsis}");
        if text_width(&candidate, scale) > max_width {
            break;
        }
        out.push(ch);
    }
    out.push_str(ellipsis);
    out
}

// ─── Constants ───────────────────────────────────────────────────────────────

const BUTTON_HEIGHT: i32 = 46;
const BUTTON_STEP: i32 = 58;
// gui/widgets.png hotbar source metrics (pixels).
const HOTBAR_WIDTH: i32 = 182;
const HOTBAR_HEIGHT: i32 = 22;
const SELECTOR_SIZE: i32 = 24;
const SLOT_PITCH: i32 = 20;
/// Chat panel width in GUI pixels (vanilla chat is 320 wide when open).
const CHAT_WIDTH_GUI: i32 = 320;
/// Vertical distance between settings rows (label + control).
const ROW_STEP: i32 = 84;
/// Space reserved above a control for its label.
const LABEL_GAP: i32 = 26;
const CONTROL_HEIGHT: i32 = 28;

const HEALTH_RED: UiColor = UiColor::rgba(212, 68, 60, 255);
/// Sidebar score numbers (vanilla chat color "red").
const SCORE_RED: UiColor = UiColor::rgba(255, 85, 85, 255);
const XP_GREEN: UiColor = UiColor::rgba(126, 232, 31, 255);
const DEATH_SCRIM: UiColor = UiColor::rgba(120, 10, 10, 140);
/// Microsoft-brand blue used on the login button.
const MS_BLUE: UiColor = UiColor::rgba(0, 120, 215, 220);

const WHITE: UiColor = UiColor::rgba(235, 241, 232, 255);
const WHITE_DIM: UiColor = UiColor::rgba(235, 241, 232, 95);
const MUTED: UiColor = UiColor::rgba(176, 190, 181, 255);
const ACCENT: UiColor = UiColor::rgba(96, 142, 108, 235);
const BLACK_120: UiColor = UiColor::rgba(0, 0, 0, 120);
const BLACK_170: UiColor = UiColor::rgba(0, 0, 0, 170);
const BLACK_210: UiColor = UiColor::rgba(0, 0, 0, 210);
