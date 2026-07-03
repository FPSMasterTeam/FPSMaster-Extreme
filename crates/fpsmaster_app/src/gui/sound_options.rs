//! The "Music & Sound Options" screen (vanilla `GuiScreenOptionsSounds`): a
//! full-width Master Volume slider on top, then a two-column grid of the eight
//! per-category volume sliders (Music, Jukebox/Noteblocks, Weather, Blocks,
//! Hostile/Friendly Creatures, Players, Ambient/Environment), and a Done button.
//! Every slider shows its category name plus the percentage (or `OFF` at 0),
//! matching vanilla. Opened from the options screen's "Music & Sounds..." button.

use fpsmaster_render::{UiFrame, UiRect};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::options::{draw_slider, slider_fraction, GuiOptions};
use super::widgets::{GuiButton, BUTTON_HEIGHT};
use super::{draw_centered_text, draw_default_background, DrawCtx, GuiAction, GuiScreen, ScreenCtx};
use crate::i18n::tr;

/// One sound-volume slider: the Master slider plus the eight categories. The
/// order (after Master) mirrors vanilla `SoundCategory.values()` minus Master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slider {
    Master,
    Music,
    Record,
    Weather,
    Block,
    Hostile,
    Neutral,
    Player,
    Ambient,
}

/// The eight category sliders laid out in the two-column grid, in vanilla order.
const CATEGORIES: [Slider; 8] = [
    Slider::Music,
    Slider::Record,
    Slider::Weather,
    Slider::Block,
    Slider::Hostile,
    Slider::Neutral,
    Slider::Player,
    Slider::Ambient,
];

impl Slider {
    /// The i18n key for this slider's category label (vanilla `soundCategory.*`,
    /// with Master reusing the same namespace).
    fn label_key(self) -> &'static str {
        match self {
            Slider::Master => "soundCategory.master",
            Slider::Music => "soundCategory.music",
            Slider::Record => "soundCategory.record",
            Slider::Weather => "soundCategory.weather",
            Slider::Block => "soundCategory.block",
            Slider::Hostile => "soundCategory.hostile",
            Slider::Neutral => "soundCategory.neutral",
            Slider::Player => "soundCategory.player",
            Slider::Ambient => "soundCategory.ambient",
        }
    }

    /// The current 0..1 volume for this category from settings (doubles as the
    /// slider fill fraction).
    fn value(self, s: &crate::settings::Settings) -> f32 {
        match self {
            Slider::Master => s.master_volume,
            Slider::Music => s.music_volume,
            Slider::Record => s.record_volume,
            Slider::Weather => s.weather_volume,
            Slider::Block => s.block_volume,
            Slider::Hostile => s.hostile_volume,
            Slider::Neutral => s.neutral_volume,
            Slider::Player => s.player_volume,
            Slider::Ambient => s.ambient_volume,
        }
    }

    /// Store `value` (0..1) back into settings via the matching setter.
    fn set(self, s: &mut crate::settings::Settings, value: f32) {
        match self {
            Slider::Master => s.set_master_volume_from01(value),
            Slider::Music => s.set_music_volume_from01(value),
            Slider::Record => s.set_record_volume_from01(value),
            Slider::Weather => s.set_weather_volume_from01(value),
            Slider::Block => s.set_block_volume_from01(value),
            Slider::Hostile => s.set_hostile_volume_from01(value),
            Slider::Neutral => s.set_neutral_volume_from01(value),
            Slider::Player => s.set_player_volume_from01(value),
            Slider::Ambient => s.set_ambient_volume_from01(value),
        }
    }
}

/// Vanilla `getSoundVolume`: the label as `Category: 42%`, or `Category: OFF`
/// when the volume is exactly zero.
fn slider_label(slider: Slider, s: &crate::settings::Settings) -> String {
    let value = slider.value(s);
    let name = tr(slider.label_key());
    if value <= 0.0 {
        format!("{}: {}", name, tr("options.off"))
    } else {
        format!("{}: {}%", name, (value * 100.0) as i32)
    }
}

#[derive(Default)]
pub struct GuiSoundOptions {
    master_rect: UiRect,
    /// Rects for the eight category sliders, parallel to [`CATEGORIES`].
    category_rects: [UiRect; 8],
    done: Option<GuiButton>,
    dragging: Option<Slider>,
    from_main_menu: bool,
}

impl GuiSoundOptions {
    pub fn new(from_main_menu: bool) -> Self {
        Self {
            from_main_menu,
            ..Self::default()
        }
    }

    /// Done/ESC step back up to the options screen (preserving its origin).
    fn back_screen(&self) -> Box<dyn GuiScreen> {
        if self.from_main_menu {
            Box::new(GuiOptions::from_main_menu())
        } else {
            Box::new(GuiOptions::new())
        }
    }

