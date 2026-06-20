//! The mod-management screen: lists every loaded mod, toggles each on/off by
//! clicking its row, and offers a hot-reload, an open-folder shortcut, and Done.
//! Reachable from both the title screen and the in-game pause menu.

use recraft_render::{UiColor, UiFrame, UiRect};

use super::main_menu::GuiMainMenu;
use super::ingame_menu::GuiIngameMenu;
use super::widgets::GuiButton;
use super::{
    draw_centered_text, draw_default_background, fit_text, DrawCtx, GuiAction, GuiScreen, ListView,
    ScreenCtx, TEXT_GRAY, TEXT_WHITE,
};
use crate::i18n::{tr, tr_args};

pub struct GuiModList {
    /// Whether we entered from the title screen (Back returns there) vs the
    /// in-game pause menu (Back returns to that, over the live world).
    from_main_menu: bool,
    scroll: i32,
    reload: Option<GuiButton>,
    open_folder: Option<GuiButton>,
    done: Option<GuiButton>,
    /// Hit-test rects captured during draw: (mod id, currently enabled, rect).
    row_rects: Vec<(String, bool, UiRect)>,
}

impl GuiModList {
    pub fn new(from_main_menu: bool) -> Self {
        Self {
            from_main_menu,
            scroll: 0,
            reload: None,
            open_folder: None,
            done: None,
            row_rects: Vec::new(),
        }
    }

    fn back_screen(&self) -> Box<dyn GuiScreen> {
        if self.from_main_menu {
            Box::new(GuiMainMenu::new())
        } else {
            Box::new(GuiIngameMenu::new())
        }
    }

    fn max_scroll(&self, list: &ListView, total_rows: i32) -> i32 {
        (total_rows - list.visible_rows()).max(0)
    }
}

impl GuiScreen for GuiModList {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        [&self.reload, &self.open_folder, &self.done]
            .iter()
            .any(|b| b.as_ref().is_some_and(|b| b.clicked(x, y)))
            || self.row_rects.iter().any(|(_, _, r)| r.contains(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(ui, ctx.width, 8 * s, s, TEXT_WHITE, &tr("recraft.mods.title"));
        let count = ctx.mods.len();
        let subtitle = if count == 0 {
            tr("recraft.mods.empty")
        } else {
            let enabled = ctx.mods.iter().filter(|m| m.enabled).count();
            tr_args("recraft.mods.enabledCount", &[&enabled.to_string(), &count.to_string()])
        };
        draw_centered_text(ui, ctx.width, 20 * s, s, TEXT_GRAY, &subtitle);

        let total_rows = ctx.mods.len() as i32;
        let list = ListView::new(ctx, 240, 36, 64);
        list.draw_background(ui);

        self.row_rects.clear();
        let visible = list.visible_rows();
        for vi in 0..visible {
            let idx = self.scroll + vi;
            if idx < 0 || idx >= total_rows {
                continue;
            }
            let m = &ctx.mods[idx as usize];
            let rect = list.row_rect(vi);
            self.row_rects.push((m.id.clone(), m.enabled, rect));
            if m.enabled {
                list.draw_selection(ui, rect);
            }

            let text_x = rect.x + 4 * s;
            let text_y = rect.y + 2 * s;
            let max_w = rect.width - 8 * s;

            // Title line: name (fallback to id), version, and the on/off state.
            let title_color = if m.enabled { TEXT_WHITE } else { TEXT_GRAY };
            let display_name = m.name.as_deref().unwrap_or(&m.id);
            let state = if m.enabled { format!("§a{}", tr("recraft.mods.on")) } else { format!("§7{}", tr("recraft.mods.off")) };
            let title = format!("{display_name} §8v{}  {state}", m.version);
            ui.text_shadowed(text_x, text_y, s, title_color, fit_text(&title, max_w, s));

            // Detail line: the description, or the capability list as a fallback.
            let detail = match &m.description {
                Some(d) if !d.is_empty() => d.clone(),
                _ if !m.capabilities.is_empty() => {
                    let caps: Vec<String> =
                        m.capabilities.iter().map(|c| format!("{c:?}")).collect();
                    caps.join(", ")
                }
                _ => m
                    .tier
                    .map(|t| format!("{t:?} mod"))
                    .unwrap_or_else(|| tr("recraft.mods.builtin")),
            };
            let detail_color = if m.enabled { TEXT_GRAY } else { UiColor::rgba(110, 110, 110, 255) };
            ui.text_shadowed(text_x, text_y + 12 * s, s, detail_color, fit_text(&detail, max_w, s));
        }
        list.draw_scrollbar(ui, self.scroll, self.max_scroll(&list, total_rows), total_rows);

        if count == 0 {
            draw_centered_text(
                ui,
                ctx.width,
                ctx.height / 2,
                s,
                TEXT_GRAY,
                &tr("recraft.mods.help"),
            );
        }

        // Bottom button row: Reload | Open Folder, then Done below.
        let cx = ctx.width / 2;
        let by = ctx.height - 52 * s;
        let reload = GuiButton::at_px(cx - 154 * s, by, 150 * s, s, tr("recraft.mods.reload"));
        let open = GuiButton::at_px(cx + 4 * s, by, 150 * s, s, tr("recraft.mods.openFolder"));
        let done = GuiButton::at_px(cx - 100 * s, by + 24 * s, 200 * s, s, tr("gui.done"));
        reload.draw(ui, s, ctx.mouse, ctx.mouse_down);
        open.draw(ui, s, ctx.mouse, ctx.mouse_down);
        done.draw(ui, s, ctx.mouse, ctx.mouse_down);
        self.reload = Some(reload);
        self.open_folder = Some(open);
        self.done = Some(done);
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.done.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::SetScreen(self.back_screen())];
        }
        if self.reload.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::ReloadMods];
        }
        if self.open_folder.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::OpenModsFolder];
        }
        for (id, enabled, rect) in &self.row_rects {
            if rect.contains(x, y) {
                return vec![GuiAction::SetModEnabled(id.clone(), !enabled)];
            }
        }
        Vec::new()
    }

    fn mouse_scrolled(&mut self, delta: f32) {
        self.scroll = (self.scroll - delta.signum() as i32).max(0);
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
                return vec![GuiAction::SetScreen(self.back_screen())];
            }
        }
        Vec::new()
    }

    fn draws_over_hud(&self) -> bool {
        !self.from_main_menu
    }
}
