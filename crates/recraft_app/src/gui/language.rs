//! The language-selection screen (vanilla `GuiLanguage`): a scrollable list of
//! every language found in the assets. Picking one applies it live (the whole
//! UI re-renders translated) and persists the choice on Done/ESC.

use recraft_render::{UiFrame, UiRect};

use super::options::GuiOptions;
use super::widgets::GuiButton;
use super::{
    draw_centered_text, draw_default_background, fit_text, DrawCtx, GuiAction, GuiScreen, ListView,
    ScreenCtx, TEXT_GRAY, TEXT_WHITE,
};
use crate::i18n::{self, tr, LangInfo};

pub struct GuiLanguage {
    languages: Vec<LangInfo>,
    selected: usize,
    scroll: i32,
    done: Option<GuiButton>,
    from_main_menu: bool,
    row_rects: Vec<(usize, UiRect)>,
}

impl GuiLanguage {
    pub fn new(from_main_menu: bool) -> Self {
        let languages = i18n::available_languages();
        let active = i18n::current_language();
        let selected = languages
            .iter()
            .position(|l| l.code == active)
            .unwrap_or(0);
        Self {
            languages,
            selected,
            scroll: 0,
            done: None,
            from_main_menu,
            row_rects: Vec::new(),
        }
    }

    fn total_rows(&self) -> i32 {
        self.languages.len() as i32
    }

    fn max_scroll(&self, list: &ListView) -> i32 {
        (self.total_rows() - list.visible_rows()).max(0)
    }

    fn back_screen(&self) -> Box<dyn GuiScreen> {
        if self.from_main_menu {
            Box::new(GuiOptions::from_main_menu())
        } else {
            Box::new(GuiOptions::new())
        }
    }
}

impl GuiScreen for GuiLanguage {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.done.as_ref().is_some_and(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 8 * s, s, TEXT_WHITE, &tr("recraft.language.title"));

        let list = ListView::new(ctx, 200, 36, 64);
        list.draw_background(ui);

        self.row_rects.clear();
        let visible = list.visible_rows();
        for vi in 0..visible {
            let idx = self.scroll + vi;
            if idx < 0 || idx >= self.total_rows() {
                continue;
            }
            let rect = list.row_rect(vi);
            self.row_rects.push((idx as usize, rect));
            if idx as usize == self.selected {
                list.draw_selection(ui, rect);
            }
            let lang = &self.languages[idx as usize];
            let text_x = rect.x + 4 * s;
            let text_y = rect.y + 2 * s;
            let max_w = rect.width - 8 * s;
            let name = fit_text(&lang.name, max_w, s);
            ui.text_shadowed(text_x, text_y, s, TEXT_WHITE, &name);
            let sub = fit_text(&format!("{} ({})", lang.region, lang.code), max_w, s);
            ui.text_shadowed(text_x, text_y + 12 * s, s, TEXT_GRAY, &sub);
        }
        list.draw_scrollbar(ui, self.scroll, self.max_scroll(&list), self.total_rows());

        let bx = (ctx.width - 200 * s) / 2;
        let by = ctx.height - 52 * s;
        self.done = Some(GuiButton::at_px(bx, by, 200 * s, s, tr("gui.done")));
        if let Some(done) = &self.done {
            done.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.done.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
        }
        for &(idx, rect) in &self.row_rects {
            if rect.contains(x, y) {
                self.selected = idx;
                // Apply the language immediately so the screen re-renders
                // translated, and record it for persistence.
                let code = self.languages[idx].code.clone();
                ctx.settings.language = code.clone();
                i18n::set_language(&code);
                return Vec::new();
            }
        }
        Vec::new()
    }

    fn mouse_scrolled(&mut self, delta: f32) {
        self.scroll = (self.scroll - delta.signum() as i32).clamp(0, i32::MAX);
    }

    fn key_pressed(
        &mut self,
        event: &winit::event::KeyEvent,
        _ctx: &mut ScreenCtx,
    ) -> Vec<GuiAction> {
        if event.state == winit::event::ElementState::Pressed {
            if let winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Escape) =
                event.physical_key
            {
                return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
            }
        }
        Vec::new()
    }

    fn draws_over_hud(&self) -> bool {
        !self.from_main_menu
    }
}
