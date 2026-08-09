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

use fpsmaster_render::{GuiTexture, UiColor, UiFrame, UiRect};
use fpsmaster_protocol::v1_8_9::packets::ServerboundPacket;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::container::{Container, WindowKind};

use super::ingame::draw_item_icon;
use super::{DrawCtx, GuiAction, GuiScreen, ScreenCtx};

/// Vanilla container title color (`0x404040`, no shadow).
const TITLE_COLOR: UiColor = UiColor::rgba(64, 64, 64, 255);

/// The player-preview pose for the inventory window (vanilla
/// `GuiInventory.drawEntityOnScreen`). All angles are degrees in the model's
/// own convention: `body_yaw` is the body twist, `net_head_yaw` is the head
/// turn relative to the body, `head_pitch` is the head tilt, and `tilt` is the
/// whole-model X lean applied during projection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewPose {
    pub body_yaw: f32,
    pub net_head_yaw: f32,
    pub head_pitch: f32,
    pub tilt: f32,
}

/// Where the preview biped sits in the window (vanilla offsets, all in GUI px
/// relative to the window origin `(guiLeft, guiTop)`): the model panel box and
/// the feet anchor `drawEntityOnScreen` draws at (`guiLeft+51, guiTop+75`).
const PREVIEW_PANEL: (i32, i32, i32, i32) = (26, 8, 75, 78); // x0, y0, x1, y1
const PREVIEW_ANCHOR_X: i32 = 51;
const PREVIEW_ANCHOR_Y: i32 = 75;
/// Vanilla `drawEntityOnScreen` look reference: the mouse is taken relative to a
/// point 50 GUI px above the feet (`guiTop + 75 - 50`).
const PREVIEW_LOOK_Y: i32 = PREVIEW_ANCHOR_Y - 50;
/// Vanilla `drawEntityOnScreen` scale.
pub const PREVIEW_SCALE: f32 = 30.0;

/// How far right the player-inventory window slides while potion effects are
/// active, in GUI px (vanilla `InventoryEffectRenderer.updateActivePotionEffects`:
/// `guiLeft = 160 + (width - xSize - 200) / 2`, i.e. centred + 60).
const EFFECT_PANEL_SHIFT: i32 = 60;

/// Whether the active-effect panel is drawn beside this window — only the
/// player inventory shows it, and only while an effect is active.
pub fn effect_panel_shown(container: &Container, has_effects: bool) -> bool {
    container.kind == WindowKind::Player && has_effects
}

/// The window origin (vanilla `guiLeft`/`guiTop`) in physical px: the panel is
/// centred, then shifted right by [`EFFECT_PANEL_SHIFT`] when the effect panel
/// takes the space on its left. Every consumer of the window position must go
/// through this — the player preview is projected from the same origin, so a
/// second, un-shifted copy of the centring maths left the biped drawn outside
/// the window whenever a potion was active.
pub fn window_origin(
    width: i32,
    height: i32,
    container: &Container,
    has_effects: bool,
    scale: i32,
) -> (i32, i32) {
    let mut x = (width - container.x_size * scale) / 2;
    let y = (height - container.y_size * scale) / 2;
    if effect_panel_shown(container, has_effects) {
        x += EFFECT_PANEL_SHIFT * scale;
    }
    (x, y)
}

/// The look pose from the cursor, mirroring vanilla `drawEntityOnScreen`:
/// `f = atan((anchorX - mouseX)/40)`, `f1 = atan((lookY - mouseY)/40)`, with
/// body yaw `f*20`, head total yaw `f*40` (so net head yaw `f*20`), head pitch
/// `-f1*20` and a whole-model lean of `-f1*20`. Mouse is in GUI px relative to
/// the window origin.
pub fn preview_pose(mouse_gui: (f32, f32), origin_gui: (f32, f32)) -> PreviewPose {
    let dx = (origin_gui.0 + PREVIEW_ANCHOR_X as f32) - mouse_gui.0;
    let dy = (origin_gui.1 + PREVIEW_LOOK_Y as f32) - mouse_gui.1;
    let f = (dx / 40.0).atan();
    let f1 = (dy / 40.0).atan();
    PreviewPose {
        body_yaw: f * 20.0,
        net_head_yaw: f * 20.0,
        head_pitch: -f1 * 20.0,
        tilt: -f1 * 20.0,
    }
}

