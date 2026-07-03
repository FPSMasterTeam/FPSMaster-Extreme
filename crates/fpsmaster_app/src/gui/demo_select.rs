use crate::game::DemoKind;

use super::main_menu::GuiMainMenu;
use super::widgets::GuiButton;
use super::{draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};

use fpsmaster_render::UiFrame;

use crate::i18n::tr;

pub struct GuiDemoSelect {
    buttons: Vec<GuiButton>,
}

impl GuiDemoSelect {
    pub fn new() -> Self {
        Self {
            buttons: Vec::new(),
        }
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        let cx = ctx.width / 2;
        let top = ctx.height / 4 + 24 * s;

        self.buttons = vec![
            GuiButton::at_px(cx - 100 * s, top, 200 * s, s, tr("fpsmaster.demo.landscape")),
            GuiButton::at_px(cx - 100 * s, top + 24 * s, 200 * s, s, tr("fpsmaster.demo.chunkStress")),
            GuiButton::at_px(cx - 100 * s, top + 48 * s, 200 * s, s, tr("fpsmaster.demo.entityStress")),
            GuiButton::at_px(cx - 100 * s, top + 84 * s, 200 * s, s, tr("gui.back")),
        ];
    }
}

impl GuiScreen for GuiDemoSelect {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.buttons.iter().any(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);

        let s = ctx.scale;
        let cx = ctx.width / 2;
        let title = tr("fpsmaster.demo.title");
        let tw = fpsmaster_render::text_width(&title, s);
        ui.text_shadowed(cx - tw / 2, ctx.height / 4, s, super::TEXT_WHITE, &title);

        for button in &self.buttons {
            button.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if self.buttons.len() < 4 {
            return Vec::new();
        }
        if self.buttons[0].clicked(x, y) {
            return vec![GuiAction::StartDemo(DemoKind::Landscape)];
        }
        if self.buttons[1].clicked(x, y) {
            return vec![GuiAction::StartDemo(DemoKind::ChunkStress)];
        }
        if self.buttons[2].clicked(x, y) {
            return vec![GuiAction::StartDemo(DemoKind::EntityStress)];
        }
        if self.buttons[3].clicked(x, y) {
            return vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))];
        }
        Vec::new()
    }

    fn key_pressed(
        &mut self,
        event: &winit::event::KeyEvent,
        _ctx: &mut ScreenCtx,
    ) -> Vec<GuiAction> {
        use winit::keyboard::{KeyCode, PhysicalKey};
        if event.state.is_pressed()
            && event.physical_key == PhysicalKey::Code(KeyCode::Escape)
        {
            return vec![GuiAction::SetScreen(Box::new(GuiMainMenu::new()))];
        }
        Vec::new()
    }
}
