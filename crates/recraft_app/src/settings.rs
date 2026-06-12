//! User-adjustable game settings (vanilla GameSettings) and the FPS counter.

use std::time::Instant;

/// User-adjustable options edited from the options screen.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Mouse sensitivity slider position in 0..=1 (0.5 == vanilla default).
    pub sensitivity: f32,
    /// Whether vertical sync (Fifo present mode) is enabled.
    pub vsync: bool,
    /// Frame-rate cap; `FPS_MAX` means unlimited.
    pub fps_cap: u32,
}

const FPS_MIN: u32 = 30;
const FPS_MAX: u32 = 260;
const FPS_STEP: u32 = 10;

impl Default for Settings {
    fn default() -> Self {
        Self {
            sensitivity: 0.5,
            vsync: true,
            fps_cap: 120,
        }
    }
}

impl Settings {
    /// Degrees of view rotation per pixel of mouse motion. Reproduces the
    /// vanilla curve so 0.5 maps to the long-standing 0.15 default.
    pub fn mouse_factor(self) -> f32 {
        let f = self.sensitivity * 0.6 + 0.2;
        f * f * f * 8.0 * 0.15
    }

    /// Sensitivity shown to the player as a 0..=200% value, vanilla-style.
    pub fn sensitivity_percent(self) -> f32 {
        self.sensitivity * 200.0
    }

    /// The active frame cap, or `None` when the slider is at "unlimited".
    pub fn fps_limit(self) -> Option<u32> {
        if self.fps_cap >= FPS_MAX {
            None
        } else {
            Some(self.fps_cap)
        }
    }

    pub fn fps_label(self) -> String {
        match self.fps_limit() {
            None => "Unlimited".to_owned(),
            Some(cap) => format!("{cap} FPS"),
        }
    }

    /// FPS slider fill fraction in 0..=1.
    pub fn fps_fraction(self) -> f32 {
        (self.fps_cap - FPS_MIN) as f32 / (FPS_MAX - FPS_MIN) as f32
    }

    pub fn set_sensitivity_from01(&mut self, value: f32) {
        self.sensitivity = value.clamp(0.0, 1.0);
    }

    pub fn set_fps_from01(&mut self, value: f32) {
        let span = (FPS_MAX - FPS_MIN) as f32;
        let raw = FPS_MIN as f32 + value.clamp(0.0, 1.0) * span;
        let stepped = (raw / FPS_STEP as f32).round() as u32 * FPS_STEP;
        self.fps_cap = stepped.clamp(FPS_MIN, FPS_MAX);
    }
}

#[derive(Debug)]
pub struct FpsCounter {
    frames: u32,
    last_sample: Instant,
    fps: f32,
}

impl FpsCounter {
    pub fn new(now: Instant) -> Self {
        Self {
            frames: 0,
            last_sample: now,
            fps: 0.0,
        }
    }

    pub fn tick(&mut self, now: Instant) {
        self.frames += 1;
        let elapsed = (now - self.last_sample).as_secs_f32();
        if elapsed >= 0.5 {
            self.fps = self.frames as f32 / elapsed;
            self.frames = 0;
            self.last_sample = now;
        }
    }

    pub fn fps(&self) -> f32 {
        self.fps
    }
}
