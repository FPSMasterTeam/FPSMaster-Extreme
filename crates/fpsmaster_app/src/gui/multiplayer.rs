//! The server list (vanilla `GuiMultiplayer`): saved servers with live
//! ping/MOTD/player-count rows, selection, double-click join, and the
//! add/edit/delete/refresh/direct-connect actions.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use fpsmaster_render::{text_width, UiColor, UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::i18n::tr;
use crate::servers::{self, PingOutcome, ServerList};

use super::edit_server::{GuiDirectConnect, GuiEditServer};
use super::main_menu::GuiMainMenu;
use super::widgets::GuiButton;
use super::{
    draw_centered_text, draw_default_background, fit_text, DrawCtx, GuiAction, GuiScreen, ListView,
    ScreenCtx, TEXT_GRAY, TEXT_WHITE,
};

/// Server row height in GUI px (vanilla rows are 36).
const ROW_HEIGHT_GUI: i32 = 36;
/// Server list width in GUI px (vanilla server list is wider than 220).
const LIST_WIDTH_GUI: i32 = 300;
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
    /// When set, the screen shows the "remove this server?" confirmation for
    /// this row instead of the list (vanilla `GuiYesNo`).
    confirm_delete: Option<usize>,
    /// Rows visible in the list on the last draw — lets keyboard navigation keep
    /// the selection scrolled into view without screen dimensions.
    last_visible: i32,
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
            confirm_delete: None,
            last_visible: 0,
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

    fn list_view(&self, ctx: &DrawCtx) -> ListView {
        // Bottom margin leaves room for the two button rows.
        ListView::new(ctx, LIST_WIDTH_GUI, ROW_HEIGHT_GUI, 64)
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let cx = ctx.width / 2;
        // Confirmation mode: just the Delete / Cancel pair (vanilla `GuiYesNo`).
        if self.confirm_delete.is_some() {
            let y = ctx.height / 6 + 96 * s;
            self.buttons = vec![
                GuiButton::at_px(cx - 155 * s, y, 150 * s, s, "Delete"),
                GuiButton::at_px(cx + 5 * s, y, 150 * s, s, "Cancel"),
            ];
            return;
        }
        // Vanilla createButtons() positions (left-anchored at width/2 - 154):
        //   row1 (height-52): Select(100) @ -154, Direct(100) @ -50, Add(100) @ +54
        //   row2 (height-28): Edit(70) @ -154, Delete(70) @ -74, Refresh(70) @ +4, Cancel(75) @ +80
        let row1 = ctx.height - 52 * s;
        let row2 = ctx.height - 28 * s;
        let has_selection = self
            .selected
            .is_some_and(|index| index < self.servers.entries.len());
        self.buttons = vec![
            // 0: Join (Select)
            GuiButton::at_px(cx - 154 * s, row1, 100 * s, s, tr("selectServer.select")).disabled(!has_selection),
            // 1: Direct Connect
            GuiButton::at_px(cx - 50 * s, row1, 100 * s, s, tr("selectServer.direct")),
            // 2: Add Server
            GuiButton::at_px(cx + 54 * s, row1, 100 * s, s, tr("fpsmaster.server.add")),
            // 3: Edit
            GuiButton::at_px(cx - 154 * s, row2, 70 * s, s, tr("selectServer.edit")).disabled(!has_selection),
            // 4: Delete
            GuiButton::at_px(cx - 74 * s, row2, 70 * s, s, tr("selectServer.delete")).disabled(!has_selection),
            // 5: Refresh
            GuiButton::at_px(cx + 4 * s, row2, 70 * s, s, tr("selectServer.refresh")),
            // 6: Cancel (back to title)
            GuiButton::at_px(cx + 80 * s, row2, 75 * s, s, tr("gui.cancel")),
        ];
    }

    fn back(&self) -> Vec<GuiAction> {
        vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))]
    }

    /// Remove the row the confirmation dialog is for, then leave confirm mode.
    fn confirm_delete_now(&mut self) {
        if let Some(index) = self.confirm_delete.take() {
            if index < self.servers.entries.len() {
                self.servers.entries.remove(index);
                self.servers.save();
                self.selected = None;
                self.refresh();
            }
        }
    }

    /// Move the selection by `delta` rows (vanilla arrow-key navigation),
    /// clamping to the list and keeping the selected row scrolled into view.
    fn select_delta(&mut self, delta: i32) {
        let count = self.servers.entries.len() as i32;
        if count == 0 {
            return;
        }
        let next = match self.selected {
            Some(i) => (i as i32 + delta).clamp(0, count - 1),
            None if delta > 0 => 0,
            None => count - 1,
        };
        self.selected = Some(next as usize);
        // Keep the selection inside the visible window.
        if next < self.scroll {
            self.scroll = next;
        } else if self.last_visible > 0 && next >= self.scroll + self.last_visible {
            self.scroll = next - self.last_visible + 1;
        }
    }

    /// Reorder the selected server by `delta` (vanilla Shift+Arrow), carrying its
    /// ping result with it and persisting the new order.
    fn move_selected(&mut self, delta: i32) {
        let Some(index) = self.selected else {
            return;
        };
        let target = index as i32 + delta;
        if target < 0 || target as usize >= self.servers.entries.len() {
            return;
        }
        let target = target as usize;
        self.servers.entries.swap(index, target);
        if index < self.pings.len() && target < self.pings.len() {
            self.pings.swap(index, target);
        }
        self.servers.save();
        self.selected = Some(target);
        self.select_delta(0); // re-clamp scroll to the moved selection
    }
}

