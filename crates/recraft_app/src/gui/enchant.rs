//! The enchanting-table option rows (vanilla `GuiEnchantment`). The enchant
//! window itself is just the standard two-slot container (item + lapis) drawn by
//! [`GuiContainer`](super::inventory::GuiContainer); this module adds the three
//! enchant-option buttons on the right, driven by the window properties the
//! server sends via `S31 WindowProperty`:
//!
//! - `properties[0..3]` — the level requirement of each of the three options
//!   (`0` means the slot is empty / no option is available).
//! - `properties[3]`     — the enchantment seed (`xpSeed`), unused for layout.
//! - `properties[4..7]`  — the enchantment hint id for each option (`-1` = none).
//!
//! Clicking an option would send a `C11 EnchantItem` packet in vanilla; that
//! serverbound packet is not modelled in `recraft_protocol`, so a click is a
//! visual no-op for now (see the TODO in [`GuiContainer`]).

use recraft_render::{UiColor, UiFrame, UiRect};

use crate::container::Container;

/// Each option button is 108 GUI px wide, 19 px tall; the three are stacked
/// starting 14 px below the window top, with their left edge 60 px in (vanilla
/// `GuiEnchantment.drawGuiContainerForegroundLayer` offsets).
const OPTION_X: i32 = 60;
const OPTION_W: i32 = 108;
const OPTION_H: i32 = 19;
const OPTION_TOP: i32 = 14;

/// Vanilla enchant-text green (`0x80FF20`) and the cost-numeral gray.
const ENCHANT_GREEN: UiColor = UiColor::rgba(0x80, 0xFF, 0x20, 255);
const ENCHANT_GRAY: UiColor = UiColor::rgba(0x68, 0x68, 0x68, 255);

/// The window-property level requirement of enchant option `row` (0..3), or
/// `None` when that option is empty.
fn option_level(container: &Container, row: usize) -> Option<i16> {
    let level = container.properties[row];
    (level > 0).then_some(level)
}

/// The enchant option index (0..3) under a physical-pixel cursor, if the panel
/// is laid out (`scale > 0`) and that option is enabled.
pub fn option_at(
    container: &Container,
    mouse: (f64, f64),
    px: i32,
    py: i32,
    scale: i32,
) -> Option<usize> {
    if scale == 0 {
        return None;
    }
    let (mx, my) = (mouse.0 as i32, mouse.1 as i32);
    for row in 0..3 {
        if option_level(container, row).is_none() {
            continue;
        }
        let x = px + OPTION_X * scale;
        let y = py + (OPTION_TOP + row as i32 * OPTION_H) * scale;
        let w = OPTION_W * scale;
        let h = OPTION_H * scale;
        if (x..x + w).contains(&mx) && (y..y + h).contains(&my) {
            return Some(row);
        }
    }
    None
}

/// Draw the three enchant-option rows: the level-requirement number on the
/// right, the cost (1/2/3) and a placeholder flavour line. Vanilla renders the
/// flavour with the standard-galactic-font and an unenchanted-item gray; we have
/// no galactic glyphs, so a dim placeholder stands in (see module TODO).
pub fn draw_options(
    ui: &mut UiFrame,
    container: &Container,
    px: i32,
    py: i32,
    scale: i32,
    hovered: Option<usize>,
) {
    for row in 0..3 {
        let x = px + OPTION_X * scale;
        let y = py + (OPTION_TOP + row as i32 * OPTION_H) * scale;
        let cell = UiRect::new(x, y, OPTION_W * scale, OPTION_H * scale);
        match option_level(container, row) {
            None => {
                // Empty option: vanilla draws the disabled button sprite; with
                // no enchant texture region we leave the background showing.
            }
            Some(level) => {
                if hovered == Some(row) {
                    ui.overlay_rect(cell, UiColor::rgba(255, 255, 255, 48));
                }
                // The "1/2/3" cost on the left, and the level requirement on the
                // right (vanilla draws the requirement in green, right-aligned).
                ui.text(x + 2 * scale, y + 1 * scale, scale, ENCHANT_GRAY, format!("{}", row + 1));
                // TODO: render the enchant hint (properties[4 + row]) with the
                // standard-galactic-font; no galactic glyphs are available, so a
                // gray placeholder bar stands in for the flavour line.
                ui.text(x + 16 * scale, y + 1 * scale, scale, ENCHANT_GREEN, ". . .");
                let label = format!("{level}");
                let w = recraft_render::text_width(&label, scale);
                ui.text(
                    x + (OPTION_W - 2) * scale - w,
                    y + 9 * scale,
                    scale,
                    ENCHANT_GREEN,
                    label,
                );
            }
        }
    }
}
