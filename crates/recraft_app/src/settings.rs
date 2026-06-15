//! User-adjustable game settings (vanilla GameSettings) and the FPS counter.

use std::time::Instant;

/// Where persisted options live (key=value text, next to the working directory,
/// like vanilla's options.txt).
const SETTINGS_FILE: &str = "recraft_options.txt";

/// User-adjustable options edited from the options screen.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Mouse sensitivity slider position in 0..=1 (0.5 == vanilla default).
    pub sensitivity: f32,
    /// Whether vertical sync (Fifo present mode) is enabled.
    pub vsync: bool,
    /// Frame-rate cap; `FPS_MAX` means unlimited.
    pub fps_cap: u32,
    /// 3D-world render resolution scale in `RENDER_SCALE_MIN..=1.0`. Below 1.0
    /// the world renders to a smaller off-screen target and is upscaled, cutting
    /// per-pixel fill/bandwidth (the main lever on weak iGPUs).
    pub render_scale: f32,
    /// Maximum horizontal chunk render distance in `RENDER_DIST_MIN..=RENDER_DIST_MAX`
    /// (vanilla "Render Distance"). Sections farther than this from the camera are
    /// skipped — the single biggest knob on weak hardware, cutting vertex, fill,
    /// draw-call and meshing cost together. The server still controls how much is
    /// loaded; this only bounds what is drawn.
    pub render_distance: u32,
    /// Auto-lower the render scale (within `RENDER_SCALE_MIN..=render_scale`) to keep
    /// the GPU frame time under the target budget on weak hardware — "it just runs"
    /// without manual tuning. Driven by the occlusion-proof GPU timestamp.
    pub adaptive_resolution: bool,
    /// Fancy graphics: sky gradient + see-through (alpha-blended) water. Off =
    /// flat horizon sky + opaque water, skipping the heaviest per-pixel work.
    pub fancy_graphics: bool,
    /// Block-atlas mipmap levels `0..=MIPMAP_MAX` (vanilla "Mipmap Levels"):
    /// 0 = off (full-res mip 0 only), 4 = full trilinear chain down to 1px.
    pub mipmap_levels: u32,
    /// Target physical render/present resolution, or `None` for the native
    /// window/display size. The swapchain present + desktop composite cost scales
    /// with this — the real lever on high-DPI screens where a "small" window is
    /// secretly huge in physical pixels.
    pub resolution: Option<(u32, u32)>,
    /// Exclusive fullscreen at the chosen resolution (bypasses the desktop
    /// compositor and lets the display hardware scale a lower mode — cheap present
    /// at a full-screen image). Off = windowed.
    pub fullscreen: bool,
    /// Shader pack master switch: per-pixel directional sun + ambient lighting.
    pub shaders: bool,
    /// Sun shadow map (only active while `shaders` is on).
    pub shader_shadows: bool,
    /// Specular highlights (only active while `shaders` is on).
    pub shader_specular: bool,
    /// Distance fog toward the sky horizon (independent of the master toggle).
    pub shader_fog: bool,
    /// Bloom glow around bright pixels (independent of the master toggle).
    pub shader_bloom: bool,
    /// Brightness gamma in 0..=1 (vanilla "Brightness"): 1.0 leaves lighting
    /// untouched, lower values darken the shadow/low-light end (not a flat
    /// multiply). Default sits below neutral so nights and caves read dark.
    pub brightness: f32,
    /// Post-process effects (applied in the tone-map pass / sky pass).
    pub post_vignette: bool,
    pub post_chromatic: bool,
    pub post_dof: bool,
    pub post_motion_blur: bool,
    pub post_auto_exposure: bool,
    pub volumetric_clouds: bool,
    /// Volumetric sun shafts (god rays); only active while `shaders` is on.
    pub volumetric_light: bool,
    /// Active resource pack name (subdirectory or zip filename under
    /// `resourcepacks/`), or `None` for default 1.8 textures.
    pub resource_pack: Option<String>,
}

/// Selectable resolutions (None = native). Common 16:9 modes most panels scale.
pub const RESOLUTION_PRESETS: [Option<(u32, u32)>; 6] = [
    None,
    Some((1920, 1080)),
    Some((1600, 900)),
    Some((1366, 768)),
    Some((1280, 720)),
    Some((960, 540)),
];

