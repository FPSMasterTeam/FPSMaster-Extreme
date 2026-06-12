//! The in-game HUD (vanilla `GuiIngame`): crosshair, hotbar, status bars,
//! experience, chat panel, action bar and the scoreboard sidebar. Not a
//! [`GuiScreen`](super::GuiScreen) — it renders whenever a world session is
//! active, with overlay screens (pause, chat, inventory) drawn on top.

use std::time::Instant;

use recraft_protocol::v1_8_9::packets::SlotItem;
use recraft_render::{text_width, GuiTexture, UiColor, UiFrame, UiRect};

use crate::chat::{self, ChatState};
use crate::gui::widgets::trim_to_tail;
use crate::scoreboard::Scoreboard;

/// HUD data snapshot for one frame.
#[derive(Debug, Clone, Copy)]
pub struct HudState<'a> {
    pub health: f32,
    pub food: i32,
    pub xp_bar: f32,
    pub xp_level: i32,
    pub selected_slot: i32,
    pub hotbar: &'a [Option<SlotItem>],
    pub inventory: &'a [Option<SlotItem>],
    pub chat: &'a ChatState,
    pub scoreboard: &'a Scoreboard,
}

// gui/widgets.png hotbar source metrics (pixels).
const HOTBAR_WIDTH: i32 = 182;
const HOTBAR_HEIGHT: i32 = 22;
const SELECTOR_SIZE: i32 = 24;
const SLOT_PITCH: i32 = 20;
/// Chat panel width in GUI pixels (vanilla chat is 320 wide when open).
const CHAT_WIDTH_GUI: i32 = 320;

const WHITE_DIM: UiColor = UiColor::rgba(235, 241, 232, 95);
const MUTED: UiColor = UiColor::rgba(176, 190, 181, 255);
const BLACK_120: UiColor = UiColor::rgba(0, 0, 0, 120);
const BLACK_170: UiColor = UiColor::rgba(0, 0, 0, 170);
const XP_GREEN: UiColor = UiColor::rgba(126, 232, 31, 255);
/// Sidebar score numbers (vanilla chat color "red").
const SCORE_RED: UiColor = UiColor::rgba(255, 85, 85, 255);

/// Vanilla chat text (pure white) with a 0..1 fade in the alpha.
fn faded_white(alpha: f32) -> UiColor {
    UiColor::rgba(255, 255, 255, (255.0 * alpha.clamp(0.0, 1.0)) as u8)
}

pub struct GuiIngame;

impl GuiIngame {
    /// Render the full HUD. `chat_input` is the live chat-box content when the
    /// chat overlay screen is open.
    pub fn render(
        ui: &mut UiFrame,
        width: i32,
        height: i32,
        fps: f32,
        chunk_count: usize,
        hud: &HudState,
        chat_input: Option<&str>,
    ) {
        draw_fps_panel(ui, fps, chunk_count);

        let center_x = width / 2;
        let center_y = height / 2;
        ui.rect(UiRect::new(center_x - 8, center_y - 1, 17, 2), WHITE_DIM);
        ui.rect(UiRect::new(center_x - 1, center_y - 8, 2, 17), WHITE_DIM);

        draw_status_bars(ui, width, height, hud);
        draw_hotbar(ui, width, height, hud);
        draw_action_bar(ui, width, height, hud);
        draw_sidebar(ui, width, height, hud);
        draw_chat(ui, width, height, hud, chat_input);
    }
}

fn gui_scale(height: i32) -> i32 {
    super::gui_scale(height)
}

fn draw_fps_panel(ui: &mut UiFrame, fps: f32, chunk_count: usize) {
    let fps_text = format!("FPS {:>3.0}", fps);
    let chunks_text = format!("CHUNKS {}", chunk_count);
    let width = text_width(&fps_text, 2).max(text_width(&chunks_text, 1)) + 24;
    ui.rect(UiRect::new(12, 12, width, 58), BLACK_170);
    ui.text(24, 24, 2, faded_white(1.0), fps_text);
    ui.text(24, 50, 1, MUTED, chunks_text);
}

/// Geometry of the GUI-scaled hotbar so both the background blit and the item
/// icons line up.
pub(crate) struct HotbarLayout {
    pub scale: i32,
    pub x0: i32,
    pub y0: i32,
    pub width: i32,
    pub height: i32,
}

pub(crate) fn hotbar_layout(width: i32, height: i32) -> HotbarLayout {
    let scale = gui_scale(height);
    let hotbar_w = HOTBAR_WIDTH * scale;
    let hotbar_h = HOTBAR_HEIGHT * scale;
    HotbarLayout {
        scale,
        x0: (width - hotbar_w) / 2,
        y0: height - hotbar_h - 2 * scale,
        width: hotbar_w,
        height: hotbar_h,
    }
}