/// Vanilla ping-bar icon cell: `(k, l)` index into icons.png. `k` selects the
/// animation column (1 while pinging), `l` the bar level 0-5.
fn ping_bar_cell(ping: Option<&PingOutcome>, slot_index: i32) -> (i32, i32) {
    match ping {
        Some(PingOutcome::Ok(info)) => {
            let l = match info.latency_ms {
                0..=149 => 0,
                150..=299 => 1,
                300..=599 => 2,
                600..=999 => 3,
                _ => 4,
            };
            (0, l)
        }
        Some(PingOutcome::Failed(_)) => (0, 5), // no connection
        None => {
            // Animated pinging bars (vanilla cycles l over time per slot).
            let mut l = (slot_index * 2) & 7;
            if l > 4 {
                l = 8 - l;
            }
            (1, l)
        }
    }
}

impl GuiScreen for GuiMultiplayer {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;

        // Delete confirmation (vanilla `GuiYesNo`): a prompt + the server name
        // over the Delete / Cancel buttons, instead of the list.
        if let Some(index) = self.confirm_delete {
            let name = self
                .servers
                .entries
                .get(index)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            draw_centered_text(
                ui,
                ctx.width,
                ctx.height / 6 + 20 * s,
                s,
                TEXT_WHITE,
                "Are you sure you want to remove this server?",
            );
            draw_centered_text(ui, ctx.width, ctx.height / 6 + 44 * s, s, TEXT_WHITE, &name);
            draw_centered_text(
                ui,
                ctx.width,
                ctx.height / 6 + 60 * s,
                s,
                TEXT_GRAY,
                "It will be lost forever! (A long time!)",
            );
            for button in &self.buttons {
                button.draw(ui, s, ctx.mouse, ctx.mouse_down);
            }
            return;
        }

        // Vanilla title at y=20 GUI px (drawn from baseline-ish, so use 20-8/2).
        draw_centered_text(ui, ctx.width, 16 * s, s, TEXT_WHITE, &tr("multiplayer.title"));

        let list = self.list_view(ctx);
        list.draw_background(ui);

        // Server rows.
        self.row_rects.clear();
        let total = self.servers.entries.len() as i32;
        let visible = list.visible_rows();
        self.last_visible = visible;
        let max_scroll = (total - visible).max(0);
        self.scroll = self.scroll.clamp(0, max_scroll);
        // A hover tooltip (ping latency or player sample), drawn last so it sits
        // over the rows and buttons.
        let mut tooltip: Option<Vec<String>> = None;

        if self.servers.entries.is_empty() {
            draw_centered_text(
                ui,
                ctx.width,
                (list.top + list.bottom) / 2 - 4 * s,
                s,
                TEXT_GRAY,
                &tr("fpsmaster.multiplayer.empty"),
            );
        }

