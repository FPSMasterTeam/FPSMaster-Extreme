//! Non-interactive progress screens (connecting / signing in / device code)
//! and the disconnected/error screen.

use std::sync::mpsc::Receiver;

use recraft_render::UiFrame;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::accounts::GuiAccounts;
use super::main_menu::GuiMainMenu;
use super::multiplayer::GuiMultiplayer;
use super::widgets::GuiButton;
use super::{
    draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx,
    TEXT_GRAY, TEXT_WHITE, TEXT_YELLOW,
};

/// Where a screen's "Back" leads.
#[derive(Debug, Clone, Copy)]
pub enum Parent {
    Accounts,
    Multiplayer,
    MainMenu,
}

impl Parent {
    pub fn create(self) -> Box<dyn GuiScreen> {
        match self {
            Parent::Accounts => Box::new(GuiAccounts::new()),
            Parent::Multiplayer => Box::new(GuiMultiplayer::new()),
            Parent::MainMenu => Box::new(GuiMainMenu::new()),
        }
    }
}

/// A title + detail line (auth progress steps, etc.).
pub struct GuiProgress {
    pub title: String,
    pub detail: String,
}

impl GuiProgress {
    pub fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
        }
    }
}

impl GuiScreen for GuiProgress {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, ctx.height / 2 - 16 * s, s, TEXT_WHITE, &self.title);
        draw_centered_text(ui, ctx.width, ctx.height / 2 + 4 * s, s, TEXT_GRAY, &self.detail);
    }
}

/// Connecting/loading screen for a server join; shows chunk progress.
pub struct GuiConnecting {
    pub host: String,
    pub port: u16,
}

impl GuiScreen for GuiConnecting {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        let (title, detail) = if ctx.chunk_count > 0 {
            (
                "Loading world...".to_owned(),
                format!("{} chunks loaded", ctx.chunk_count),
            )
        } else {
            (
                "Connecting to server...".to_owned(),
                format!("{}:{}", self.host, self.port),
            )
        };
        draw_centered_text(ui, ctx.width, ctx.height / 2 - 16 * s, s, TEXT_WHITE, &title);
        draw_centered_text(ui, ctx.width, ctx.height / 2 + 4 * s, s, TEXT_GRAY, &detail);
    }
}

/// Microsoft device-code prompt.
pub struct GuiAuthCode {
    pub user_code: String,
    pub verification_uri: String,
}

impl GuiScreen for GuiAuthCode {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        let mid = ctx.height / 2;
        draw_centered_text(ui, ctx.width, mid - 40 * s, s, TEXT_WHITE, "Microsoft Login");
        draw_centered_text(
            ui,
            ctx.width,
            mid - 20 * s,
            s,
            TEXT_GRAY,
            "Open your browser and go to:",
        );
        draw_centered_text(ui, ctx.width, mid - 8 * s, s, TEXT_WHITE, &self.verification_uri);
        draw_centered_text(ui, ctx.width, mid + 8 * s, s, TEXT_GRAY, "Enter code:");
        draw_centered_text(ui, ctx.width, mid + 20 * s, 2 * s, TEXT_YELLOW, &self.user_code);
        draw_centered_text(
            ui,
            ctx.width,
            mid + 44 * s,
            s,
            TEXT_GRAY,
            "Waiting for login... ESC to cancel",
        );
    }

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
        {
            return vec![GuiAction::SetScreen(Box::new(GuiAccounts::new()))];
        }
        Vec::new()
    }
}

/// Connection failure / kick / auth error with a back button.
pub struct GuiDisconnected {
    pub title: String,
    pub message: String,
    pub parent: Parent,
    buttons: Vec<GuiButton>,
}

impl GuiDisconnected {
    pub fn new(title: impl Into<String>, message: impl Into<String>, parent: Parent) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            parent,
            buttons: Vec::new(),
        }
    }
}

impl GuiScreen for GuiDisconnected {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        let s = ctx.scale;
        self.buttons = vec![GuiButton::at_px(
            (ctx.width - 200 * s) / 2,
            ctx.height / 2 + 24 * s,
            200 * s,
            s,
            "Back",
        )];
        draw_default_background(ui, ctx);
        draw_centered_text(ui, ctx.width, ctx.height / 2 - 30 * s, s, TEXT_WHITE, &self.title);
        let message = super::fit_text(&self.message, ctx.width - 16 * s, s);
        draw_centered_text(ui, ctx.width, ctx.height / 2 - 8 * s, s, TEXT_GRAY, &message);
        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.buttons.first().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::SetScreen(self.parent.create())];
        }
        Vec::new()
    }

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed
            && matches!(
                event.physical_key,
                PhysicalKey::Code(KeyCode::Escape) | PhysicalKey::Code(KeyCode::Enter)
            )
        {
            return vec![GuiAction::SetScreen(self.parent.create())];
        }
        Vec::new()
    }
}

/// Waiting for the local Paper server to accept connections.
pub struct GuiStartingServer {
    ready_rx: Receiver<bool>,
    port: u16,
    cancel: Option<GuiButton>,
}

impl GuiStartingServer {
    pub fn new(ready_rx: Receiver<bool>, port: u16) -> Self {
        Self {
            ready_rx,
            port,
            cancel: None,
        }
    }
}

impl GuiScreen for GuiStartingServer {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.cancel.as_ref().is_some_and(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        let s = ctx.scale;
        let cancel = GuiButton::at_px(
            (ctx.width - 200 * s) / 2,
            ctx.height / 2 + 24 * s,
            200 * s,
            s,
            "Cancel",
        );
        draw_default_background(ui, ctx);
        draw_centered_text(
            ui,
            ctx.width,
            ctx.height / 2 - 16 * s,
            s,
            TEXT_WHITE,
            "Starting local server...",
        );
        draw_centered_text(
            ui,
            ctx.width,
            ctx.height / 2 + 4 * s,
            s,
            TEXT_GRAY,
            "Waiting for server to start",
        );
        cancel.draw(ui, s, ctx.mouse, ctx.mouse_down);
        self.cancel = Some(cancel);
    }

    fn update(&mut self, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        match self.ready_rx.try_recv() {
            Ok(true) => vec![GuiAction::Connect {
                host: "localhost".into(),
                port: self.port,
            }],
            Ok(false) => vec![GuiAction::SetScreen(Box::new(GuiDisconnected::new(
                "Failed to start server",
                "Server did not respond within 60 seconds",
                Parent::MainMenu,
            )))],
            Err(_) => Vec::new(),
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.cancel.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::QuitToTitle];
        }
        Vec::new()
    }

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
        {
            return vec![GuiAction::QuitToTitle];
        }
        Vec::new()
    }
}