const FPS_MIN: u32 = 30;
const FPS_MAX: u32 = 260;
const FPS_STEP: u32 = 10;
pub const RENDER_SCALE_MIN: f32 = 0.5;
/// Render-distance bounds in chunks (vanilla spans 2..32; we cap lower to keep
/// the square cull cheap and the default modest for weak hardware).
pub const RENDER_DIST_MIN: u32 = 2;
pub const RENDER_DIST_MAX: u32 = 32;
/// Highest selectable mipmap level (16px tiles → mips 0..4).
pub const MIPMAP_MAX: u32 = 4;
// Brightness is a gamma knob (0 = darkest shadows, 1 = neutral), not a flat
// multiplier — it pulls the dark/low-light end down while leaving fully-lit
// surfaces alone.
const BRIGHTNESS_MIN: f32 = 0.0;
const BRIGHTNESS_MAX: f32 = 1.0;

impl Default for Settings {
    fn default() -> Self {
        Self {
            sensitivity: 0.5,
            vsync: true,
            fps_cap: 120,
            render_scale: 1.0,
            render_distance: 12,
            adaptive_resolution: false,
            fancy_graphics: true,
            mipmap_levels: MIPMAP_MAX,
            resolution: None,
            fullscreen: false,
            shaders: false,
            shader_shadows: true,
            shader_specular: true,
            shader_fog: false,
            shader_bloom: false,
            brightness: 0.5,
            post_vignette: true,
            post_chromatic: true,
            post_dof: false,
            post_motion_blur: false,
            post_auto_exposure: true,
            volumetric_clouds: true,
            volumetric_light: true,
            resource_pack: None,
        }
    }
}

impl Settings {
    /// Load persisted options from [`SETTINGS_FILE`], falling back to defaults
    /// for a missing file or any missing/invalid key. Values are clamped to
    /// their valid ranges so a hand-edited file can't put the game in a bad state.
    pub fn load() -> Self {
        Self::load_from(std::path::Path::new(SETTINGS_FILE))
    }

    fn load_from(path: &std::path::Path) -> Self {
        let mut s = Self::default();
        let (mut res_w, mut res_h) = (0u32, 0u32);
        let Ok(text) = std::fs::read_to_string(path) else {
            return s;
        };
        for line in text.lines() {
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            let val = val.trim();
            match key.trim() {
                "sensitivity" => {
                    if let Ok(v) = val.parse() {
                        s.sensitivity = v;
                    }
                }
                "vsync" => {
                    if let Ok(v) = val.parse() {
                        s.vsync = v;
                    }
                }
                "fps_cap" => {
                    if let Ok(v) = val.parse() {
                        s.fps_cap = v;
                    }
                }
                "render_scale" => {
                    if let Ok(v) = val.parse() {
                        s.render_scale = v;
                    }
                }
                "render_distance" => {
                    if let Ok(v) = val.parse() {
                        s.render_distance = v;
                    }
                }
                "adaptive_resolution" => {
                    if let Ok(v) = val.parse() {
                        s.adaptive_resolution = v;
                    }
                }
                "fancy_graphics" => {
                    if let Ok(v) = val.parse() {
                        s.fancy_graphics = v;
                    }
                }
                "mipmap_levels" => {
                    if let Ok(v) = val.parse() {
                        s.mipmap_levels = v;
                    }
                }
                "resolution_w" => {
                    if let Ok(v) = val.parse() {
                        res_w = v;
                    }
                }
                "resolution_h" => {
                    if let Ok(v) = val.parse() {
                        res_h = v;
                    }
                }
                "fullscreen" => {
                    if let Ok(v) = val.parse() {
                        s.fullscreen = v;
                    }
                }
                "shaders" => {
                    if let Ok(v) = val.parse() {
                        s.shaders = v;
                    }
                }
                "shader_shadows" => {
                    if let Ok(v) = val.parse() {
                        s.shader_shadows = v;
                    }
                }
                "shader_specular" => {
                    if let Ok(v) = val.parse() {
                        s.shader_specular = v;
                    }
                }
                "shader_fog" => {
                    if let Ok(v) = val.parse() {
                        s.shader_fog = v;
                    }
                }
                "shader_bloom" => {
                    if let Ok(v) = val.parse() {
                        s.shader_bloom = v;
                    }
                }
                "brightness" => {
                    if let Ok(v) = val.parse() {
                        s.brightness = v;
                    }
                }
                "post_vignette" => {
                    if let Ok(v) = val.parse() {
                        s.post_vignette = v;
                    }
                }
                "post_chromatic" => {
                    if let Ok(v) = val.parse() {
                        s.post_chromatic = v;
                    }
                }
                "post_dof" => {
                    if let Ok(v) = val.parse() {
                        s.post_dof = v;
                    }
                }
                "post_motion_blur" => {
                    if let Ok(v) = val.parse() {
                        s.post_motion_blur = v;
                    }
                }
                "post_auto_exposure" => {
                    if let Ok(v) = val.parse() {
                        s.post_auto_exposure = v;
                    }
                }
                "volumetric_clouds" => {
                    if let Ok(v) = val.parse() {
                        s.volumetric_clouds = v;
                    }
                }
                "volumetric_light" => {
                    if let Ok(v) = val.parse() {
                        s.volumetric_light = v;
                    }
                }
                "resource_pack" => {
                    if !val.is_empty() {
                        s.resource_pack = Some(val.to_owned());
                    }
                }
                _ => {}
            }
        }
        s.sensitivity = s.sensitivity.clamp(0.0, 1.0);
        s.fps_cap = s.fps_cap.clamp(FPS_MIN, FPS_MAX);
        s.render_scale = s.render_scale.clamp(RENDER_SCALE_MIN, 1.0);
        s.render_distance = s.render_distance.clamp(RENDER_DIST_MIN, RENDER_DIST_MAX);
        s.mipmap_levels = s.mipmap_levels.min(MIPMAP_MAX);
        s.brightness = s.brightness.clamp(BRIGHTNESS_MIN, BRIGHTNESS_MAX);
        s.resolution = if res_w > 0 && res_h > 0 {
            Some((res_w, res_h))
        } else {
            None
        };
        s
    }