/// The preview's scissor rect and feet anchor in physical px. `origin_px` is the
/// window origin `(guiLeft, guiTop)` in physical px and `scale` the GUI pixel
/// scale, so a GUI-px offset `o` maps to `origin_px + o*scale`. Returns
/// `(scissor[x,y,w,h], anchor[x,y], pixels_per_block)`.
pub fn preview_layout(origin_px: (i32, i32), scale: i32) -> ([u32; 4], [f32; 2], f32) {
    let (x0, y0, x1, y1) = PREVIEW_PANEL;
    let scissor = [
        (origin_px.0 + x0 * scale).max(0) as u32,
        (origin_px.1 + y0 * scale).max(0) as u32,
        ((x1 - x0) * scale).max(0) as u32,
        ((y1 - y0) * scale).max(0) as u32,
    ];
    let anchor = [
        (origin_px.0 + PREVIEW_ANCHOR_X * scale) as f32,
        (origin_px.1 + PREVIEW_ANCHOR_Y * scale) as f32,
    ];
    // Vanilla scales the entity (1 unit = 1 block) by `PREVIEW_SCALE`, so one
    // model block spans `PREVIEW_SCALE` GUI px → `PREVIEW_SCALE * scale` physical.
    (scissor, anchor, PREVIEW_SCALE * scale as f32)
}

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
        // The world scrim is drawn centrally before the HUD (so the HUD stays
        // visible); here we only draw the inventory window + its contents.
        let Some(hud) = ctx.hud else { return };
        let Some(container) = hud.container else { return };

        let scale = ctx.scale;
        let pw = container.x_size * scale;
        let ph = container.y_size * scale;
        let show_effects = effect_panel_shown(container, !hud.effects.is_empty());
        let (px, py) =
            window_origin(ctx.width, ctx.height, container, !hud.effects.is_empty(), scale);
        // Cache the layout so the input handlers can map cursor → slot.
        self.px = px;
        self.py = py;
        self.scale = scale;

        // Dark fallback panel behind the texture (readable without assets).
        ui.rect(UiRect::new(px, py, pw, ph), UiColor::rgba(0, 0, 0, 210));
        draw_background(ui, container, px, py, scale);
        draw_progress(ui, container, px, py, scale);
        draw_titles(ui, container, px, py, scale);
        if show_effects {
            draw_active_potion_effects(ui, hud.effects, px, py, scale);
        }
        if container.kind == WindowKind::Enchant {
            let hovered = super::enchant::option_at(container, ctx.mouse, px, py, scale);
            super::enchant::draw_options(ui, container, px, py, scale, hovered);
        }
        if container.kind == WindowKind::Anvil {
            draw_anvil_field(ui, container, hud.inventory, px, py, scale);
        }
        if container.kind == WindowKind::Villager {
            super::merchant::draw_offer(ui, container, px, py, scale, ctx.mouse, hud.skin_rows);
        }

        let icon = 16 * scale;
        let hovered = self.slot_at(ctx.mouse, container);
        for (i, slot) in container.slots().iter().enumerate() {
            let cell = UiRect::new(px + slot.x * scale, py + slot.y * scale, icon, icon);
            if let Some(ref item) = container.slot_item(i, hud.inventory) {
                draw_item_icon(ui, cell, item, scale.max(2), false, hud.skin_rows);
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
            draw_item_icon(ui, cell, item, scale.max(2), true, hud.skin_rows);
        } else if let Some(slot) = hovered {
            // Hovering an item with an empty cursor: show the vanilla tooltip.
            if let Some(ref item) = container.slot_item(slot as usize, hud.inventory) {
                let lines = fpsmaster_render::build_tooltip(item);
                super::draw_tooltip(
                    ui,
                    &lines,
                    ctx.mouse.0 as i32,
                    ctx.mouse.1 as i32,
                    ctx.width,
                    ctx.height,
                    scale,
                );
            }
        } else if container.kind == WindowKind::Villager {
            // A villager trade preview item / deprecated arrow under the cursor.
            super::merchant::draw_tooltip(ui, container, ctx, px, py, scale);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        // Ignore a second button pressed while a paint-drag is in progress
        // (vanilla locks the drag to its owning button).
        if ctx.game.container_drag_active() {
            return Vec::new();
        }
        // Enchanting table: a left click on an enabled enchant option would send
        // a `C11 EnchantItem` packet in vanilla. That serverbound packet isn't
        // modelled in `fpsmaster_protocol`, so the click is a visual no-op here.
        // TODO: send the enchant action once `EnchantItem` exists in the protocol.
        if let Some(container) = ctx.game.open_container() {
            if container.kind == WindowKind::Enchant
                && super::enchant::option_at(container, (x, y), self.px, self.py, self.scale)
                    .is_some()
            {
                return Vec::new();
            }
            // A villager `< >` trade-selection button: change the selection and
            // ask the server (MC|TrSel) to fill the trade slots for that recipe.
            if container.kind == WindowKind::Villager {
                if let Some(button) =
                    super::merchant::button_at(container, (x, y), self.px, self.py, self.scale)
                {
                    let selected = container.selected_trade();
                    let index = match button {
                        super::merchant::TradeButton::Prev => selected.saturating_sub(1),
                        super::merchant::TradeButton::Next => selected + 1,
                    };
                    return match ctx.game.merchant_select(index) {
                        Some(packet) => vec![GuiAction::SendPacket(packet)],
                        None => Vec::new(),
                    };
                }
            }
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
        WindowKind::Anvil => GuiTexture::Anvil,
        WindowKind::Beacon => GuiTexture::Beacon,
        WindowKind::Villager => GuiTexture::Villager,
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
        crate::i18n::tr("container.inventory"),
    );
}

/// The anvil's rename field (vanilla `GuiRepair`): a recessed box at the top of
/// the window showing the left-input item's display name. Display-only — there
/// is no rename plumbing (no editable text input, no `C17 PluginMessage MC|ItemName`
/// send), so it just echoes the current name. TODO: make it an editable field.
fn draw_anvil_field(
    ui: &mut UiFrame,
    container: &Container,
    player: &[Option<fpsmaster_protocol::v1_8_9::packets::SlotItem>],
    px: i32,
    py: i32,
    scale: i32,
) {
    // Vanilla field: 110 px wide, 16 px tall, top-left at (26, 24).
    let field = UiRect::new(px + 26 * scale, py + 24 * scale, 110 * scale, 16 * scale);
    ui.rect(field, UiColor::rgba(0, 0, 0, 128));
    // The left input item (window slot 0) supplies the displayed name.
    if let Some(item) = container.slot_item(0, player) {
        let name = fpsmaster_render::item_display_name(item.id, item.damage);
        ui.text(field.x + 7 * scale, field.y + 4 * scale, scale, UiColor::rgba(224, 224, 224, 255), name);
    }
}

/// The active potion-effect panel beside the player inventory (vanilla
/// `InventoryEffectRenderer.drawActivePotionEffects`): one 140×32 GUI-px row per
/// effect, anchored 124 px to the left of the window, each showing the effect
/// icon, localized name (+ amplifier level) and remaining duration. Rows pack
/// tighter than 33 px once more than five effects are active.
fn draw_active_potion_effects(
    ui: &mut UiFrame,
    effects: &[(u8, i8, i32)],
    px: i32,
    py: i32,
    scale: i32,
) {
    let panel_x = px - 124 * scale;
    let step = if effects.len() > 5 {
        132 / (effects.len() as i32 - 1)
    } else {
        33
    };
    for (n, &(id, amplifier, duration)) in effects.iter().enumerate() {
        let Some((name, icon)) = potion_info(id) else {
            continue;
        };
        let row_y = py + n as i32 * step * scale;
        // Panel background sprite (inventory.png at 0,166 sized 140×32).
        ui.image(
            UiRect::new(panel_x, row_y, 140 * scale, 32 * scale),
            GuiTexture::Inventory,
            0,
            166,
            140,
            32,
        );
        // Status icon (18×18 from the icon grid that starts at y=198).
        if let Some((col, irow)) = icon {
            ui.image(
                UiRect::new(panel_x + 6 * scale, row_y + 7 * scale, 18 * scale, 18 * scale),
                GuiTexture::Inventory,
                col * 18,
                198 + irow * 18,
                18,
                18,
            );
        }
        let mut label = crate::i18n::localize_name(name);
        match amplifier {
            1 => label.push_str(" II"),
            2 => label.push_str(" III"),
            3 => label.push_str(" IV"),
            _ => {}
        }
        ui.text_shadowed(
            panel_x + 28 * scale,
            row_y + 6 * scale,
            scale,
            UiColor::rgba(255, 255, 255, 255),
            label,
        );
        ui.text_shadowed(
            panel_x + 28 * scale,
            row_y + 16 * scale,
            scale,
            UiColor::rgba(127, 127, 127, 255),
            ticks_to_elapsed(duration),
        );
    }
}

/// Display name and icon-grid `(column, row)` for a potion id (vanilla `Potion`),
/// or `None` for ids with no effect entry. The icon is `None` for the instant /
/// iconless potions (heal, harm, saturation).
fn potion_info(id: u8) -> Option<(&'static str, Option<(u32, u32)>)> {
    Some(match id {
        1 => ("Speed", Some((0, 0))),
        2 => ("Slowness", Some((1, 0))),
        3 => ("Haste", Some((2, 0))),
        4 => ("Mining Fatigue", Some((3, 0))),
        5 => ("Strength", Some((4, 0))),
        6 => ("Instant Health", None),
        7 => ("Instant Damage", None),
        8 => ("Jump Boost", Some((2, 1))),
        9 => ("Nausea", Some((3, 1))),
        10 => ("Regeneration", Some((7, 0))),
        11 => ("Resistance", Some((6, 1))),
        12 => ("Fire Resistance", Some((7, 1))),
        13 => ("Water Breathing", Some((0, 2))),
        14 => ("Invisibility", Some((0, 1))),
        15 => ("Blindness", Some((5, 1))),
        16 => ("Night Vision", Some((4, 1))),
        17 => ("Hunger", Some((1, 1))),
        18 => ("Weakness", Some((5, 0))),
        19 => ("Poison", Some((6, 0))),
        20 => ("Wither", Some((1, 2))),
        21 => ("Health Boost", Some((2, 2))),
        22 => ("Absorption", Some((2, 2))),
        23 => ("Saturation", None),
        _ => return None,
    })
}

/// Vanilla `StringUtils.ticksToElapsedTime`: `mm:ss` from a tick count.
fn ticks_to_elapsed(ticks: i32) -> String {
    let secs = ticks / 20;
    format!("{}:{:02}", secs / 60, secs % 60)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_on_the_anchor_yields_a_neutral_facing() {
        // The mouse sitting exactly on the model anchor (no offset) → zero yaw,
        // and on the look reference (50 px up) → zero pitch/tilt.
        let origin = (0.0, 0.0);
        let pose = preview_pose(
            (PREVIEW_ANCHOR_X as f32, PREVIEW_LOOK_Y as f32),
            origin,
        );
        assert!(pose.body_yaw.abs() < 1e-4, "{pose:?}");
        assert!(pose.net_head_yaw.abs() < 1e-4, "{pose:?}");
        assert!(pose.head_pitch.abs() < 1e-4, "{pose:?}");
        assert!(pose.tilt.abs() < 1e-4, "{pose:?}");
    }

    #[test]
    fn moving_the_cursor_turns_and_tilts_like_vanilla() {
        let origin = (10.0, 20.0);
        let anchor_x = origin.0 + PREVIEW_ANCHOR_X as f32;
        let look_y = origin.1 + PREVIEW_LOOK_Y as f32;
        // Cursor 40 px right of the anchor and 40 px below the look point.
        let mouse = (anchor_x + 40.0, look_y + 40.0);
        let pose = preview_pose(mouse, origin);
        // dx = -40, dy = -40 → f = atan(-1) = -π/4, so body_yaw = -π/4*20.
        let expected = (-1.0_f32).atan() * 20.0;
        assert!((pose.body_yaw - expected).abs() < 1e-4, "{pose:?}");
        assert!((pose.head_pitch - (-expected)).abs() < 1e-4, "{pose:?}");
        // Direct vanilla relations: head yaw equals body yaw, tilt equals pitch.
        assert!((pose.net_head_yaw - pose.body_yaw).abs() < 1e-4, "{pose:?}");
        assert!((pose.tilt - pose.head_pitch).abs() < 1e-4, "{pose:?}");
        // Right-and-below cursor: body turns negative (mirrors vanilla
        // `anchorX - mouseX`), head pitches/leans up (negative `-f1`, f1<0).
        assert!(pose.body_yaw < 0.0, "{pose:?}");
        assert!(pose.head_pitch > 0.0, "{pose:?}");
    }

    #[test]
    fn yaw_and_pitch_are_clamped_by_the_atan_curve() {
        // atan saturates near ±90°, so the *20 scaling caps the body yaw near
        // ±π/2*20 ≈ ±31.4° regardless of how far the cursor is dragged. A cursor
        // far to the right (`anchorX - mouseX` very negative) saturates negative.
        let pose = preview_pose((10_000.0, 0.0), (0.0, 0.0));
        assert!(pose.body_yaw <= -31.0 && pose.body_yaw >= -31.5, "{pose:?}");
    }

    #[test]
    fn the_effect_panel_shifts_the_window_and_the_preview_together() {
        // The preview is projected from `window_origin`, so both must see the
        // same 60-GUI-px shift; when they disagreed the biped drew outside the
        // window (and over the effect panel) whenever a potion was active.
        let player = Container::player();
        let (w, h, scale) = (1920, 1080, 3);
        let centred = window_origin(w, h, &player, false, scale);
        let shifted = window_origin(w, h, &player, true, scale);
        assert_eq!(shifted.0 - centred.0, EFFECT_PANEL_SHIFT * scale);
        assert_eq!(shifted.1, centred.1, "the shift is horizontal only");

        // The preview panel and feet anchor ride the shifted origin.
        let (scissor_a, anchor_a, _) = preview_layout(centred, scale);
        let (scissor_b, anchor_b, _) = preview_layout(shifted, scale);
        assert_eq!(scissor_b[0] - scissor_a[0], (EFFECT_PANEL_SHIFT * scale) as u32);
        assert_eq!(anchor_b[0] - anchor_a[0], (EFFECT_PANEL_SHIFT * scale) as f32);
    }

    #[test]
    fn only_the_player_window_makes_room_for_the_effect_panel() {
        // Server containers (chest here) never draw the panel, so they stay
        // centred even with effects running.
        let chest = Container::open(1, "minecraft:chest", "Chest".into(), 27);
        assert!(!effect_panel_shown(&chest, true));
        assert_eq!(
            window_origin(1920, 1080, &chest, true, 3),
            window_origin(1920, 1080, &chest, false, 3),
        );
        assert!(effect_panel_shown(&Container::player(), true));
        assert!(!effect_panel_shown(&Container::player(), false));
    }

    #[test]
    fn worn_equipment_still_clears_the_preview_panel() {
        // The preview is scissored to the panel box, so a layer taller than the
        // bare biped gets clipped rather than overflowing. The tallest is a worn
        // skull: `LayerCustomHead` grows the 8px head 1.1875× about the neck
        // pivot at model y 24, reaching 33.5 against the biped's 32.
        const WORN_HEAD_TOP_PX: f32 = 24.0 + 1.1875 * 8.0;
        for scale in 1..=4 {
            let (scissor, anchor, pixels_per_block) = preview_layout((0, 0), scale);
            let top = anchor[1] - (WORN_HEAD_TOP_PX / 16.0) * pixels_per_block;
            assert!(
                top >= scissor[1] as f32,
                "scale {scale}: a worn head reaches y={top}, above the panel top {}",
                scissor[1],
            );
        }
    }

    #[test]
    fn panel_layout_offsets_from_the_window_origin() {
        let (scissor, anchor, ppb) = preview_layout((100, 50), 2);
        // Panel box (26,8)->(75,78) GUI px, ×2 scale, offset by the origin.
        assert_eq!(scissor, [100 + 26 * 2, 50 + 8 * 2, (75 - 26) * 2, (78 - 8) * 2]);
        // Feet anchor at (51,75) GUI px ×2.
        assert_eq!(anchor, [(100 + 51 * 2) as f32, (50 + 75 * 2) as f32]);
        assert_eq!(ppb, PREVIEW_SCALE * 2.0);
    }
}