        for (visible_index, index) in
            (self.scroll..total).take(visible as usize).enumerate()
        {
            let index = index as usize;
            let entry = &self.servers.entries[index];
            let rect = list.row_rect(visible_index as i32);
            self.row_rects.push((index, rect));

            if self.selected == Some(index) {
                list.draw_selection(ui, rect);
            }

            let ping = self.pings.get(index).and_then(|p| p.as_ref());

            // Server icon: 32×32 at the row's top-left. Use the server's
            // favicon when the ping returned one, else a dark placeholder plate.
            let icon = UiRect::new(rect.x, rect.y, 32 * s, 32 * s);
            match ping.and_then(|p| match p {
                PingOutcome::Ok(info) => info.favicon.clone(),
                _ => None,
            }) {
                Some(favicon) => ui.raw_image(icon, favicon),
                None => {
                    ui.rect(icon, UiColor::rgba(0, 0, 0, 160));
                    ui.rect(
                        UiRect::new(icon.x, icon.y, icon.width, s),
                        UiColor::rgba(80, 80, 80, 255),
                    );
                }
            }

            // Text column starts at x + 32 + 3 (vanilla).
            let text_x = rect.x + 35 * s;
            let text_w = rect.width - 35 * s;

            // Line 1: server name (white).
            let name = fit_text(&entry.name, text_w - 60 * s, s);
            ui.text_shadowed(text_x, rect.y + s, s, TEXT_WHITE, name);

            // Player count, right-aligned on line 1 (left of the ping bars).
            // Vanilla shows only the population here, not the server brand.
            let pop = match ping {
                Some(PingOutcome::Ok(info)) => format!("§7{}", info.players),
                _ => String::new(),
            };
            if !pop.is_empty() {
                let pw = text_width(&pop, s);
                let px = rect.x + rect.width - pw - 15 * s - 2 * s;
                ui.text_shadowed(px, rect.y + s, s, TEXT_GRAY, pop);
                // Hovering the player count lists the online-player sample.
                if let Some(PingOutcome::Ok(info)) = ping {
                    if !info.sample.is_empty()
                        && UiRect::new(px, rect.y + s, pw, 8 * s).contains(ctx.mouse.0, ctx.mouse.1)
                    {
                        tooltip = Some(info.sample.clone());
                    }
                }
            }

            // Lines 2-3: MOTD wrapped to 2 lines (vanilla), or status text.
            let motd = match ping {
                Some(PingOutcome::Ok(info)) => format!("§7{}", info.motd),
                Some(PingOutcome::Failed(err)) => format!("§4{err}"),
                None => format!("§7{}", tr("fpsmaster.multiplayer.pinging")),
            };
            let lines = crate::chat::wrap_legacy(&motd, text_w - 2 * s, s);
            for (i, line) in lines.iter().take(2).enumerate() {
                ui.text_shadowed(
                    text_x,
                    rect.y + 12 * s + i as i32 * 9 * s,
                    s,
                    TEXT_GRAY,
                    line,
                );
            }

            // Ping bars: icons.png src (k*10, 176 + l*8), 10×8, at (x+listWidth-15, y).
            let (k, l) = ping_bar_cell(ping, visible_index as i32);
            let bar = UiRect::new(rect.x + rect.width - 15 * s, rect.y, 10 * s, 8 * s);
            ui.image(
                bar,
                fpsmaster_render::GuiTexture::Icons,
                (k * 10) as u32,
                (176 + l * 8) as u32,
                10,
                8,
            );
            // Hovering the ping bar shows latency / connection status.
            if bar.contains(ctx.mouse.0, ctx.mouse.1) {
                tooltip = Some(vec![match ping {
                    Some(PingOutcome::Ok(info)) => format!("{}ms", info.latency_ms),
                    Some(PingOutcome::Failed(_)) => "(no connection)".to_owned(),
                    None => "Pinging...".to_owned(),
                }]);
            }
        }

        list.draw_scrollbar(ui, self.scroll, max_scroll, total);

        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }

        if let Some(lines) = tooltip {
            super::draw_tooltip(
                ui,
                &lines,
                ctx.mouse.0 as i32,
                ctx.mouse.1 as i32,
                ctx.width,
                ctx.height,
                s,
            );
        }
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
        // Confirmation dialog: only its Delete / Cancel buttons are live.
        if self.confirm_delete.is_some() {
            if self.buttons.first().is_some_and(|b| b.clicked(x, y)) {
                self.confirm_delete_now();
            } else if self.buttons.get(1).is_some_and(|b| b.clicked(x, y)) {
                self.confirm_delete = None;
            }
            return Vec::new();
        }
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
        if self.buttons.len() < 7 {
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
            // Delete asks for confirmation first (vanilla `GuiYesNo`).
            if self.selected.is_some_and(|i| i < self.servers.entries.len()) {
                self.confirm_delete = self.selected;
            }
            return Vec::new();
        }
        if self.buttons[5].clicked(x, y) {
            self.refresh();
            return Vec::new();
        }
        if self.buttons[6].clicked(x, y) {
            return self.back();
        }
        Vec::new()
    }

    fn mouse_scrolled(&mut self, delta: f32) {
        self.scroll -= delta.signum() as i32;
        // Clamped against the live row count next draw.
        self.scroll = self.scroll.max(0);
    }

    fn key_pressed(&mut self, event: &KeyEvent, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state != ElementState::Pressed {
            return Vec::new();
        }
        // While confirming a delete, the keyboard only confirms or cancels.
        if self.confirm_delete.is_some() {
            match event.physical_key {
                PhysicalKey::Code(KeyCode::Escape) => self.confirm_delete = None,
                PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => self.confirm_delete_now(),
                _ => {}
            }
            return Vec::new();
        }
        let shift = ctx.modifiers.shift_key();
        match event.physical_key {
            PhysicalKey::Code(KeyCode::Escape) => {
                vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))]
            }
            PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                self.join_selected()
            }
            // Shift+Arrow reorders the selected server; plain Arrow moves the
            // selection (vanilla `GuiMultiplayer.keyTyped`).
            PhysicalKey::Code(KeyCode::ArrowUp) => {
                if shift {
                    self.move_selected(-1);
                } else {
                    self.select_delta(-1);
                }
                Vec::new()
            }
            PhysicalKey::Code(KeyCode::ArrowDown) => {
                if shift {
                    self.move_selected(1);
                } else {
                    self.select_delta(1);
                }
                Vec::new()
            }
            PhysicalKey::Code(KeyCode::F5) => {
                self.refresh();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}