    /// Write the current options to [`SETTINGS_FILE`]. Best-effort: a failure
    /// (e.g. read-only directory) is logged, not fatal.
    pub fn save(&self) {
        self.save_to(std::path::Path::new(SETTINGS_FILE));
    }

    fn save_to(&self, path: &std::path::Path) {
        let (res_w, res_h) = self.resolution.unwrap_or((0, 0));
        let text = format!(
            "sensitivity={}\nvsync={}\nfps_cap={}\nrender_scale={}\nrender_distance={}\nadaptive_resolution={}\nfancy_graphics={}\nmipmap_levels={}\nresolution_w={}\nresolution_h={}\nfullscreen={}\nshaders={}\nshader_shadows={}\nshader_specular={}\nshader_fog={}\nshader_bloom={}\nbrightness={}\npost_vignette={}\npost_chromatic={}\npost_dof={}\npost_motion_blur={}\npost_auto_exposure={}\nvolumetric_clouds={}\nvolumetric_light={}\nresource_pack={}\n",
            self.sensitivity,
            self.vsync,
            self.fps_cap,
            self.render_scale,
            self.render_distance,
            self.adaptive_resolution,
            self.fancy_graphics,
            self.mipmap_levels,
            res_w,
            res_h,
            self.fullscreen,
            self.shaders,
            self.shader_shadows,
            self.shader_specular,
            self.shader_fog,
            self.shader_bloom,
            self.brightness,
            self.post_vignette,
            self.post_chromatic,
            self.post_dof,
            self.post_motion_blur,
            self.post_auto_exposure,
            self.volumetric_clouds,
            self.volumetric_light,
            self.resource_pack.as_deref().unwrap_or(""),
        );
        if let Err(err) = std::fs::write(path, text) {
            log::warn!("failed to save settings to {}: {err}", path.display());
        }
    }

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

    /// Render-scale slider fill fraction in 0..=1.
    pub fn render_scale_fraction(self) -> f32 {
        (self.render_scale - RENDER_SCALE_MIN) / (1.0 - RENDER_SCALE_MIN)
    }

    pub fn render_scale_percent(self) -> u32 {
        (self.render_scale * 100.0).round() as u32
    }

    pub fn set_render_scale_from01(&mut self, value: f32) {
        let raw = RENDER_SCALE_MIN + value.clamp(0.0, 1.0) * (1.0 - RENDER_SCALE_MIN);
        // Snap to 5% steps for clean labels (50%, 55%, … 100%).
        self.render_scale = ((raw * 20.0).round() / 20.0).clamp(RENDER_SCALE_MIN, 1.0);
    }

