//! The survival inventory screen (vanilla `GuiInventory`), display-only.

use recraft_render::{GuiTexture, UiColor, UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::ingame::draw_item_icon;
use super::{DrawCtx, GuiAction, GuiScreen, ScreenCtx};

#[derive(Default)]
pub struct GuiInventory;

impl GuiInventory {
    pub fn new() -> Self {
        Self
    }
}

impl GuiScreen for GuiInventory {
    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        ui.rect(
            UiRect::new(0, 0, ctx.width, ctx.height),
            UiColor::rgba(16, 16, 16, 160),
        );
        let Some(hud) = ctx.hud else { return };

        // The vanilla survival inventory window is 176×166; GUI-scale it and
        // place item icons at the texture's slot coordinates.
        let scale = ctx.scale;
        let pw = 176 * scale;
        let ph = 166 * scale;
        let px = (ctx.width - pw) / 2;
        let py = (ctx.height - ph) / 2;
        // Dark fallback panel behind the texture (readable without assets).
        ui.rect(UiRect::new(px, py, pw, ph), UiColor::rgba(0, 0, 0, 210));
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

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed
            && matches!(
                event.physical_key,
                PhysicalKey::Code(KeyCode::Escape) | PhysicalKey::Code(KeyCode::KeyE)
            )
        {
            return vec![GuiAction::CloseScreen];
        }
        Vec::new()
    }

    fn draws_over_hud(&self) -> bool {
        true
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
