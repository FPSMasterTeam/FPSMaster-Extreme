//! The server list (vanilla `GuiMultiplayer`): saved servers with live
//! ping/MOTD/player-count rows, selection, double-click join, and the
//! add/edit/delete/refresh/direct-connect actions.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use recraft_render::{text_width, UiColor, UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::servers::{self, PingOutcome, ServerList};

use super::edit_server::{GuiDirectConnect, GuiEditServer};
use super::main_menu::GuiMainMenu;
use super::widgets::GuiButton;
use super::{
    draw_centered_text, draw_default_background, fit_text, DrawCtx, GuiAction, GuiScreen,
    ScreenCtx, TEXT_GRAY, TEXT_WHITE,
};

/// Server row height in GUI px (vanilla rows are 36).
const ROW_HEIGHT_GUI: i32 = 30;
const DOUBLE_CLICK: Duration = Duration::from_millis(350);

pub struct GuiMultiplayer {
    servers: ServerList,
    pings: Vec<Option<PingOutcome>>,
    ping_rx: Option<Receiver<(usize, PingOutcome)>>,
    selected: Option<usize>,
    scroll: i32,
    last_click: Option<(usize, Instant)>,
    row_rects: Vec<(usize, UiRect)>,
    buttons: Vec<GuiButton>,
}

impl GuiMultiplayer {
    pub fn new() -> Self {
        let servers = ServerList::load();
        let mut screen = Self {
            pings: Vec::new(),
            ping_rx: None,
            selected: None,
            scroll: 0,
            last_click: None,
            row_rects: Vec::new(),
            buttons: Vec::new(),
            servers,
        };
        screen.refresh();
        screen
    }

    /// Re-ping every saved server.
    fn refresh(&mut self) {
        self.pings = vec![None; self.servers.entries.len()];
        self.ping_rx = (!self.servers.entries.is_empty())
            .then(|| servers::ping_all(&self.servers.entries));
    }

    fn join_selected(&self) -> Vec<GuiAction> {
        let Some(index) = self.selected else {
            return Vec::new();
        };
        let Some(entry) = self.servers.entries.get(index) else {
            return Vec::new();
        };
        match servers::parse_server_address(&entry.address) {
            Some((host, port)) => vec![GuiAction::Connect { host, port }],
            None => Vec::new(),
        }
    }

    fn visible_rows(&self, ctx: &DrawCtx) -> i32 {
        let s = ctx.scale;
        let list_top = 32 * s;
        let list_bottom = ctx.height - 56 * s;
        ((list_bottom - list_top) / (ROW_HEIGHT_GUI * s)).max(1)
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let half = 98 * s;
        let row1 = ctx.height - 50 * s;
        let row2 = ctx.height - 26 * s;
        let x = (ctx.width - 3 * half - 2 * 4 * s) / 2;
        let step = half + 4 * s;
        let has_selection = self
            .selected
            .is_some_and(|index| index < self.servers.entries.len());
        self.buttons = vec![
            GuiButton::at_px(x, row1, half, s, "Join Server").disabled(!has_selection),
            GuiButton::at_px(x + step, row1, half, s, "Direct Connect"),
            GuiButton::at_px(x + 2 * step, row1, half, s, "Add Server"),
            GuiButton::at_px(x, row2, half, s, "Edit").disabled(!has_selection),
            GuiButton::at_px(x + step, row2, half, s, "Delete").disabled(!has_selection),
            GuiButton::at_px(x + 2 * step, row2, half, s, "Refresh"),
        ];
    }

    fn delete_selected(&mut self) {
        if let Some(index) = self.selected {
            if index < self.servers.entries.len() {
                self.servers.entries.remove(index);
                self.servers.save();
                self.selected = None;
                self.refresh();
            }
        }
    }
}