/// Draw the vanilla hotbar (gui/widgets.png): the 182×22 background, the 24×22
/// selection box over the active slot, and each hotbar item's icon + count.
fn draw_hotbar(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let layout = hotbar_layout(width, height);
    let (scale, x0, y0) = (layout.scale, layout.x0, layout.y0);

    // Fallback strip so the hotbar still reads if widgets.png is missing.
    ui.rect(UiRect::new(x0, y0, layout.width, layout.height), BLACK_120);
    ui.image(
        UiRect::new(x0, y0, layout.width, layout.height),
        GuiTexture::Widgets,
        0,
        0,
        HOTBAR_WIDTH as u32,
        HOTBAR_HEIGHT as u32,
    );

    for (i, item) in hud.hotbar.iter().enumerate() {
        if let Some(item) = item {
            let cell = UiRect::new(
                x0 + (3 + i as i32 * SLOT_PITCH) * scale,
                y0 + 3 * scale,
                16 * scale,
                16 * scale,
            );
            draw_item_icon(ui, cell, *item, scale.max(2));
        }
    }

    let slot = hud.selected_slot.clamp(0, 8);
    let sel_x = x0 + slot * SLOT_PITCH * scale - scale;
    let sel_y = y0 - scale;
    ui.image(
        UiRect::new(sel_x, sel_y, SELECTOR_SIZE * scale, HOTBAR_HEIGHT * scale),
        GuiTexture::Widgets,
        0,
        HOTBAR_HEIGHT as u32,
        SELECTOR_SIZE as u32,
        HOTBAR_HEIGHT as u32,
    );
}

/// Health hearts, hunger haunches and the experience bar, all blitted from the
/// vanilla `gui/icons.png` sprite sheet (9×9 heart/haunch icons; the 182×5 XP
/// bar). Missing icons.png simply blits nothing.
fn draw_status_bars(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let layout = hotbar_layout(width, height);
    let scale = layout.scale;
    let icon = 9 * scale;
    let step = 8 * scale; // hearts/haunches overlap by 1px → 8px pitch

    // XP bar sits directly above the hotbar; the heart/hunger row above that.
    let xp_h = 5 * scale;
    let xp_y = layout.y0 - xp_h - 2 * scale;
    let row_y = xp_y - icon - scale;

    // Experience bar (full hotbar width) with a centered level number.
    ui.image(
        UiRect::new(layout.x0, xp_y, layout.width, xp_h),
        GuiTexture::Icons,
        0,
        64,
        182,
        5,
    );
    let frac = hud.xp_bar.clamp(0.0, 1.0);
    if frac > 0.0 {
        let fill_w = (layout.width as f32 * frac) as i32;
        let src_w = (182.0 * frac).round().clamp(1.0, 182.0) as u32;
        ui.image(
            UiRect::new(layout.x0, xp_y, fill_w, xp_h),
            GuiTexture::Icons,
            0,
            69,
            src_w,
            5,
        );
    }
    if hud.xp_level > 0 {
        let label = format!("{}", hud.xp_level);
        let lw = text_width(&label, scale);
        ui.text_shadowed(
            layout.x0 + (layout.width - lw) / 2,
            xp_y - 9 * scale,
            scale,
            XP_GREEN,
            label,
        );
    }

    // Health hearts, left-aligned over the left half.
    let hp = hud.health.round().clamp(0.0, 20.0) as i32;
    for i in 0..10 {
        let dst = UiRect::new(layout.x0 + i * step, row_y, icon, icon);
        ui.image(dst, GuiTexture::Icons, 16, 0, 9, 9); // empty container
        match hp - i * 2 {
            n if n >= 2 => ui.image(dst, GuiTexture::Icons, 52, 0, 9, 9), // full
            1 => ui.image(dst, GuiTexture::Icons, 61, 0, 9, 9),           // half
            _ => {}
        }
    }

    // Hunger haunches, right-aligned (drawn right→left like vanilla).
    let food = hud.food.clamp(0, 20);
    let right = layout.x0 + layout.width - icon;
    for i in 0..10 {
        let dst = UiRect::new(right - i * step, row_y, icon, icon);
        ui.image(dst, GuiTexture::Icons, 16, 27, 9, 9); // empty container
        match food - i * 2 {
            n if n >= 2 => ui.image(dst, GuiTexture::Icons, 52, 27, 9, 9), // full
            1 => ui.image(dst, GuiTexture::Icons, 61, 27, 9, 9),           // half
            _ => {}
        }
    }
}

/// Draw an item's thumbnail (real block texture for block items) plus its
/// stack count in the bottom-right. Shared with the inventory screen.
pub(crate) fn draw_item_icon(ui: &mut UiFrame, rect: UiRect, item: SlotItem, text_scale: i32) {
    ui.item_icon(rect, item.id);
    if item.count > 1 {
        let label = format!("{}", item.count);
        let w = text_width(&label, text_scale);
        ui.text_shadowed(
            rect.x + rect.width - w - text_scale,
            rect.y + rect.height - 8 * text_scale,
            text_scale,
            faded_white(1.0),
            label,
        );
    }
}

