//! The Controls screen (vanilla "Controls"): a scrollable list of every
//! rebindable [`GameAction`] with its current key shown as a button. Clicking a
//! button enters "press a key" capture mode; the next key press rebinds that
//! action, persists the settings and returns to the list. A conflicting key
//! (bound to more than one action) is flagged in red.

use fpsmaster_render::{text_width, UiColor, UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::options::GuiOptions;
use super::widgets::GuiButton;
use super::{
    draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ListView,
    ScreenCtx, TEXT_WHITE,
};
use crate::settings::{keycode_name, GameAction};
use crate::i18n::tr;

const CONFLICT_RED: UiColor = UiColor::rgba(255, 85, 85, 255);

pub struct GuiControls {
    /// The action currently waiting for a key press, if in capture mode.
    binding: Option<GameAction>,
    scroll: i32,
    /// Per visible row: the action and the key-button rect, for hit-testing.
    row_buttons: Vec<(GameAction, UiRect)>,
    done: Option<GuiButton>,
    from_main_menu: bool,
}

impl GuiControls {
    pub fn new(from_main_menu: bool) -> Self {
        Self {
            binding: None,
            scroll: 0,
            row_buttons: Vec::new(),
            done: None,
            from_main_menu,
        }
    }

    fn back_screen(&self) -> Box<dyn GuiScreen> {
        if self.from_main_menu {
            Box::new(GuiOptions::from_main_menu())
        } else {
            Box::new(GuiOptions::new())
        }
    }

    fn total_rows(&self) -> i32 {
        GameAction::ALL.len() as i32
    }

    fn max_scroll(&self, list: &ListView) -> i32 {
        (self.total_rows() - list.visible_rows()).max(0)
    }
}

impl GuiScreen for GuiControls {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.done.as_ref().is_some_and(|b| b.clicked(x, y))
            || self.row_buttons.iter().any(|(_, r)| r.contains(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 8 * s, s, TEXT_WHITE, &tr("controls.title"));

        let list = ListView::new(ctx, 220, 24, 52);
        list.draw_background(ui);

        self.row_buttons.clear();
        let visible = list.visible_rows();
        for vi in 0..visible {
            let idx = self.scroll + vi;
            if idx < 0 || idx >= self.total_rows() {
                continue;
            }
            let action = GameAction::ALL[idx as usize];
            let rect = list.row_rect(vi);

            // Left: the action label, vertically centered in the row.
            let label = action.label();
            let conflict = ctx.settings.keybinds.conflict(action);
            let color = if conflict { CONFLICT_RED } else { TEXT_WHITE };
            let label_y = rect.y + (rect.height - 8 * s) / 2;
            ui.text_shadowed(rect.x + 4 * s, label_y, s, color, label);

            // Right: the key button. In capture mode it shows "> ... <".
            let btn_w = 90 * s;
            let btn_x = rect.x + rect.width - btn_w;
            let key_label = if self.binding == Some(action) {
                "> ? <".to_owned()
            } else {
                let name = keycode_name(ctx.settings.keybinds.key_for(action));
                if conflict {
                    format!("[{name}]")
                } else {
                    name
                }
            };
            let btn = GuiButton::at_px(btn_x, rect.y, btn_w, s, key_label);
            btn.draw(ui, s, ctx.mouse, ctx.mouse_down);
            self.row_buttons.push((action, btn.rect));
        }
        list.draw_scrollbar(ui, self.scroll, self.max_scroll(&list), self.total_rows());

        // A hint line while waiting for a key.
        if self.binding.is_some() {
            let hint = tr("fpsmaster.controls.bindHint");
            let w = text_width(&hint, s);
            ui.text_shadowed((ctx.width - w) / 2, ctx.height - 40 * s, s, CONFLICT_RED, &hint);
        }

        let bx = (ctx.width - 200 * s) / 2;
        let by = ctx.height - 28 * s;
        self.done = Some(GuiButton::at_px(bx, by, 200 * s, s, tr("gui.done")));
        if let Some(done) = &self.done {
            done.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        // A click while capturing cancels the capture (matches vanilla).
        if self.binding.is_some() {
            self.binding = None;
            return Vec::new();
        }
        if self.done.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
        }
        for &(action, rect) in &self.row_buttons {
            if rect.contains(x, y) {
                self.binding = Some(action);
                return Vec::new();
            }
        }
        Vec::new()
    }

    fn mouse_scrolled(&mut self, delta: f32) {
        self.scroll = (self.scroll - delta.signum() as i32).max(0);
    }

    fn key_pressed(&mut self, event: &KeyEvent, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state != ElementState::Pressed {
            return Vec::new();
        }
        let PhysicalKey::Code(code) = event.physical_key else {
            return Vec::new();
        };
        if let Some(action) = self.binding.take() {
            // Escape cancels the capture instead of binding to Escape (which is
            // the reserved pause key).
            if code != KeyCode::Escape {
                ctx.settings.keybinds.set(action, code);
                return vec![GuiAction::SaveSettings];
            }
            return Vec::new();
        }
        if code == KeyCode::Escape {
            return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
        }
        Vec::new()
    }

    fn draws_over_hud(&self) -> bool {
        !self.from_main_menu
    }
}