impl GuiScreen for GuiMultiplayer {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 10 * s, s, TEXT_WHITE, "Play Multiplayer");

        // Server rows.
        self.row_rects.clear();
        let list_x = (ctx.width - 300 * s) / 2;
        let list_w = 300 * s;
        let row_h = ROW_HEIGHT_GUI * s;
        let visible = self.visible_rows(ctx);
        let max_scroll = (self.servers.entries.len() as i32 - visible).max(0);
        self.scroll = self.scroll.clamp(0, max_scroll);

        if self.servers.entries.is_empty() {
            draw_centered_text(
                ui,
                ctx.width,
                ctx.height / 2 - 12 * s,
                s,
                TEXT_GRAY,
                "No servers yet — Add Server or Direct Connect below.",
            );
        }

        for (visible_index, index) in
            (self.scroll..self.servers.entries.len() as i32).take(visible as usize).enumerate()
        {
            let index = index as usize;
            let entry = &self.servers.entries[index];
            let rect = UiRect::new(list_x, 32 * s + visible_index as i32 * row_h, list_w, row_h - 2 * s);
            self.row_rects.push((index, rect));

            ui.rect(rect, UiColor::rgba(0, 0, 0, 110));
            if self.selected == Some(index) {
                // Vanilla selection: a light outer border.
                let b = UiColor::rgba(160, 160, 160, 255);
                ui.rect(UiRect::new(rect.x, rect.y, rect.width, s), b);
                ui.rect(UiRect::new(rect.x, rect.y + rect.height - s, rect.width, s), b);
                ui.rect(UiRect::new(rect.x, rect.y, s, rect.height), b);
                ui.rect(UiRect::new(rect.x + rect.width - s, rect.y, s, rect.height), b);
            }

            let pad = 3 * s;
            let name = fit_text(&entry.name, rect.width - 90 * s, s);
            ui.text_shadowed(rect.x + pad, rect.y + pad, s, TEXT_WHITE, name);

            // Right side: players + latency (or status).
            let status = match self.pings.get(index).and_then(|p| p.as_ref()) {
                Some(PingOutcome::Ok(info)) => {
                    format!("§8{} §7{}  §a{}ms", info.version, info.players, info.latency_ms)
                }
                Some(PingOutcome::Failed(_)) => "§cCan't connect".to_owned(),
                None => "§7Pinging...".to_owned(),
            };
            let status_w = text_width(&status, s);
            ui.text_shadowed(
                rect.x + rect.width - status_w - pad,
                rect.y + pad,
                s,
                TEXT_GRAY,
                status,
            );

            // Second line: MOTD (or the failure), then the address dimmed.
            let detail = match self.pings.get(index).and_then(|p| p.as_ref()) {
                Some(PingOutcome::Ok(info)) => format!("§7{}", info.motd),
                Some(PingOutcome::Failed(err)) => format!("§4{}", fit_text(err, rect.width, s)),
                None => format!("§8{}", entry.address),
            };
            let detail = fit_text(&detail, rect.width - 2 * pad, s);
            ui.text_shadowed(rect.x + pad, rect.y + pad + 11 * s, s, TEXT_GRAY, detail);
        }

        if max_scroll > 0 {
            let label = format!("{} / {}", self.scroll + 1, self.servers.entries.len());
            draw_centered_text(ui, ctx.width, ctx.height - 64 * s, s, TEXT_GRAY, &label);
        }

        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }

        // Back link bottom-right corner.
        ui.text_shadowed(
            ctx.width - text_width("ESC: Back", s) - 4 * s,
            ctx.height - 10 * s,
            s,
            TEXT_GRAY,
            "ESC: Back",
        );
    }

    fn update(&mut self, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if let Some(rx) = &self.ping_rx {
            while let Ok((index, outcome)) = rx.try_recv() {
                if let Some(slot) = self.pings.get_mut(index) {
                    *slot = Some(outcome);
                }
            }
        }
        Vec::new()
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        // Row selection (+ double-click join).
        for (index, rect) in &self.row_rects {
            if rect.contains(x, y) {
                let now = Instant::now();
                let double = self
                    .last_click
                    .is_some_and(|(i, at)| i == *index && now - at < DOUBLE_CLICK);
                self.selected = Some(*index);
                self.last_click = Some((*index, now));
                if double {
                    return self.join_selected();
                }
                return Vec::new();
            }
        }
        if self.buttons.len() < 6 {
            return Vec::new();
        }
        if self.buttons[0].clicked(x, y) {
            return self.join_selected();
        }
        if self.buttons[1].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiDirectConnect::new()))];
        }
        if self.buttons[2].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiEditServer::add()))];
        }
        if self.buttons[3].clicked(x, y) {
            if let Some(index) = self.selected {
                if let Some(entry) = self.servers.entries.get(index) {
                    return vec![GuiAction::SetScreen(Box::new(GuiEditServer::edit(
                        index,
                        entry.clone(),
                    )))];
                }
            }
        }
        if self.buttons[4].clicked(x, y) {
            self.delete_selected();
            return Vec::new();
        }
        if self.buttons[5].clicked(x, y) {
            self.refresh();
            return Vec::new();
        }
        Vec::new()
    }

    fn mouse_scrolled(&mut self, delta: f32) {
        self.scroll -= delta.signum() as i32;
        // Clamped against the live row count next draw.
        self.scroll = self.scroll.max(0);
    }

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state != ElementState::Pressed {
            return Vec::new();
        }
        match event.physical_key {
            PhysicalKey::Code(KeyCode::Escape) => {
                vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))]
            }
            PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                self.join_selected()
            }
            _ => Vec::new(),
        }
    }
}