/// The chat panel: recent lines above the hotbar (fading when closed, full
/// backlog when open) plus the input bar when the chat overlay is open.
fn draw_chat(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState, input: Option<&str>) {
    let scale = gui_scale(height);
    let open = input.is_some();
    let now = Instant::now();

    let line_height = 10 * scale;
    let pad = 2 * scale;
    let chat_width = (CHAT_WIDTH_GUI * scale).min(width - 8 * scale);
    let wrap_width = chat_width - 2 * pad;
    let x = 2 * scale;

    // Input bar pinned to the bottom edge.
    let mut bottom = height - 2 * scale;
    if let Some(input) = input {
        let bar = UiRect::new(
            x,
            bottom - line_height - 2 * pad,
            chat_width,
            line_height + 2 * pad,
        );
        ui.rect(bar, BLACK_170);
        let visible = trim_to_tail(&format!("{input}_"), wrap_width, scale);
        ui.text_shadowed(bar.x + pad, bar.y + pad + scale, scale, faded_white(1.0), visible);
        bottom = bar.y - 2 * scale;
    } else {
        bottom -= 42 * scale; // sit above the hotbar + status rows when closed
    }

    // Wrapped rows, newest at the bottom, capped like vanilla (10 closed / 20 open).
    let max_rows = if open { 20 } else { 10 };
    let mut rows: Vec<(String, f32)> = Vec::new();
    for (text, alpha) in hud.chat.visible_lines(now, open) {
        let wrapped = chat::wrap_legacy(text, wrap_width, scale);
        // Within one message the first wrapped row is drawn topmost.
        for row in wrapped.into_iter().rev() {
            rows.push((row, alpha));
            if rows.len() >= max_rows {
                break;
            }
        }
        if rows.len() >= max_rows {
            break;
        }
    }
    for (row, alpha) in rows {
        let rect = UiRect::new(x, bottom - line_height, chat_width, line_height);
        ui.rect(rect, UiColor::rgba(0, 0, 0, (127.0 * alpha) as u8));
        ui.text_shadowed(rect.x + pad, rect.y + scale, scale, faded_white(alpha), row);
        bottom = rect.y;
    }
}

/// The action-bar text (chat position 2) centered above the hotbar.
fn draw_action_bar(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let Some((text, alpha)) = hud.chat.action_bar(Instant::now()) else {
        return;
    };
    let scale = gui_scale(height);
    let text_w = text_width(text, scale);
    let layout = hotbar_layout(width, height);
    ui.text_shadowed(
        (width - text_w) / 2,
        layout.y0 - 30 * scale,
        scale,
        faded_white(alpha),
        text,
    );
}

/// The scoreboard sidebar on the right edge, vanilla-style: title row, then
/// up to 15 score rows (highest first) with red score numbers.
fn draw_sidebar(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let Some(view) = hud.scoreboard.sidebar_view() else {
        return;
    };
    if view.rows.is_empty() {
        return;
    }
    let scale = gui_scale(height);
    let line_height = 10 * scale;
    let pad = 2 * scale;

    // Width: the widest of the title and each "text ... score" row
    // (text_width already ignores the § codes).
    let mut panel_width = text_width(&view.title, scale);
    for (text, score) in &view.rows {
        let row_w = text_width(text, scale) + 4 * scale + text_width(&score.to_string(), scale);
        panel_width = panel_width.max(row_w);
    }
    let panel_width = (panel_width + 2 * pad).min(width / 2);

    let total_height = (view.rows.len() as i32 + 1) * line_height;
    let x0 = width - panel_width - 2 * scale;
    let mut y = (height - total_height) / 2;

    // Title row (slightly darker, text centered). Vanilla draws the sidebar
    // text without a shadow.
    ui.rect(
        UiRect::new(x0, y, panel_width, line_height),
        UiColor::rgba(0, 0, 0, 96),
    );
    let title_w = text_width(&view.title, scale);
    ui.text(
        x0 + (panel_width - title_w) / 2,
        y + scale,
        scale,
        faded_white(1.0),
        view.title.clone(),
    );
    y += line_height;

    for (text, score) in &view.rows {
        ui.rect(
            UiRect::new(x0, y, panel_width, line_height),
            UiColor::rgba(0, 0, 0, 80),
        );
        ui.text(x0 + pad, y + scale, scale, faded_white(1.0), text.clone());
        let score_text = score.to_string();
        let score_w = text_width(&score_text, scale);
        ui.text(
            x0 + panel_width - score_w - pad,
            y + scale,
            scale,
            SCORE_RED,
            score_text,
        );
        y += line_height;
    }
}
