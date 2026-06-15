//! The in-game HUD (vanilla `GuiIngame`): crosshair, hotbar, status bars,
//! experience, chat panel, action bar and the scoreboard sidebar. Not a
//! [`GuiScreen`](super::GuiScreen) — it renders whenever a world session is
//! active, with overlay screens (pause, chat, inventory) drawn on top.

use std::time::Instant;

use recraft_protocol::v1_8_9::packets::SlotItem;
use recraft_render::{text_height, text_width, GuiTexture, OverlayTextures, RenderStats, UiColor, UiFrame, UiRect};

use crate::chat::{self, ChatState};
use crate::game::{ScreenOverlay, TitleOverlay};
use crate::gui::widgets::trim_to_tail;
use crate::scoreboard::Scoreboard;
use crate::text_input::TextInput;

/// HUD data snapshot for one frame.
#[derive(Debug, Clone)]
pub struct HudState<'a> {
    pub health: f32,
    pub food: i32,
    pub armor: i32,
    pub xp_bar: f32,
    pub xp_level: i32,
    pub selected_slot: i32,
    pub hotbar: &'a [Option<SlotItem>],
    pub inventory: &'a [Option<SlotItem>],
    /// The open window (player inventory or a server container), whose slot
    /// layout the container screen renders. `None` when no window is open.
    pub container: Option<&'a crate::container::Container>,
    /// The stack carried on the cursor in an open inventory (vanilla slot -1).
    pub cursor_item: Option<&'a SlotItem>,
    pub chat: &'a ChatState,
    pub scoreboard: &'a Scoreboard,
    /// Tab-list roster; drawn as the player-list overlay while `tab_open`.
    pub player_list: &'a crate::player_list::PlayerList,
    /// Whether the Tab key is held (show the player-list overlay).
    pub tab_open: bool,
    pub title: Option<TitleOverlay<'a>>,
    pub screen_overlay: ScreenOverlay,
    pub overlay_textures: &'a OverlayTextures,
}

/// Data for the F3 debug overlay: the player feet position (world coords),
/// look angles and the previous frame's render-pass timings/draw scale.
#[derive(Debug, Clone, Copy)]
pub struct DebugInfo {
    /// Player feet position in world coordinates (vanilla posX/posY/posZ).
    pub pos: [f64; 3],
    pub on_ground: bool,
    pub yaw: f32,
    pub pitch: f32,
    pub stats: RenderStats,
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
    /// Render the full HUD. `chat_input` is the live chat-box buffer when the
    /// chat overlay screen is open (its caret and IME composition are drawn,
    /// and its caret area recorded for IME candidate-window placement).
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        ui: &mut UiFrame,
        width: i32,
        height: i32,
        fps: f32,
        chunk_count: usize,
        hud: &HudState,
        chat_input: Option<&mut TextInput>,
        debug: Option<&DebugInfo>,
        // Whether a screen (inventory / pause / chat) is open. The HUD still draws
        // (over the screen's scrim) so the hotbar/status bars stay visible, but the
        // crosshair is hidden so it doesn't sit in the middle of the menu.
        screen_open: bool,
    ) {
        let scale = gui_scale(width, height);

        draw_screen_overlay(ui, width, height, hud.screen_overlay, hud.overlay_textures);

        // F3 replaces the small FPS readout with the full debug overlay.
        match debug {
            Some(info) => draw_debug_overlay(ui, width, scale, fps, chunk_count, info),
            None => draw_fps_panel(ui, scale, fps, chunk_count),
        }

        // Crosshair: 10 GUI px arms, 2 GUI px thick. Hidden while a screen is open.
        if !screen_open {
            let center_x = width / 2;
            let center_y = height / 2;
            ui.rect(
                UiRect::new(center_x - 5 * scale, center_y - scale, 10 * scale, 2 * scale),
                WHITE_DIM,
            );
            ui.rect(
                UiRect::new(center_x - scale, center_y - 5 * scale, 2 * scale, 10 * scale),
                WHITE_DIM,
            );
        }

        // Chat is the bottom-most HUD layer: the status bars (health / hunger / XP)
        // and the hotbar draw on top of it, so an open chat or a long backlog never
        // covers the HUD — solved by z-order, not by repositioning the chat.
        draw_chat(ui, width, height, hud, chat_input);
        draw_status_bars(ui, width, height, hud);
        draw_hotbar(ui, width, height, hud);
        draw_title(ui, width, height, hud);
        draw_action_bar(ui, width, height, hud);
        draw_sidebar(ui, width, height, hud);
        draw_tab_list(ui, width, height, hud);
    }
}

