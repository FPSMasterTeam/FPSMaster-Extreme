//! Account management (Microsoft login, refresh-token entry, account rows).

use recraft_render::{UiColor, UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::main_menu::GuiMainMenu;
use super::widgets::{GuiButton, GuiTextField};
use super::{
    draw_centered_text, draw_default_background, fit_text, DrawCtx, GuiAction, GuiScreen,
    ScreenCtx, TEXT_GRAY, TEXT_GREEN, TEXT_WHITE,
};

#[derive(Default)]
pub struct GuiAccounts {
    buttons: Vec<GuiButton>,
    /// (uuid, use-button, remove-button) per account row, refreshed each draw.
    rows: Vec<(String, GuiButton, GuiButton)>,
}

impl GuiAccounts {
    pub fn new() -> Self {
        Self::default()
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let x = (ctx.width - 200 * s) / 2;
        let bottom = ctx.height - 50 * s;
        self.buttons = vec![
            GuiButton::at_px(x, bottom, 200 * s, s, "Add with Microsoft"),
            GuiButton::at_px(x, bottom + 24 * s, 64 * s, s, "Add Token"),
            GuiButton::at_px(x + 68 * s, bottom + 24 * s, 64 * s, s, "Copy Token"),
            GuiButton::at_px(x + 136 * s, bottom + 24 * s, 64 * s, s, "Back"),
        ];
        self.rows.clear();
        for (index, account) in ctx.accounts.iter().enumerate() {
            let y = 40 * s + index as i32 * 24 * s;
            let row_x = (ctx.width - 260 * s) / 2;
            self.rows.push((
                account.uuid.clone(),
                GuiButton::at_px(row_x + 170 * s, y, 40 * s, s, "Use"),
                GuiButton::at_px(row_x + 214 * s, y, 40 * s, s, "Del"),
            ));
        }
    }
}

impl GuiScreen for GuiAccounts {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 10 * s, s, TEXT_WHITE, "Accounts");

        if ctx.accounts.is_empty() {
            draw_centered_text(
                ui,
                ctx.width,
                ctx.height / 2 - 30 * s,
                s,
                TEXT_GRAY,
                "No accounts yet — add one below.",
            );
        }
        for (index, account) in ctx.accounts.iter().enumerate() {
            let y = 40 * s + index as i32 * 24 * s;
            let row_x = (ctx.width - 260 * s) / 2;
            let name_rect = UiRect::new(row_x, y, 166 * s, 20 * s);
            ui.rect(name_rect, UiColor::rgba(0, 0, 0, 120));
            let label = fit_text(&account.username, name_rect.width - 8 * s, s);
            ui.text_shadowed(
                name_rect.x + 4 * s,
                y + 6 * s,
                s,
                if account.active { TEXT_GREEN } else { TEXT_WHITE },
                if account.active {
                    format!("{label} §7(active)")
                } else {
                    label
                },
            );
            if let Some((_, use_btn, del_btn)) = self.rows.get(index) {
                use_btn.draw(ui, s, ctx.mouse, ctx.mouse_down);
                del_btn.draw(ui, s, ctx.mouse, ctx.mouse_down);
            }
        }

        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        for (uuid, use_btn, del_btn) in &self.rows {
            if use_btn.clicked(x, y) {
                return vec![GuiAction::UseAccount(uuid.clone())];
            }
            if del_btn.clicked(x, y) {
                return vec![GuiAction::RemoveAccount(uuid.clone())];
            }
        }
        if self.buttons.len() < 4 {
            return Vec::new();
        }
        if self.buttons[0].clicked(x, y) {
            return vec![GuiAction::StartMicrosoftLogin];
        }
        if self.buttons[1].clicked(x, y) {
            // Pre-fill the token field from the clipboard, like before.
            let prefill = ctx
                .clipboard
                .as_mut()
                .and_then(|c| c.get_text().ok())
                .map(|t| t.trim().to_owned())
                .unwrap_or_default();
            return vec![GuiAction::SetScreen(Box::new(GuiAddToken::new(prefill)))];
        }
        if self.buttons[2].clicked(x, y) {
            return vec![GuiAction::CopyActiveToken];
        }
        if self.buttons[3].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))];
        }
        Vec::new()
    }

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
        {
            return vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))];
        }
        Vec::new()
    }
}

/// Paste-a-refresh-token screen.
pub struct GuiAddToken {
    token: GuiTextField,
    buttons: Vec<GuiButton>,
}

impl GuiAddToken {
    pub fn new(prefill: String) -> Self {
        Self {
            token: GuiTextField::new(UiRect::new(0, 0, 0, 0), 4096)
                .with_text(prefill)
                .masked(),
            buttons: Vec::new(),
        }
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let field_w = 220 * s;
        let x = (ctx.width - field_w) / 2;
        let top = ctx.height / 4;
        self.token.rect = UiRect::new(x, top + 16 * s, field_w, 16 * s);
        let btn_y = top + 48 * s;
        let bx = (ctx.width - 200 * s) / 2;
        self.buttons = vec![
            GuiButton::at_px(bx, btn_y, 98 * s, s, "Add Account"),
            GuiButton::at_px(bx + 102 * s, btn_y, 98 * s, s, "Cancel"),
        ];
    }

    fn submit(&self) -> Vec<GuiAction> {
        let token = self.token.text.trim().to_owned();
        if token.is_empty() {
            return Vec::new();
        }
        vec![GuiAction::LoginWithToken(token)]
    }
}

impl GuiScreen for GuiAddToken {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        let top = ctx.height / 4;
        draw_centered_text(ui, ctx.width, top - 24 * s, s, TEXT_WHITE, "Add with Refresh Token");
        draw_centered_text(
            ui,
            ctx.width,
            top + 4 * s,
            s,
            TEXT_GRAY,
            "Paste a Microsoft refresh token (Cmd/Ctrl+V on the Accounts screen):",
        );
        self.token.draw(ui, s);
        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        self.token.mouse_clicked(x, y);
        if self.buttons.len() == 2 {
            if self.buttons[0].clicked(x, y) {
                return self.submit();
            }
            if self.buttons[1].clicked(x, y) {
                return vec![GuiAction::SetScreen(Box::new(GuiAccounts::new()))];
            }
        }
        Vec::new()
    }

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed {
            match event.physical_key {
                PhysicalKey::Code(KeyCode::Escape) => {
                    return vec![GuiAction::SetScreen(Box::new(GuiAccounts::new()))];
                }
                PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                    return self.submit();
                }
                _ => {}
            }
        }
        self.token.key_pressed(event);
        Vec::new()
    }
}
