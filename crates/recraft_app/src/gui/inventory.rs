//! The container window screen (vanilla `GuiContainer` and its subclasses):
//! draws the active [`Container`](crate::container::Container) — the player
//! inventory (window 0, opened with E) or any server-opened window (chest,
//! furnace, dispenser, hopper, crafting table, brewing stand, enchanting
//! table). It renders the per-kind background and slot grid, and drives the
//! full vanilla slot interaction — pick up, place, swap, merge, shift-move,
//! number-swap, drop, double-click collect, creative clone and paint-drag —
//! through [`GameState`](crate::game::GameState), which predicts locally and
//! emits the matching ClickWindow packets.

use std::time::{Duration, Instant};

use recraft_render::{GuiTexture, UiColor, UiFrame, UiRect};
use recraft_protocol::v1_8_9::packets::ServerboundPacket;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::container::{Container, WindowKind};

use super::ingame::draw_item_icon;
use super::{DrawCtx, GuiAction, GuiScreen, ScreenCtx};

/// Vanilla container title color (`0x404040`, no shadow).
const TITLE_COLOR: UiColor = UiColor::rgba(64, 64, 64, 255);

#[derive(Default)]
pub struct GuiContainer {
    /// Panel origin and GUI scale cached from the last `draw`, so input handlers
    /// (which receive no screen dimensions) can hit-test slots.
    px: i32,
    py: i32,
    scale: i32,
    /// Last clicked slot + time, for vanilla double-click (mode 6) detection.
    last_click_slot: i16,
    last_click_at: Option<Instant>,
}

impl GuiContainer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The window slot index under a physical-pixel cursor position, if any.
    fn slot_at(&self, mouse: (f64, f64), container: &Container) -> Option<i16> {
        if self.scale == 0 {
            return None;
        }
        let (mx, my) = (mouse.0 as i32, mouse.1 as i32);
        let icon = 16 * self.scale;
        for (i, slot) in container.slots().iter().enumerate() {
            let cell_x = self.px + slot.x * self.scale;
            let cell_y = self.py + slot.y * self.scale;
            if (cell_x..cell_x + icon).contains(&mx) && (cell_y..cell_y + icon).contains(&my) {
                return Some(i as i16);
            }
        }
        None
    }

    /// Record this click for double-click detection and report whether it was a
    /// double-click on `slot` (same slot, within 250 ms).
    fn register_double_click(&mut self, slot: Option<i16>, now: Instant) -> bool {
        let is_double = slot.is_some()
            && slot == Some(self.last_click_slot)
            && self
                .last_click_at
                .is_some_and(|t| now.duration_since(t) < Duration::from_millis(250));
        self.last_click_at = Some(now);
        self.last_click_slot = slot.unwrap_or(-999);
        is_double
    }
}

/// Wrap container packets as send-actions for the host to dispatch.
fn send(packets: Vec<ServerboundPacket>) -> Vec<GuiAction> {
    packets.into_iter().map(GuiAction::SendPacket).collect()
}

impl GuiScreen for GuiContainer {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        ui.rect(
            UiRect::new(0, 0, ctx.width, ctx.height),
            UiColor::rgba(16, 16, 16, 160),
        );
        let Some(hud) = ctx.hud else { return };
        let Some(container) = hud.container else { return };

        let scale = ctx.scale;
        let pw = container.x_size * scale;
        let ph = container.y_size * scale;
        let px = (ctx.width - pw) / 2;
        let py = (ctx.height - ph) / 2;
        // Cache the layout so the input handlers can map cursor → slot.
        self.px = px;
        self.py = py;
        self.scale = scale;

        // Dark fallback panel behind the texture (readable without assets).
        ui.rect(UiRect::new(px, py, pw, ph), UiColor::rgba(0, 0, 0, 210));
        draw_background(ui, container, px, py, scale);
        draw_progress(ui, container, px, py, scale);
        draw_titles(ui, container, px, py, scale);

        let icon = 16 * scale;
        let hovered = self.slot_at(ctx.mouse, container);
        for (i, slot) in container.slots().iter().enumerate() {
            let cell = UiRect::new(px + slot.x * scale, py + slot.y * scale, icon, icon);
            if let Some(item) = container.slot_item(i, hud.inventory) {
                draw_item_icon(ui, cell, item, scale.max(2), false);
            }
            if hovered == Some(i as i16) {
                // Vanilla highlights the hovered slot (white at ~50% alpha) —
                // on the overlay layer so it covers 3D block icons too.
                ui.overlay_rect(cell, UiColor::rgba(255, 255, 255, 128));
            }
        }