fn gui_scale(width: i32, height: i32) -> i32 {
    super::gui_scale(width, height)
}

fn draw_fps_panel(ui: &mut UiFrame, scale: i32, fps: f32, chunk_count: usize) {
    let fps_text = format!("FPS {:>3.0}", fps);
    let chunks_text = format!("Chunks {chunk_count}");
    let width = text_width(&fps_text, scale).max(text_width(&chunks_text, scale)) + 8 * scale;
    ui.rect(UiRect::new(4 * scale, 4 * scale, width, 26 * scale), BLACK_170);
    ui.text_shadowed(8 * scale, 8 * scale, scale, faded_white(1.0), fps_text);
    ui.text_shadowed(8 * scale, 19 * scale, scale, MUTED, chunks_text);
}

/// Per-line background behind the F3 text (vanilla draws a translucent plate).
const DEBUG_BG: UiColor = UiColor::rgba(16, 16, 16, 160);

/// The vanilla F3 debug overlay: world/position info down the left edge and
/// the renderer's per-pass profiler down the right edge, each line on a
/// translucent plate. The render stats are the previous frame's (collected
/// after the draw), one frame stale — fine for a live readout.
fn draw_debug_overlay(
    ui: &mut UiFrame,
    width: i32,
    scale: i32,
    fps: f32,
    chunk_count: usize,
    info: &DebugInfo,
) {
    let line = text_height(scale) + 2 * scale;
    let margin = 2 * scale;

    let [fx, fy, fz] = info.pos;
    let (bx, by, bz) = (fx.floor() as i64, fy.floor() as i64, fz.floor() as i64);
    let (cx, cz) = (bx.div_euclid(16), bz.div_euclid(16));
    let (rx, ry, rz) = (bx.rem_euclid(16), by.rem_euclid(16), bz.rem_euclid(16));

    // Vanilla EnumFacing.fromAngle: yaw 0 = south(+Z), CW through west/north/east.
    let yaw_n = info.yaw.rem_euclid(360.0);
    let (facing, axis) = match (((yaw_n / 90.0) + 0.5) as i32) & 3 {
        0 => ("south", "Towards positive Z"),
        1 => ("west", "Towards negative X"),
        2 => ("north", "Towards negative Z"),
        _ => ("east", "Towards positive X"),
    };

    let grounded = if info.on_ground { "yes" } else { "no" };
    let left = [
        format!("ReCraft  {fps:.0} fps"),
        format!("XYZ: {fx:.3} / {fy:.3} / {fz:.3}"),
        format!("Block: {bx} {by} {bz}  (grounded: {grounded})"),
        format!("Chunk: {rx} {ry} {rz} in {cx} {cz}"),
        format!("Facing: {facing} ({axis}) ({yaw_n:.1} / {:.1})", info.pitch),
        format!("Chunks: {chunk_count}"),
    ];

    let s = &info.stats;
    let frame_ms = if fps > 0.0 { 1000.0 / fps } else { 0.0 };
    let gpu = if s.gpu_us > 0 {
        format!("gpu: {:.2} ms", s.gpu_us as f32 / 1000.0)
    } else {
        "gpu: n/a".to_owned()
    };
    let right = [
        format!("frame: {frame_ms:.2} ms ({fps:.0} fps)"),
        gpu,
        format!("cpu prepare {}us  encode {}us", s.prepare_us, s.encode_us),
        format!("submit {}us  acquire {}us", s.submit_us, s.acquire_us),
        format!("draws {}  visible {}", s.draw_calls, s.visible_chunks),
        format!("tris {}", s.chunk_indices / 3),
    ];

    for (i, text) in left.iter().enumerate() {
        let y = margin + i as i32 * line;
        let w = text_width(text, scale);
        ui.rect(
            UiRect::new(margin - scale, y - scale, w + 2 * scale, line),
            DEBUG_BG,
        );
        ui.text_shadowed(margin, y, scale, faded_white(1.0), text.clone());
    }
    for (i, text) in right.iter().enumerate() {
        let y = margin + i as i32 * line;
        let w = text_width(text, scale);
        let x = width - margin - w;
        ui.rect(
            UiRect::new(x - scale, y - scale, w + 2 * scale, line),
            DEBUG_BG,
        );
        ui.text_shadowed(x, y, scale, MUTED, text.clone());
    }
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
    let scale = gui_scale(width, height);
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
            draw_item_icon(ui, cell, item, scale.max(2), false);
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

    // Armor bar, left-aligned above health (only when wearing armor).
    let armor = hud.armor.clamp(0, 20);
    if armor > 0 {
        let armor_y = row_y - 10 * scale;
        for i in 0..10 {
            let dst = UiRect::new(layout.x0 + i * step, armor_y, icon, icon);
            ui.image(dst, GuiTexture::Icons, 16, 9, 9, 9); // empty
            match armor - i * 2 {
                n if n >= 2 => ui.image(dst, GuiTexture::Icons, 34, 9, 9, 9), // full
                1 => ui.image(dst, GuiTexture::Icons, 25, 9, 9, 9),           // half
                _ => {}
            }
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
/// Draw a slot's item. Block items with real geometry become 3D cubes (queued
/// for the GPU cube pass); everything else is a flat icon. The stack count
/// always goes to the overlay layer so it stays on top of the cube. `overlay`
/// routes flat icons to the foreground too (for the cursor-carried stack, which
/// must draw over the slots).
pub(crate) fn draw_item_icon(
    ui: &mut UiFrame,
    rect: UiRect,
    item: &SlotItem,
    text_scale: i32,
    overlay: bool,
) {
    if let Some((block_id, meta)) = recraft_render::gui_item::is_block_icon(item.id, item.damage) {
        ui.block_item(rect, block_id, meta);
    } else if overlay {
        ui.overlay_item_icon(rect, item.id);
    } else {
        ui.item_icon(rect, item.id);
    }
    if item.count > 1 {
        let label = format!("{}", item.count);
        let w = text_width(&label, text_scale);
        ui.overlay_text_shadowed(
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
fn draw_chat(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState, input: Option<&mut TextInput>) {
    let scale = gui_scale(width, height);
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
        let text_x = bar.x + pad;
        let text_y = bar.y + pad + scale;
        // Render committed text with the active IME composition spliced in at
        // the caret, plus a trailing "_" caret marker.
        let (before, preedit, after) = input.segments();
        let display = format!("{before}{preedit}_{after}");
        let prefix = format!("{before}{preedit}");
        let visible = trim_to_tail(&display, wrap_width, scale);
        ui.text_shadowed(text_x, text_y, scale, faded_white(1.0), visible);
        // Anchor the IME candidate window at the caret (physical px).
        let prefix_visible = trim_to_tail(&prefix, wrap_width, scale);
        let caret_x = text_x + text_width(&prefix_visible, scale);
        input.set_caret_area(caret_x, bar.y, 2 * scale, bar.height);
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

/// Center-screen title + subtitle with vanilla fade timing.
fn draw_title(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let Some(title) = hud.title else {
        return;
    };
    let scale = gui_scale(width, height);
    let alpha = (255.0 * title.alpha).round().clamp(0.0, 255.0) as u8;
    if alpha <= 8 {
        return;
    }
    let color = UiColor::rgba(255, 255, 255, alpha);
    let center_x = width / 2;
    let center_y = height / 2;
    let title_scale = 4 * scale;
    let subtitle_scale = 2 * scale;
    let title_w = text_width(title.title, title_scale);
    let subtitle_w = text_width(title.subtitle, subtitle_scale);
    ui.text_shadowed(
        center_x - title_w / 2,
        center_y - 10 * scale,
        title_scale,
        color,
        title.title,
    );
    ui.text_shadowed(
        center_x - subtitle_w / 2,
        center_y + 5 * scale,
        subtitle_scale,
        color,
        title.subtitle,
    );
}

/// The action-bar text (chat position 2) centered above the hotbar.
fn draw_action_bar(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    let Some((text, alpha)) = hud.chat.action_bar(Instant::now()) else {
        return;
    };
    let scale = gui_scale(width, height);
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
    let scale = gui_scale(width, height);
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

/// Connection-bar sprite row in icons.png for a latency (vanilla
/// `GuiPlayerTabOverlay.drawPing`): 0 = full (green) … 5 = no signal.
fn ping_bars_index(ping: i32) -> u32 {
    match ping {
        p if p < 0 => 5,
        p if p < 150 => 0,
        p if p < 300 => 1,
        p if p < 600 => 2,
        p if p < 1000 => 3,
        _ => 4,
    }
}

/// The tab player-list overlay (vanilla `GuiPlayerTabOverlay`): a translucent
/// panel of player names laid out in up-to-20-row columns, each row showing a
/// connection-bar icon, with the server header above and footer below.
fn draw_tab_list(ui: &mut UiFrame, width: i32, height: i32, hud: &HudState) {
    if !hud.tab_open {
        return;
    }
    let players = hud.player_list.sorted();
    if players.is_empty() {
        return;
    }
    let scale = gui_scale(width, height);

    // Display text per player: explicit display name, else team-decorated name.
    let names: Vec<String> = players
        .iter()
        .map(|p| match &p.display_name {
            Some(json) => chat::flatten_chat_json(json),
            None => hud.scoreboard.decorate_entry(&p.name),
        })
        .collect();

    // Column layout: at most 20 rows per column (vanilla).
    let count = players.len();
    let mut columns = 1usize;
    let mut rows = count;
    while rows > 20 {
        columns += 1;
        rows = count.div_ceil(columns);
    }

    let line_h = 9 * scale;
    let ping_w = 10 * scale;
    let cell_gap = 5 * scale; // space between name and the ping bar
    let col_gap = 6 * scale; // space between columns

    // Per-column name width, then per-column total cell width.
    let mut col_name_w = vec![0i32; columns];
    for (i, name) in names.iter().enumerate() {
        let col = i / rows;
        col_name_w[col] = col_name_w[col].max(text_width(name, scale));
    }
    let col_w: Vec<i32> = col_name_w.iter().map(|w| w + cell_gap + ping_w).collect();
    // Left offset of each column inside the grid.
    let mut col_x = vec![0i32; columns];
    for c in 1..columns {
        col_x[c] = col_x[c - 1] + col_w[c - 1] + col_gap;
    }
    let grid_w: i32 = col_w.iter().sum::<i32>() + col_gap * (columns as i32 - 1);
    let grid_h = rows as i32 * line_h;

    let header_lines: Vec<&str> = split_nonempty(&hud.player_list.header);
    let footer_lines: Vec<&str> = split_nonempty(&hud.player_list.footer);
    let gap = line_h / 2;
    let header_block = if header_lines.is_empty() {
        0
    } else {
        header_lines.len() as i32 * line_h + gap
    };
    let footer_block = if footer_lines.is_empty() {
        0
    } else {
        footer_lines.len() as i32 * line_h + gap
    };

    // Content width is the widest of the grid and any header/footer line.
    let mut content_w = grid_w;
    for line in header_lines.iter().chain(footer_lines.iter()) {
        content_w = content_w.max(text_width(line, scale));
    }

    let pad = 2 * scale;
    let center_x = width / 2;
    let block_h = header_block + grid_h + footer_block;
    let top = ((height - block_h) / 2).max(2 * scale);

    // One translucent backing panel (vanilla draws faint per-cell rects; a
    // single panel reads the same and keeps the columns legible).
    let bg_w = content_w + 2 * pad;
    ui.rect(
        UiRect::new(center_x - bg_w / 2, top - pad, bg_w, block_h + 2 * pad),
        BLACK_170,
    );

    let mut y = top;
    for line in &header_lines {
        let w = text_width(line, scale);
        ui.text_shadowed(center_x - w / 2, y + scale, scale, faded_white(1.0), *line);
        y += line_h;
    }
    if !header_lines.is_empty() {
        y += gap;
    }

    let grid_x = center_x - grid_w / 2;
    for (i, (player, name)) in players.iter().zip(names.iter()).enumerate() {
        let col = i / rows;
        let row = (i % rows) as i32;
        let cell_x = grid_x + col_x[col];
        let cell_y = y + row * line_h;
        ui.text_shadowed(cell_x, cell_y + scale, scale, faded_white(1.0), name.clone());
        let bar_x = cell_x + col_w[col] - ping_w;
        ui.image(
            UiRect::new(bar_x, cell_y + scale, ping_w, 8 * scale),
            GuiTexture::Icons,
            0,
            176 + ping_bars_index(player.ping) * 8,
            10,
            8,
        );
    }
    y += grid_h;

    if !footer_lines.is_empty() {
        y += gap;
        for line in &footer_lines {
            let w = text_width(line, scale);
            ui.text_shadowed(center_x - w / 2, y + scale, scale, faded_white(1.0), *line);
            y += line_h;
        }
    }
}

/// Split flattened header/footer text into lines, dropping an all-empty result.
fn split_nonempty(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n').collect()
}

fn draw_screen_overlay(
    ui: &mut UiFrame,
    width: i32,
    height: i32,
    overlay: ScreenOverlay,
    textures: &OverlayTextures,
) {
    let full = UiRect::new(0, 0, width, height);
    match overlay {
        ScreenOverlay::None => {}
        ScreenOverlay::Water => {
            // Vanilla renders misc/underwater.png tiled at ~10% alpha plus blue
            // fog. A semi-transparent blue rect approximates the combined effect.
            ui.rect(full, UiColor::rgba(0, 10, 40, 160));
        }
        ScreenOverlay::Lava => {
            if let Some(tex) = &textures.lava {
                ui.raw_image(full, tex.clone());
            } else {
                ui.rect(full, UiColor::rgba(207, 85, 0, 230));
            }
        }
        ScreenOverlay::Fire => {
            if let Some(tex) = &textures.fire {
                // Two overlapping quads covering the lower ~60% of the screen,
                // offset horizontally like vanilla's first-person fire.
                let fire_h = height * 6 / 10;
                let top = height - fire_h;
                let off = width / 8;
                ui.raw_image(UiRect::new(-off, top, width, fire_h), tex.clone());
                ui.raw_image(UiRect::new(off, top, width, fire_h), tex.clone());
            } else {
                let fire_h = height * 6 / 10;
                let top = height - fire_h;
                let fire_rect = UiRect::new(0, top, width, fire_h);
                ui.gradient_rect(fire_rect, UiColor::rgba(220, 130, 0, 180), UiColor::rgba(200, 60, 0, 220));
            }
        }
    }
}