    fn layout(&mut self, ctx: &DrawCtx) {
        let s = ctx.scale;
        // Two-column grid centered on the window: columns 160 GUI px apart,
        // each slider 150 GUI px wide (vanilla), rows 24 GUI px apart.
        let left = ctx.width / 2 - 155 * s;
        let col1 = ctx.width / 2 + 5 * s;
        let top = ctx.height / 6 - 12 * s;
        let h = BUTTON_HEIGHT * s;
        // Master spans the full width of both columns on the first row.
        self.master_rect = UiRect::new(left, top, 310 * s, h);
        // Categories start on row 1, filling the grid left-to-right, top-down.
        for (i, _) in CATEGORIES.iter().enumerate() {
            let grid = i + 2; // Master consumed grid cells 0 and 1.
            let x = if grid % 2 == 0 { left } else { col1 };
            let y = top + 24 * s * (grid as i32 >> 1);
            self.category_rects[i] = UiRect::new(x, y, 150 * s, h);
        }
        self.done = Some(GuiButton::at_px(
            ctx.width / 2 - 100 * s,
            ctx.height / 6 + 168 * s,
            200 * s,
            s,
            tr("gui.done"),
        ));
    }

    /// Which slider a click at (x, y) landed on, if any.
    fn slider_at(&self, x: f64, y: f64) -> Option<Slider> {
        if self.master_rect.contains(x, y) {
            return Some(Slider::Master);
        }
        CATEGORIES
            .iter()
            .zip(self.category_rects.iter())
            .find(|(_, rect)| rect.contains(x, y))
            .map(|(slider, _)| *slider)
    }

    /// The layout rect for a given slider (for drag fraction math).
    fn rect_for(&self, slider: Slider) -> UiRect {
        if slider == Slider::Master {
            return self.master_rect;
        }
        CATEGORIES
            .iter()
            .position(|s| *s == slider)
            .map(|i| self.category_rects[i])
            .unwrap_or_default()
    }

    fn apply_drag(&mut self, x: f64, ctx: &mut ScreenCtx) {
        if let Some(slider) = self.dragging {
            let fraction = slider_fraction(self.rect_for(slider), x);
            slider.set(ctx.settings, fraction);
        }
    }
}

impl GuiScreen for GuiSoundOptions {
    fn clicks_button(&self, x: f64, y: f64) -> bool {
        self.done.as_ref().is_some_and(|b| b.clicked(x, y))
    }

    fn draw(&mut self, ui: &mut UiFrame, ctx: &DrawCtx) {
        self.layout(ctx);
        draw_default_background(ui, ctx);
        let s = ctx.scale;
        draw_centered_text(
            ui,
            ctx.width,
            15 * s,
            s,
            super::TEXT_WHITE,
            &tr("options.sounds.title"),
        );

        draw_slider(
            ui,
            self.master_rect,
            s,
            ctx.settings.master_volume,
            &slider_label(Slider::Master, ctx.settings),
        );
        for (i, slider) in CATEGORIES.iter().enumerate() {
            draw_slider(
                ui,
                self.category_rects[i],
                s,
                slider.value(ctx.settings),
                &slider_label(*slider, ctx.settings),
            );
        }
        if let Some(done) = &self.done {
            done.draw(ui, s, ctx.mouse, ctx.mouse_down);
        }
    }

    fn mouse_clicked(&mut self, x: f64, y: f64, ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if let Some(slider) = self.slider_at(x, y) {
            self.dragging = Some(slider);
            self.apply_drag(x, ctx);
            return Vec::new();
        }
        if self.done.as_ref().is_some_and(|b| b.clicked(x, y)) {
            return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
        }
        Vec::new()
    }

    fn mouse_dragged(&mut self, x: f64, _y: f64, ctx: &mut ScreenCtx) {
        self.apply_drag(x, ctx);
    }

    fn mouse_released(
        &mut self,
        _x: f64,
        _y: f64,
        _right: bool,
        _ctx: &mut ScreenCtx,
    ) -> Vec<GuiAction> {
        // Persist the adjusted volumes when a drag ends (vanilla saves on every
        // change; once on release keeps disk writes cheap and is enough since
        // the settings snapshot is already live for playback).
        let action = if self.dragging.is_some() {
            vec![GuiAction::SaveSettings]
        } else {
            Vec::new()
        };
        self.dragging = None;
        action
    }

    fn key_pressed(&mut self, event: &KeyEvent, _ctx: &mut ScreenCtx) -> Vec<GuiAction> {
        if event.state == ElementState::Pressed
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
        {
            return vec![GuiAction::SaveSettings, GuiAction::SetScreen(self.back_screen())];
        }
        Vec::new()
    }

    fn draws_over_hud(&self) -> bool {
        !self.from_main_menu
    }
}