        // The carried stack follows the cursor, centered on it.
        if let Some(item) = hud.cursor_item {
            let cell = UiRect::new(
                ctx.mouse.0 as i32 - icon / 2,
                ctx.mouse.1 as i32 - icon / 2,
                icon,
                icon,
            );
            draw_item_icon(ui, cell, item, scale.max(2), true);
        } else if let Some(slot) = hovered {
            // Hovering an item with an empty cursor: show the vanilla tooltip.
            if let Some(item) = container.slot_item(slot as usize, hud.inventory) {
                let name = recraft_render::item_display_name(item.id, item.damage);
                super::draw_tooltip(
                    ui,
                    &[name],
                    ctx.mouse.0 as i32,
                    ctx.mouse.1 as i32,
                    ctx.width,
                    ctx.height,
                    scale,
                );
            }
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        // Ignore a second button pressed while a paint-drag is in progress
        // (vanilla locks the drag to its owning button).
        if ctx.game.container_drag_active() {
            return Vec::new();
        }
        let slot = slot_under(self, ctx, (x, y));
        if ctx.modifiers.shift_key() {
            return match slot {
                Some(slot) => send(ctx.game.container_click(slot, 0, 1)),
                None => Vec::new(),
            };
        }
        let now = Instant::now();
        if ctx.game.cursor_item().is_some() {
            // Holding a stack: a quick second click on the same slot is a
            // double-click collect (mode 6); otherwise begin a left paint-drag.
            if self.register_double_click(slot, now) {
                if let Some(slot) = slot {
                    return send(ctx.game.container_click(slot, 0, 6));
                }
            }
            ctx.game.container_drag_begin(0);
            if let Some(slot) = slot {
                ctx.game.container_drag_add(slot);
            }
            Vec::new()
        } else {
            self.register_double_click(slot, now);
            // -999 (outside the window) with an empty cursor does nothing.
            match slot {
                Some(slot) => send(ctx.game.container_click(slot, 0, 0)),
                None => Vec::new(),
            }
        }
    }

    fn mouse_right_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if ctx.game.container_drag_active() {
            return Vec::new();
        }
        let slot = slot_under(self, ctx, (x, y));
        if ctx.game.cursor_item().is_some() {
            ctx.game.container_drag_begin(1);
            if let Some(slot) = slot {
                ctx.game.container_drag_add(slot);
            }
            Vec::new()
        } else {
            match slot {
                Some(slot) => send(ctx.game.container_click(slot, 1, 0)),
                None => Vec::new(),
            }
        }
    }

    fn mouse_middle_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        // Creative middle-click clone (mode 3); vanilla only sends it in creative.
        if ctx.game.container_drag_active() || !ctx.game.is_creative() {
            return Vec::new();
        }
        match slot_under(self, ctx, (x, y)) {
            Some(slot) => send(ctx.game.container_click(slot, 2, 3)),
            None => Vec::new(),
        }
    }

    fn mouse_dragged(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) {
        if ctx.game.container_drag_active() {
            if let Some(slot) = slot_under(self, ctx, (x, y)) {
                ctx.game.container_drag_add(slot);
            }
        }
    }

    fn mouse_released(&mut self, x: f64, y: f64, right: bool, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        // Only the button that started the drag may end it; a release of the
        // other button mid-drag is ignored.
        let owning_right = ctx.game.container_drag_button() == 1;
        if !ctx.game.container_drag_active() || right != owning_right {
            return Vec::new();
        }
        if ctx.game.container_drag_len() > 1 {
            return send(ctx.game.container_drag_commit());
        }
        // A drag over a single slot is just a normal click (place / drop).
        let button = if owning_right { 1 } else { 0 };
        ctx.game.container_drag_cancel();
        let slot = slot_under(self, ctx, (x, y)).unwrap_or(-999);
        send(ctx.game.container_click(slot, button, 0))
    }

    fn key_pressed(&mut self, event: &KeyEvent, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state != ElementState::Pressed {
            return Vec::new();
        }
        let PhysicalKey::Code(code) = event.physical_key else {
            return Vec::new();
        };
        match code {
            KeyCode::Escape | KeyCode::KeyE => {
                let mut actions = Vec::new();
                if let Some(packet) = ctx.game.container_close() {
                    actions.push(GuiAction::SendPacket(packet));
                }
                actions.push(GuiAction::CloseScreen);
                actions
            }
            KeyCode::KeyQ => match slot_under(self, ctx, ctx.mouse) {
                Some(slot) => {
                    // Q drops one, Ctrl+Q drops the whole stack.
                    let button = if ctx.modifiers.control_key() { 1 } else { 0 };
                    send(ctx.game.container_click(slot, button, 4))
                }
                None => Vec::new(),
            },
            _ => match hotbar_digit(code) {
                Some(hotbar) => match slot_under(self, ctx, ctx.mouse) {
                    Some(slot) => send(ctx.game.container_click(slot, hotbar, 2)),
                    None => Vec::new(),
                },
                None => Vec::new(),
            },
        }
    }

    fn draws_over_hud(&self) -> bool {
        true
    }
}

