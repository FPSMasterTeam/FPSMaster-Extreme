use std::path::PathBuf;

use recraft_render::{discover_resource_packs, PackMeta, UiFrame, UiRect};

use super::options::GuiOptions;
use super::widgets::GuiButton;
use super::{
    draw_centered_text, draw_default_background, fit_text, DrawCtx, GuiAction, GuiScreen,
    ListView, ScreenCtx, TEXT_GRAY, TEXT_WHITE,
};
use crate::settings::Settings;

struct PackEntry {
    name: String,
    #[allow(dead_code)]
    path: PathBuf,
    meta: PackMeta,
}

pub struct GuiResourcePacks {
    packs: Vec<PackEntry>,
    selected: usize,
    scroll: i32,
    done: Option<GuiButton>,
    from_main_menu: bool,
    active_pack: Option<String>,
    row_rects: Vec<(usize, UiRect)>,
}

impl GuiResourcePacks {
    pub fn new(from_main_menu: bool, settings: &Settings) -> Self {
        let packs_dir = PathBuf::from("resourcepacks");
        let discovered = discover_resource_packs(&packs_dir);
        let packs: Vec<PackEntry> = discovered
            .into_iter()
            .map(|(name, path, meta)| PackEntry { name, path, meta })
            .collect();
        let selected = match &settings.resource_pack {
            None => 0,
            Some(active) => packs
                .iter()
                .position(|p| p.name == *active)
                .map(|i| i + 1)
                .unwrap_or(0),
        };
        Self {
            active_pack: settings.resource_pack.clone(),
            packs,
            selected,
            scroll: 0,
            done: None,
            from_main_menu,
            row_rects: Vec::new(),
        }
    }

    fn total_rows(&self) -> i32 {
        1 + self.packs.len() as i32
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

impl GuiScreen for GuiResourcePacks {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.done.as_ref().is_some_and(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 8 * s, s, TEXT_WHITE, "Resource Packs");

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
            let text_x = rect.x + 4 * s;
            let text_y = rect.y + 2 * s;
            let max_w = rect.width - 8 * s;
            if idx == 0 {
                let label = fit_text("Default (Vanilla 1.8)", max_w, s);
                ui.text_shadowed(text_x, text_y, s, TEXT_WHITE, &label);
                ui.text_shadowed(text_x, text_y + 12 * s, s, TEXT_GRAY, "No resource pack");
            } else {
                let pack = &self.packs[idx as usize - 1];
                let label = fit_text(&pack.name, max_w, s);
                ui.text_shadowed(text_x, text_y, s, TEXT_WHITE, &label);
                let info = format!("Format: {}  {}", pack.meta.pack_format, pack.meta.description);
                let info = fit_text(&info, max_w, s);
                ui.text_shadowed(text_x, text_y + 12 * s, s, TEXT_GRAY, &info);
            }
        }
        list.draw_scrollbar(ui, self.scroll, self.max_scroll(&list), self.total_rows());

        let bx = (ctx.width - 200 * s) / 2;
        let by = ctx.height - 52 * s;
        self.done = Some(GuiButton::at_px(bx, by, 200 * s, s, "Done"));
        if let Some(done) = &self.done {
            done.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.done.as_ref().is_some_and(|b| b.clicked(x, y)) {
            let new_pack = if self.selected == 0 {
                None
            } else {
                Some(self.packs[self.selected - 1].name.clone())
            };
            let changed = new_pack != self.active_pack;
            ctx.settings.resource_pack = new_pack.clone();
            let mut actions = vec![GuiAction::SaveSettings];
            if changed {
                let path = new_pack.map(|name| PathBuf::from("resourcepacks").join(name));
                actions.push(GuiAction::ReloadResourcePack(path));
            }
            actions.push(GuiAction::SetScreen(self.back_screen()));
            return actions;
        }
        for &(idx, rect) in &self.row_rects {
            if rect.contains(x, y) {
                self.selected = idx;
                return Vec::new();
            }
        }
        Vec::new()
    }

    fn mouse_scrolled(&mut self, delta: f32) {
        self.scroll = (self.scroll - delta.signum() as i32).max(0);
    }

    fn key_pressed(&mut self, event: &winit::event::KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == winit::event::ElementState::Pressed {
            if let winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Escape) = event.physical_key {
                return vec![GuiAction::SetScreen(self.back_screen())];
            }
        }
        Vec::new()
    }

    fn draws_over_hud(&self) -> bool {
        !self.from_main_menu
    }
}