    /// Render-distance slider fill fraction in 0..=1.
    pub fn render_distance_fraction(self) -> f32 {
        (self.render_distance - RENDER_DIST_MIN) as f32 / (RENDER_DIST_MAX - RENDER_DIST_MIN) as f32
    }

    pub fn set_render_distance_from01(&mut self, value: f32) {
        let span = (RENDER_DIST_MAX - RENDER_DIST_MIN) as f32;
        let raw = RENDER_DIST_MIN as f32 + value.clamp(0.0, 1.0) * span;
        self.render_distance = (raw.round() as u32).clamp(RENDER_DIST_MIN, RENDER_DIST_MAX);
    }

    /// Brightness slider fill fraction in 0..=1.
    pub fn brightness_fraction(self) -> f32 {
        (self.brightness - BRIGHTNESS_MIN) / (BRIGHTNESS_MAX - BRIGHTNESS_MIN)
    }

    pub fn brightness_percent(self) -> u32 {
        (self.brightness * 100.0).round() as u32
    }

    pub fn set_brightness_from01(&mut self, value: f32) {
        let raw = BRIGHTNESS_MIN + value.clamp(0.0, 1.0) * (BRIGHTNESS_MAX - BRIGHTNESS_MIN);
        // Snap to 5% steps for clean labels.
        self.brightness = ((raw * 20.0).round() / 20.0).clamp(BRIGHTNESS_MIN, BRIGHTNESS_MAX);
    }

    /// Advance the mipmap level, wrapping `0 → 1 → … → MIPMAP_MAX → 0`.
    pub fn cycle_mipmap_levels(&mut self) {
        self.mipmap_levels = if self.mipmap_levels >= MIPMAP_MAX {
            0
        } else {
            self.mipmap_levels + 1
        };
    }

    pub fn mipmap_label(self) -> String {
        if self.mipmap_levels == 0 {
            "OFF".to_owned()
        } else {
            self.mipmap_levels.to_string()
        }
    }

    /// Cycle through [`RESOLUTION_PRESETS`] (Native → 1080p → … → 540p → Native).
    pub fn cycle_resolution(&mut self) {
        let idx = RESOLUTION_PRESETS
            .iter()
            .position(|&r| r == self.resolution)
            .unwrap_or(0);
        self.resolution = RESOLUTION_PRESETS[(idx + 1) % RESOLUTION_PRESETS.len()];
    }

    pub fn resolution_label(self) -> String {
        match self.resolution {
            None => "Native".to_owned(),
            Some((w, h)) => format!("{w}x{h}"),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_disk() {
        let path = std::env::temp_dir().join("recraft_settings_round_trip.txt");
        let _ = std::fs::remove_file(&path);

        let mut original = Settings::default();
        original.sensitivity = 0.73;
        original.vsync = false;
        original.fps_cap = 60;
        original.render_scale = 0.7;
        original.fancy_graphics = false;
        original.mipmap_levels = 2;
        original.brightness = 0.55;
        original.save_to(&path);

        let loaded = Settings::load_from(&path);
        assert!((loaded.sensitivity - 0.73).abs() < 1e-6);
        assert!(!loaded.vsync);
        assert_eq!(loaded.fps_cap, 60);
        assert!((loaded.render_scale - 0.7).abs() < 1e-6);
        assert!(!loaded.fancy_graphics);
        assert_eq!(loaded.mipmap_levels, 2);
        assert!((loaded.brightness - 0.55).abs() < 1e-6);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = std::env::temp_dir().join("recraft_settings_does_not_exist.txt");
        let _ = std::fs::remove_file(&path);
        let loaded = Settings::load_from(&path);
        let default = Settings::default();
        assert_eq!(loaded.fps_cap, default.fps_cap);
        assert_eq!(loaded.mipmap_levels, default.mipmap_levels);
        assert!((loaded.render_scale - default.render_scale).abs() < 1e-6);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let path = std::env::temp_dir().join("recraft_settings_clamp.txt");
        std::fs::write(&path, "render_scale=5.0\nmipmap_levels=99\nfps_cap=1\n").unwrap();
        let loaded = Settings::load_from(&path);
        assert!(loaded.render_scale <= 1.0);
        assert!(loaded.mipmap_levels <= MIPMAP_MAX);
        assert!(loaded.fps_cap >= FPS_MIN);
        let _ = std::fs::remove_file(&path);
    }
}
