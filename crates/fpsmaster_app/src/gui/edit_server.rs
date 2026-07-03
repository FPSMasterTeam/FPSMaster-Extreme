//! Add/edit a saved server (vanilla `GuiScreenAddServer`) and the direct
//! connect screen (vanilla `GuiScreenServerList`).

use fpsmaster_render::{UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::i18n::tr;
use crate::servers::{self, ServerEntry, ServerList};
use crate::text_input::TextInput;

use super::multiplayer::GuiMultiplayer;
use super::widgets::{GuiButton, GuiTextField};
use super::{draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

pub struct GuiEditServer {
    /// Index into the saved list when editing; None when adding.
    editing: Option<usize>,
    name: GuiTextField,
    address: GuiTextField,
    buttons: Vec<GuiButton>,
}

impl GuiEditServer {
    pub fn add() -> Self {
        Self::with_values(None, &tr("fpsmaster.server.defaultName"), "")
    }

    pub fn edit(index: usize, entry: ServerEntry) -> Self {
        Self::with_values(Some(index), &entry.name, &entry.address)
    }

    fn with_values(editing: Option<usize>, name: &str, address: &str) -> Self {
        let mut name = GuiTextField::new(UiRect::new(0, 0, 0, 0), 64).with_text(name);
        let mut address = GuiTextField::new(UiRect::new(0, 0, 0, 0), 128).with_text(address);
        name.focused = false;
        address.focused = true;
        Self {
            editing,
            name,
            address,
            buttons: Vec::new(),
        }
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let field_w = 200 * s;
        let x = (ctx.width - field_w) / 2;
        let top = ctx.height / 4;
        self.name.rect = UiRect::new(x, top + 12 * s, field_w, 16 * s);
        self.address.rect = UiRect::new(x, top + 48 * s, field_w, 16 * s);
        let done_y = top + 80 * s;
        self.buttons = vec![
            GuiButton::at_px(x, done_y, 98 * s, s, tr("gui.done")),
            GuiButton::at_px(x + 102 * s, done_y, 98 * s, s, tr("gui.cancel")),
        ];
    }

    fn save(&self) -> Vec<GuiAction> {
        let name = self.name.text().trim();
        let address = self.address.text().trim();
        if address.is_empty() {
            return Vec::new();
        }
        let entry = ServerEntry {
            name: if name.is_empty() { address } else { name }.to_owned(),
            address: address.to_owned(),
        };
        let mut list = ServerList::load();
        match self.editing {
            Some(index) if index < list.entries.len() => list.entries[index] = entry,
            _ => list.entries.push(entry),
        }
        list.save();
        vec![GuiAction::SetScreen(Box::new(GuiMultiplayer::new()))]
    }
}

impl GuiScreen for GuiEditServer {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        let top = ctx.height / 4;
        let title = if self.editing.is_some() {
            tr("addServer.title")
        } else {
            tr("fpsmaster.server.add")
        };
        draw_centered_text(ui, ctx.width, top - 24 * s, s, super::TEXT_WHITE, &title);
        ui.text_shadowed(self.name.rect.x, top + 2 * s, s, super::TEXT_GRAY, tr("addServer.enterName"));
        ui.text_shadowed(
            self.address.rect.x,
            top + 38 * s,
            s,
            super::TEXT_GRAY,
            tr("addServer.enterIp"),
        );
        self.name.draw(ui, s);
        self.address.draw(ui, s);
        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        self.name.mouse_clicked(x, y);
        self.address.mouse_clicked(x, y);
        if self.buttons.len() == 2 {
            if self.buttons[0].clicked(x, y) {
                return self.save();
            }
            if self.buttons[1].clicked(x, y) {
                return vec![GuiAction::SetScreen(Box::new(GuiMultiplayer::new()))];
            }
        }
        Vec::new()
    }

    fn key_pressed(&mut self, event: &KeyEvent, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed {
            match event.physical_key {
                PhysicalKey::Code(KeyCode::Escape) => {
                    return vec![GuiAction::SetScreen(Box::new(GuiMultiplayer::new()))];
                }
                PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                    return self.save();
                }
                PhysicalKey::Code(KeyCode::Tab) => {
                    let to_name = self.address.focused;
                    self.name.focused = to_name;
                    self.address.focused = !to_name;
                    return Vec::new();
                }
                _ => {}
            }
        }
        self.name
            .key_pressed(event, ctx.modifiers, ctx.clipboard.as_deref_mut());
        self.address
            .key_pressed(event, ctx.modifiers, ctx.clipboard.as_deref_mut());
        Vec::new()
    }

    fn focused_text_input(&mut self) -> Option<&mut TextInput> {
        if let Some(input) = self.name.focused_input() {
            return Some(input);
        }
        self.address.focused_input()
    }
}

/// Direct connect: a single address field and a join button.
pub struct GuiDirectConnect {
    address: GuiTextField,
    buttons: Vec<GuiButton>,
}

impl GuiDirectConnect {
    pub fn new() -> Self {
        Self {
            address: GuiTextField::new(UiRect::new(0, 0, 0, 0), 128),
            buttons: Vec::new(),
        }
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let field_w = 200 * s;
        let x = (ctx.width - field_w) / 2;
        let top = ctx.height / 4;
        self.address.rect = UiRect::new(x, top + 16 * s, field_w, 16 * s);
        let done_y = top + 48 * s;
        self.buttons = vec![
            GuiButton::at_px(x, done_y, 98 * s, s, tr("selectServer.select")),
            GuiButton::at_px(x + 102 * s, done_y, 98 * s, s, tr("gui.cancel")),
        ];
    }

    fn join(&self) -> Vec<GuiAction> {
        match servers::parse_server_address(self.address.text()) {
            Some((host, port)) => vec![GuiAction::Connect { host, port }],
            None => Vec::new(),
        }
    }
}

impl GuiScreen for GuiDirectConnect {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        let top = ctx.height / 4;
        draw_centered_text(ui, ctx.width, top - 24 * s, s, super::TEXT_WHITE, &tr("selectServer.direct"));
        ui.text_shadowed(
            self.address.rect.x,
            top + 6 * s,
            s,
            super::TEXT_GRAY,
            tr("addServer.enterIp"),
        );
        self.address.draw(ui, s);
        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        self.address.mouse_clicked(x, y);
        if self.buttons.len() == 2 {
            if self.buttons[0].clicked(x, y) {
                return self.join();
            }
            if self.buttons[1].clicked(x, y) {
                return vec![GuiAction::SetScreen(Box::new(GuiMultiplayer::new()))];
            }
        }
        Vec::new()
    }

    fn key_pressed(&mut self, event: &KeyEvent, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed {
            match event.physical_key {
                PhysicalKey::Code(KeyCode::Escape) => {
                    return vec![GuiAction::SetScreen(Box::new(GuiMultiplayer::new()))];
                }
                PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                    return self.join();
                }
                _ => {}
            }
        }
        self.address
            .key_pressed(event, ctx.modifiers, ctx.clipboard.as_deref_mut());
        Vec::new()
    }

    fn focused_text_input(&mut self) -> Option<&mut TextInput> {
        self.address.focused_input()
    }
}
