//! Account management (Microsoft login, refresh-token entry, account rows).

use recraft_render::{text_width, UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::text_input::TextInput;

use super::main_menu::GuiMainMenu;
use super::widgets::{GuiButton, GuiTextField};
use super::{
    draw_centered_text, draw_default_background, fit_text, DrawCtx, GuiAction, GuiScreen, ListView,
    ScreenCtx, TEXT_GRAY, TEXT_GREEN, TEXT_WHITE,
};

/// Account row height in GUI px, mirroring the server list's two-line rows.
const ROW_HEIGHT_GUI: i32 = 36;
const LIST_WIDTH_GUI: i32 = 300;

#[derive(Default)]
pub struct GuiAccounts {
    buttons: Vec<GuiButton>,
    selected: Option<usize>,
    scroll: i32,
    /// (account-index, row-rect) per visible row, refreshed each draw.
    row_rects: Vec<(usize, UiRect)>,
    /// The selected account's uuid, captured at draw time so input handling
    /// (which only has `ScreenCtx`) can resolve it without the account list.
    selected_uuid: Option<String>,
}

impl GuiAccounts {
    pub fn new() -> Self {
        Self::default()
    }

    fn list_view(&self, ctx: &DrawCtx) -> ListView {
        ListView::new(ctx, LIST_WIDTH_GUI, ROW_HEIGHT_GUI, 52)
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        // Bottom action row, styled like the server list's button strip.
        let half = 98 * s;
        let row = ctx.height - 28 * s;
        let x = (ctx.width - 3 * half - 2 * 4 * s) / 2;
        let step = half + 4 * s;
        let has_selection = self
            .selected
            .is_some_and(|index| index < ctx.accounts.len());
        self.buttons = vec![
            GuiButton::at_px(x, row, half, s, "Use Account").disabled(!has_selection),
            GuiButton::at_px(x + step, row, half, s, "Add with Microsoft"),
            GuiButton::at_px(x + 2 * step, row, half, s, "Add Token"),
            // Second row: delete / copy token / back.
            GuiButton::at_px(x, row - 24 * s, half, s, "Delete").disabled(!has_selection),
            GuiButton::at_px(x + step, row - 24 * s, half, s, "Copy Token"),
            GuiButton::at_px(x + 2 * step, row - 24 * s, half, s, "Back"),
        ];
    }

}

impl GuiScreen for GuiAccounts {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 16 * s, s, TEXT_WHITE, "Accounts");

        let list = self.list_view(ctx);
        list.draw_background(ui);

        self.row_rects.clear();
        let total = ctx.accounts.len() as i32;
        let visible = list.visible_rows();
        let max_scroll = (total - visible).max(0);
        self.scroll = self.scroll.clamp(0, max_scroll);

        if ctx.accounts.is_empty() {
            draw_centered_text(
                ui,
                ctx.width,
                (list.top + list.bottom) / 2 - 4 * s,
                s,
                TEXT_GRAY,
                "No accounts yet — add one below.",
            );
        }

        for (visible_index, index) in (self.scroll..total).take(visible as usize).enumerate() {
            let index = index as usize;
            let account = &ctx.accounts[index];
            let rect = list.row_rect(visible_index as i32);
            self.row_rects.push((index, rect));

            if self.selected == Some(index) {
                list.draw_selection(ui, rect);
            }

            let pad = 3 * s;
            // Username (green if this is the signed-in account).
            let name = fit_text(&account.username, rect.width - 90 * s, s);
            ui.text_shadowed(
                rect.x + pad,
                rect.y + pad,
                s,
                if account.active { TEXT_GREEN } else { TEXT_WHITE },
                name,
            );
            // Active tag, right-aligned on the first line.
            if account.active {
                let tag = "§a(in use)";
                let tw = text_width(tag, s);
                ui.text_shadowed(rect.x + rect.width - tw - pad, rect.y + pad, s, TEXT_GREEN, tag);
            }
            // Second line: the UUID, dimmed.
            let uuid = fit_text(&format!("§8{}", account.uuid), rect.width - 2 * pad, s);
            ui.text_shadowed(rect.x + pad, rect.y + pad + 12 * s, s, TEXT_GRAY, uuid);
        }

        list.draw_scrollbar(ui, self.scroll, max_scroll, total);

        // Capture the selected account's uuid for input handling.
        self.selected_uuid = self
            .selected
            .and_then(|i| ctx.accounts.get(i))
            .map(|a| a.uuid.clone());

        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        for (index, rect) in &self.row_rects {
            if rect.contains(x, y) {
                self.selected = Some(*index);
                return Vec::new();
            }
        }
        if self.buttons.len() < 6 {
            return Vec::new();
        }
        if self.buttons[0].clicked(x, y) {
            if let Some(uuid) = self.selected_uuid.clone() {
                return vec![GuiAction::UseAccount(uuid)];
            }
            return Vec::new();
        }
        if self.buttons[1].clicked(x, y) {
            return vec![GuiAction::StartMicrosoftLogin];
        }
        if self.buttons[2].clicked(x, y) {
            // Pre-fill the token field from the clipboard, like before.
            let prefill = ctx
                .clipboard
                .as_mut()
                .and_then(|c| c.get_text().ok())
                .map(|t| t.trim().to_owned())
                .unwrap_or_default();
            return vec![GuiAction::SetScreen(Box::new(GuiAddToken::new(prefill)))];
        }
        if self.buttons[3].clicked(x, y) {
            if let Some(uuid) = self.selected_uuid.take() {
                self.selected = None;
                return vec![GuiAction::RemoveAccount(uuid)];
            }
            return Vec::new();
        }
        if self.buttons[4].clicked(x, y) {
            return vec![GuiAction::CopyActiveToken];
        }
        if self.buttons[5].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))];
        }
        Vec::new()
    }

    fn mouse_scrolled(&mut self, delta: f32) {
        self.scroll -= delta.signum() as i32;
        self.scroll = self.scroll.max(0);
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
        let token = self.token.text().trim().to_owned();
        if token.is_empty() {
            return Vec::new();
        }
        vec![GuiAction::LoginWithToken(token)]
    }
}

impl GuiScreen for GuiAddToken {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

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

    fn key_pressed(&mut self, event: &KeyEvent, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
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
        self.token
            .key_pressed(event, ctx.modifiers, ctx.clipboard.as_deref_mut());
        Vec::new()
    }

    fn focused_text_input(&mut self) -> Option<&mut TextInput> {
        self.token.focused_input()
    }
}
