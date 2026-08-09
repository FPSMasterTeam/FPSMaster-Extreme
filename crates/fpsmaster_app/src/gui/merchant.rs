//! The villager trade row (vanilla `GuiMerchant`). The merchant window's three
//! interactive slots (two buy + one sell) are the standard server-driven
//! container drawn by [`GuiContainer`](super::inventory::GuiContainer); this
//! module adds the trade *preview* drawn above them — the currently selected
//! offer's items, the `< >` selection buttons, and the disabled (deprecated)
//! overlay — from the offers the server sends over `MC|TrList`.
//!
//! Selecting an offer sends `MC|TrSel` (via [`merchant_select`](crate::game::GameState::merchant_select)),
//! and the server fills the real trade slots for that recipe.

use fpsmaster_render::{GuiTexture, UiFrame, UiRect};

use crate::container::Container;

use super::ingame::draw_item_icon;
use super::DrawCtx;

// Preview item positions, window-relative GUI px (vanilla `GuiMerchant.drawScreen`).
const BUY1: (i32, i32) = (36, 24);
const BUY2: (i32, i32) = (62, 24);
const SELL: (i32, i32) = (120, 24);

// `< >` selection buttons (vanilla `GuiMerchant.MerchantButton`): 12×19 px, the
// previous button left of the buy slots, the next button right of the sell slot.
const PREV_BTN: (i32, i32) = (36 - 19, 24 - 1);
const NEXT_BTN: (i32, i32) = (120 + 27, 24 - 1);
const BTN_W: i32 = 12;
const BTN_H: i32 = 19;

/// Which selection button the cursor is over (only the enabled one is reported).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TradeButton {
    Prev,
    Next,
}

fn prev_enabled(container: &Container) -> bool {
    container.selected_trade() > 0
}

fn next_enabled(container: &Container) -> bool {
    container.selected_trade() + 1 < container.trades().len()
}

fn hit(mouse: (f64, f64), x: i32, y: i32, w: i32, h: i32) -> bool {
    let (mx, my) = (mouse.0 as i32, mouse.1 as i32);
    (x..x + w).contains(&mx) && (y..y + h).contains(&my)
}

/// The enabled selection button under a physical-pixel cursor, if any.
pub fn button_at(
    container: &Container,
    mouse: (f64, f64),
    px: i32,
    py: i32,
    scale: i32,
) -> Option<TradeButton> {
    if scale == 0 || container.trades().is_empty() {
        return None;
    }
    let prev = (px + PREV_BTN.0 * scale, py + PREV_BTN.1 * scale);
    let next = (px + NEXT_BTN.0 * scale, py + NEXT_BTN.1 * scale);
    if prev_enabled(container) && hit(mouse, prev.0, prev.1, BTN_W * scale, BTN_H * scale) {
        return Some(TradeButton::Prev);
    }
    if next_enabled(container) && hit(mouse, next.0, next.1, BTN_W * scale, BTN_H * scale) {
        return Some(TradeButton::Next);
    }
    None
}

/// Draw the selected offer's preview items, the `< >` buttons, and (when the
/// offer is locked out) the deprecated overlay. The interactive slots below are
/// drawn separately by the container renderer.
#[allow(clippy::too_many_arguments)]
pub fn draw_offer(
    ui: &mut UiFrame,
    container: &Container,
    px: i32,
    py: i32,
    scale: i32,
    mouse: (f64, f64),
    skin_rows: &std::collections::HashMap<[u8; 16], u32>,
) {
    let Some(recipe) = container.trades().get(container.selected_trade()) else {
        return;
    };

    // A locked-out trade greys both trade arrows with the deprecated sprite
    // (212,0,28,21); drawn first (vanilla background layer) so the offer items
    // sit on top.
    if recipe.disabled {
        for y in [21, 51] {
            ui.image(
                UiRect::new(px + 83 * scale, py + y * scale, 28 * scale, 21 * scale),
                GuiTexture::Villager,
                212,
                0,
                28,
                21,
            );
        }
    }

    let icon = 16 * scale;
    let mut preview = |pos: (i32, i32), item: &fpsmaster_protocol::v1_8_9::packets::SlotItem| {
        let cell = UiRect::new(px + pos.0 * scale, py + pos.1 * scale, icon, icon);
        draw_item_icon(ui, cell, item, scale.max(2), false, skin_rows);
    };
    preview(BUY1, &recipe.buy);
    if let Some(ref buy2) = recipe.buy_secondary {
        preview(BUY2, buy2);
    }
    preview(SELL, &recipe.sell);

    draw_button(ui, container, TradeButton::Prev, px, py, scale, mouse);
    draw_button(ui, container, TradeButton::Next, px, py, scale, mouse);
}

fn draw_button(
    ui: &mut UiFrame,
    container: &Container,
    button: TradeButton,
    px: i32,
    py: i32,
    scale: i32,
    mouse: (f64, f64),
) {
    let (pos, enabled, sy) = match button {
        TradeButton::Prev => (PREV_BTN, prev_enabled(container), 19),
        TradeButton::Next => (NEXT_BTN, next_enabled(container), 0),
    };
    let (bx, by) = (px + pos.0 * scale, py + pos.1 * scale);
    // Vanilla sprite columns: 176 idle, 188 hovered, 200 disabled.
    let sx = if !enabled {
        200
    } else if hit(mouse, bx, by, BTN_W * scale, BTN_H * scale) {
        188
    } else {
        176
    };
    ui.image(
        UiRect::new(bx, by, BTN_W * scale, BTN_H * scale),
        GuiTexture::Villager,
        sx,
        sy,
        BTN_W as u32,
        BTN_H as u32,
    );
}

/// Draw the hover tooltip for a preview item or a deprecated arrow (drawn last,
/// over everything). Returns `true` if a tooltip was shown.
pub fn draw_tooltip(ui: &mut UiFrame, container: &Container, ctx: &DrawCtx, px: i32, py: i32, scale: i32) -> bool {
    let Some(recipe) = container.trades().get(container.selected_trade()) else {
        return false;
    };
    let icon = 16 * scale;
    let item_hover = |pos: (i32, i32)| hit(ctx.mouse, px + pos.0 * scale, py + pos.1 * scale, icon, icon);

    let tip = |ui: &mut UiFrame, lines: &[String]| {
        super::draw_tooltip(
            ui,
            lines,
            ctx.mouse.0 as i32,
            ctx.mouse.1 as i32,
            ctx.width,
            ctx.height,
            scale,
        );
    };

    if item_hover(BUY1) {
        tip(ui, &fpsmaster_render::build_tooltip(&recipe.buy));
    } else if recipe.buy_secondary.as_ref().is_some_and(|_| item_hover(BUY2)) {
        tip(ui, &fpsmaster_render::build_tooltip(recipe.buy_secondary.as_ref().unwrap()));
    } else if item_hover(SELL) {
        tip(ui, &fpsmaster_render::build_tooltip(&recipe.sell));
    } else if recipe.disabled
        && (hit(ctx.mouse, px + 83 * scale, py + 21 * scale, 28 * scale, 21 * scale)
            || hit(ctx.mouse, px + 83 * scale, py + 51 * scale, 28 * scale, 21 * scale))
    {
        tip(ui, &[crate::i18n::tr("fpsmaster.merchant.unavailable")]);
    } else {
        return false;
    }
    true
}