/// Hit-test a window slot using the cached layout and the active container.
fn slot_under(screen: &GuiContainer, ctx: &ScreenCtx, mouse: (f64, f64)) -> Option<i16> {
    let container = ctx.game.open_container()?;
    screen.slot_at(mouse, container)
}

/// Blit the window's background texture (the chest is two parts for a variable
/// row count; every other kind is one full blit), falling back silently to the
/// dark panel when the texture is missing.
fn draw_background(ui: &mut UiFrame, container: &Container, px: i32, py: i32, scale: i32) {
    let (x_size, y_size) = (container.x_size, container.y_size);
    if let WindowKind::Chest(rows) = container.kind {
        let rows = rows as i32;
        let top = rows * 18 + 17;
        ui.image(
            UiRect::new(px, py, x_size * scale, top * scale),
            GuiTexture::Chest,
            0,
            0,
            x_size as u32,
            top as u32,
        );
        ui.image(
            UiRect::new(px, py + top * scale, x_size * scale, 96 * scale),
            GuiTexture::Chest,
            0,
            126,
            x_size as u32,
            96,
        );
        return;
    }
    let texture = match container.kind {
        WindowKind::Player => GuiTexture::Inventory,
        WindowKind::Dispenser => GuiTexture::Dispenser,
        WindowKind::Hopper => GuiTexture::Hopper,
        WindowKind::Furnace => GuiTexture::Furnace,
        WindowKind::Crafting => GuiTexture::CraftingTable,
        WindowKind::Brewing => GuiTexture::BrewingStand,
        WindowKind::Enchant => GuiTexture::EnchantingTable,
        WindowKind::Chest(_) => unreachable!(),
    };
    ui.image(
        UiRect::new(px, py, x_size * scale, y_size * scale),
        texture,
        0,
        0,
        x_size as u32,
        y_size as u32,
    );
}

/// Window titles (vanilla `drawGuiContainerForegroundLayer`): the container name
/// top-left, plus the "Inventory" label over the player slots. The player
/// inventory window draws no title (it is part of `inventory.png`).
fn draw_titles(ui: &mut UiFrame, container: &Container, px: i32, py: i32, scale: i32) {
    if container.kind == WindowKind::Player {
        return;
    }
    if !container.title.is_empty() {
        ui.text(px + 8 * scale, py + 6 * scale, scale, TITLE_COLOR, container.title.clone());
    }
    // "Inventory" sits 96 px up from the window bottom (vanilla offset).
    ui.text(
        px + 8 * scale,
        py + (container.y_size - 96 + 2) * scale,
        scale,
        TITLE_COLOR,
        "Inventory",
    );
}

/// Progress sprites driven by WindowProperty: the furnace flame + smelt arrow.
/// (Brewing/enchant overlays are not drawn.)
fn draw_progress(ui: &mut UiFrame, container: &Container, px: i32, py: i32, scale: i32) {
    if container.kind != WindowKind::Furnace {
        return;
    }
    let p = container.properties;
    let burn = p[0] as i32;
    let burn_total = if p[1] != 0 { p[1] as i32 } else { 200 };
    if burn > 0 {
        // Flame fills bottom-up: k px tall (vanilla scales the 13 px sprite).
        let k = (burn * 13 / burn_total.max(1)).clamp(0, 12);
        ui.image(
            UiRect::new(px + 56 * scale, py + (36 + 12 - k) * scale, 14 * scale, (k + 1) * scale),
            GuiTexture::Furnace,
            176,
            (12 - k) as u32,
            14,
            (k + 1) as u32,
        );
    }
    let cook = p[2] as i32;
    let cook_total = p[3] as i32;
    if cook_total != 0 && cook != 0 {
        let l = (cook * 24 / cook_total).clamp(0, 24);
        ui.image(
            UiRect::new(px + 79 * scale, py + 34 * scale, (l + 1) * scale, 16 * scale),
            GuiTexture::Furnace,
            176,
            14,
            (l + 1) as u32,
            16,
        );
    }
}

/// The hotbar index (0..8) for a number-row key, or `None` for other keys.
fn hotbar_digit(code: KeyCode) -> Option<i8> {
    Some(match code {
        KeyCode::Digit1 => 0,
        KeyCode::Digit2 => 1,
        KeyCode::Digit3 => 2,
        KeyCode::Digit4 => 3,
        KeyCode::Digit5 => 4,
        KeyCode::Digit6 => 5,
        KeyCode::Digit7 => 6,
        KeyCode::Digit8 => 7,
        KeyCode::Digit9 => 8,
        _ => return None,
    })
}
