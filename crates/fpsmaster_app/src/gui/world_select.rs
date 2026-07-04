//! Single-player world-selection screen. Replaces the old "launch a local Paper
//! server" flow: lists the player's saved worlds (name + seed), lets them create
//! a new one, and enters a built-in generated world with no server. The demo
//! worlds live here too, tucked in the bottom-right corner.

use fpsmaster_render::UiFrame;

use crate::worlds::{self, WorldEntry};

use super::demo_select::GuiDemoSelect;
use super::main_menu::GuiMainMenu;
use super::widgets::GuiButton;
use super::{draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

/// Worlds shown at once. No scrolling yet — a fresh install has none, and this
/// keeps the layout a simple vanilla-style button stack.
const MAX_ROWS: usize = 6;

pub struct GuiSelectWorld {
    worlds: Vec<WorldEntry>,
    /// Rebuilt every `draw` from the current window size: [create, world rows…,
    /// back, demo]. `shown` records how many world rows there are so click
    /// handling can index the trailing buttons.
    buttons: Vec<GuiButton>,
    shown: usize,
}

impl GuiSelectWorld {
    pub fn new() -> Self {
        Self {
            worlds: worlds::load(),
            buttons: Vec::new(),
            shown: 0,
        }
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let cx = ctx.width / 2;
        let top = ctx.height / 4 + 8 * s;

        self.shown = self.worlds.len().min(MAX_ROWS);
        let mut buttons = Vec::with_capacity(self.shown + 3);

        // [0] Create a new world.
        buttons.push(GuiButton::at_px(
            cx - 100 * s,
            top,
            200 * s,
            s,
            "Create New World",
        ));

        // [1..=shown] one button per saved world.
        for (i, world) in self.worlds.iter().take(self.shown).enumerate() {
            let y = top + (24 * (i as i32 + 1)) * s;
            buttons.push(GuiButton::at_px(cx - 100 * s, y, 200 * s, s, world.name.clone()));
        }

        // [shown+1] Back to the title screen.
        let back_y = top + (24 * (self.shown as i32 + 1) + 12) * s;
        buttons.push(GuiButton::at_px(cx - 100 * s, back_y, 200 * s, s, "Back"));

        // [shown+2] Demo worlds, tucked into the bottom-right corner.
        buttons.push(GuiButton::at_px(
            ctx.width - 82 * s,
            ctx.height - 24 * s,
            80 * s,
            s,
            "Demo",
        ));

        self.buttons = buttons;
    }

    fn back_index(&self) -> usize {
        self.shown + 1
    }

    fn demo_index(&self) -> usize {
        self.shown + 2
    }
}

impl GuiScreen for GuiSelectWorld {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);

        let s = ctx.scale;
        let cx = ctx.width / 2;
        let title = "Select World";
        let tw = fpsmaster_render::text_width(title, s);
        ui.text_shadowed(cx - tw / 2, ctx.height / 4 - 8 * s, s, super::TEXT_WHITE, title);

        if self.worlds.is_empty() {
            let hint = "No worlds yet — create one to start playing.";
            let hw = fpsmaster_render::text_width(hint, s);
            ui.text_shadowed(
                cx - hw / 2,
                ctx.height / 4 + 32 * s,
                s,
                super::TEXT_GRAY,
                hint,
            );
        }

        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.buttons.is_empty() {
            return Vec::new();
        }
        // [0] Create: make a new world, persist it, and enter it immediately.
        if self.buttons[0].clicked(x, y) {
            let entry = worlds::create(&mut self.worlds);
            return vec![GuiAction::StartLocalWorld { seed: entry.seed }];
        }
        // [1..=shown] enter an existing world (regenerated from its seed).
        for i in 0..self.shown {
            if self.buttons[i + 1].clicked(x, y) {
                return vec![GuiAction::StartLocalWorld {
                    seed: self.worlds[i].seed,
                }];
            }
        }
        if self.buttons[self.back_index()].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))];
        }
        if self.buttons[self.demo_index()].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiDemoSelect::new()))];
        }
        Vec::new()
    }

    fn key_pressed(
        &mut self,
        event: &winit::event::KeyEvent,
        _ctx: &mut ScreenCtx,
    ) -> Vec<GuiAction> {
        use winit::keyboard::{KeyCode, PhysicalKey};
        if event.state.is_pressed() && event.physical_key == PhysicalKey::Code(KeyCode::Escape) {
            return vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))];
        }
        Vec::new()
    }
}
