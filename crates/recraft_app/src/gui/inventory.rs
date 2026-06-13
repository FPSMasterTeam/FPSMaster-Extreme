//! The survival inventory screen (vanilla `GuiInventory`): draws the 2×2 craft,
//! armor, main and hotbar slots and drives full slot interaction — pick up,
//! place, swap, merge, shift-move, number-swap, drop and paint-drag — through
//! [`GameState`](crate::game::GameState)'s container logic, which predicts the
//! result locally and emits the matching ClickWindow packets.

use recraft_render::{GuiTexture, UiColor, UiFrame, UiRect};
use recraft_protocol::v1_8_9::packets::ServerboundPacket;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::ingame::draw_item_icon;
use super::{DrawCtx, GuiAction, GuiScreen, ScreenCtx};

/// The vanilla survival inventory window is 176×166 px.
const WINDOW_W: i32 = 176;
const WINDOW_H: i32 = 166;

#[derive(Default)]
pub struct GuiInventory {
    /// Panel origin and GUI scale cached from the last `draw`, so input handlers
    /// (which receive no screen dimensions) can hit-test slots.
    px: i32,
    py: i32,
    scale: i32,
}

impl GuiInventory {
    pub fn new() -> Self {
        Self::default()
    }

    /// The window-0 slot index under a physical-pixel cursor position, if any.
    fn slot_at(&self, mouse: (f64, f64)) -> Option<i16> {
        if self.scale == 0 {
            return None;
        }
        let (mx, my) = (mouse.0 as i32, mouse.1 as i32);
        let icon = 16 * self.scale;
        for (slot, sx, sy) in inventory_slot_positions() {
            let cell_x = self.px + sx * self.scale;
            let cell_y = self.py + sy * self.scale;
            if (cell_x..cell_x + icon).contains(&mx) && (cell_y..cell_y + icon).contains(&my) {
                return Some(slot as i16);
            }
        }
        None
    }
}

/// Wrap container packets as send-actions for the host to dispatch.
fn send(packets: Vec<ServerboundPacket>) -> Vec<GuiAction> {
    packets.into_iter().map(GuiAction::SendPacket).collect()
}

impl GuiScreen for GuiInventory {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        ui.rect(
            UiRect::new(0, 0, ctx.width, ctx.height),
            UiColor::rgba(16, 16, 16, 160),
        );
        let Some(hud) = ctx.hud else { return };

        let scale = ctx.scale;
        let pw = WINDOW_W * scale;
        let ph = WINDOW_H * scale;
        let px = (ctx.width - pw) / 2;
        let py = (ctx.height - ph) / 2;
        // Cache the layout so the input handlers can map cursor → slot.
        self.px = px;
        self.py = py;
        self.scale = scale;

        // Dark fallback panel behind the texture (readable without assets).
        ui.rect(UiRect::new(px, py, pw, ph), UiColor::rgba(0, 0, 0, 210));
        ui.image(
            UiRect::new(px, py, pw, ph),
            GuiTexture::Inventory,
            0,
            0,
            WINDOW_W as u32,
            WINDOW_H as u32,
        );

        let icon = 16 * scale;
        let hovered = self.slot_at(ctx.mouse);
        for (slot, sx, sy) in inventory_slot_positions() {
            let cell = UiRect::new(px + sx * scale, py + sy * scale, icon, icon);
            if let Some(Some(item)) = hud.inventory.get(slot) {
                draw_item_icon(ui, cell, *item, scale.max(2));
            }
            if hovered == Some(slot as i16) {
                // Vanilla highlights the hovered slot (white at ~50% alpha).
                ui.rect(cell, UiColor::rgba(255, 255, 255, 128));
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
            draw_item_icon(ui, cell, item, scale.max(2));
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        // Ignore a second button pressed while a paint-drag is in progress
        // (vanilla locks the drag to its owning button).
        if ctx.game.drag_active() {
            return Vec::new();
        }
        let slot = self.slot_at((x, y));
        if ctx.modifiers.shift_key() {
            return match slot {
                Some(slot) => send(ctx.game.click_slot(slot, 0, 1)),
                None => Vec::new(),
            };
        }
        if ctx.game.cursor_item().is_some() {
            // Holding a stack: this may become a paint-drag — defer to release.
            ctx.game.drag_begin(false);
            if let Some(slot) = slot {
                ctx.game.drag_add_slot(slot);
            }
            Vec::new()
        } else {
            match slot {
                Some(slot) => send(ctx.game.click_slot(slot, 0, 0)),
                None => Vec::new(),
            }
        }
    }

    fn mouse_right_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if ctx.game.drag_active() {
            return Vec::new();
        }
        let slot = self.slot_at((x, y));
        if ctx.game.cursor_item().is_some() {
            ctx.game.drag_begin(true);
            if let Some(slot) = slot {
                ctx.game.drag_add_slot(slot);
            }
            Vec::new()
        } else {
            match slot {
                Some(slot) => send(ctx.game.click_slot(slot, 1, 0)),
                None => Vec::new(),
            }
        }
    }

    fn mouse_dragged(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) {
        if ctx.game.drag_active() {
            if let Some(slot) = self.slot_at((x, y)) {
                ctx.game.drag_add_slot(slot);
            }
        }
    }

    fn mouse_released(&mut self, x: f64, y: f64, right: bool, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        // Only the button that started the drag may end it; a release of the
        // other button mid-drag is ignored.
        if !ctx.game.drag_active() || right != ctx.game.drag_is_right() {
            return Vec::new();
        }
        if ctx.game.drag_len() > 1 {
            return send(ctx.game.drag_commit());
        }
        // A drag over a single slot is just a normal click (place / drop).
        let button = if ctx.game.drag_is_right() { 1 } else { 0 };
        ctx.game.drag_cancel();
        let slot = self.slot_at((x, y)).unwrap_or(-999);
        send(ctx.game.click_slot(slot, button, 0))
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
                let packet = ctx.game.close_inventory();
                vec![GuiAction::SendPacket(packet), GuiAction::CloseScreen]
            }
            KeyCode::KeyQ => match self.slot_at(ctx.mouse) {
                Some(slot) => {
                    // Q drops one, Ctrl+Q drops the whole stack.
                    let button = if ctx.modifiers.control_key() { 1 } else { 0 };
                    send(ctx.game.click_slot(slot, button, 4))
                }
                None => Vec::new(),
            },
            _ => match hotbar_digit(code) {
                Some(hotbar) => match self.slot_at(ctx.mouse) {
                    Some(slot) => send(ctx.game.click_slot(slot, hotbar, 2)),
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
