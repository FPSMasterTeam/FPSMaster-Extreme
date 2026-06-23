//! User-adjustable game settings (vanilla GameSettings) and the FPS counter.

use std::time::Instant;

use winit::keyboard::KeyCode;

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
    /// Smooth lighting: per-vertex light + ambient occlusion (vanilla "Smooth
    /// Lighting"). OFF switches to flat per-face light with greedy-merged cube
    /// faces — far fewer triangles on open terrain, at the cost of AO/gradients.
    pub smooth_lighting: bool,
    /// Temporal anti-aliasing: jittered sampling + history reprojection, resolved
    /// against the motion-vector buffer. Suppresses the post motion blur while on.
    pub taa: bool,
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
    /// Show a small FPS readout in the top-left during gameplay (vanilla-style
    /// "Show FPS"). Off by default; the full F3 debug overlay is independent.
    pub show_fps: bool,
    /// Use the 1.7-style hand/arm animations (the "OldAnimations" mod): the
    /// older attack-swing curve and the 1.7 sword-block pose. Off by default
    /// (the 1.8 animations match the rest of the client).
    pub old_animations: bool,
    /// Active resource pack name (subdirectory or zip filename under
    /// `resourcepacks/`), or `None` for default 1.8 textures.
    pub resource_pack: Option<String>,
    /// Customizable key bindings (vanilla "Controls"). Maps each rebindable
    /// [`GameAction`] to a physical [`KeyCode`].
    pub keybinds: Keybinds,
    /// Ids of mods the user has disabled from the mod-management screen. Disabled
    /// mods load but their hooks are not dispatched. Persisted comma-separated.
    pub disabled_mods: Vec<String>,
    /// Active UI language code (`en_US`, `zh_CN`, …), matching a vanilla `.lang`
    /// file in the assets. Drives [`crate::i18n`].
    pub language: String,
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
            smooth_lighting: true,
            taa: false,
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
            show_fps: false,
            old_animations: false,
            resource_pack: None,
            keybinds: Keybinds::default(),
            disabled_mods: Vec::new(),
            language: crate::i18n::DEFAULT_LANG.to_owned(),
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
                "taa" => {
                    if let Ok(v) = val.parse() {
                        s.taa = v;
                    }
                }
                "smooth_lighting" => {
                    if let Ok(v) = val.parse() {
                        s.smooth_lighting = v;
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
                "show_fps" => {
                    if let Ok(v) = val.parse() {
                        s.show_fps = v;
                    }
                }
                "old_animations" => {
                    if let Ok(v) = val.parse() {
                        s.old_animations = v;
                    }
                }
                "resource_pack" => {
                    if !val.is_empty() {
                        s.resource_pack = Some(val.to_owned());
                    }
                }
                "language" => {
                    if !val.is_empty() {
                        s.language = val.to_owned();
                    }
                }
                "disabled_mods" => {
                    s.disabled_mods = val
                        .split(',')
                        .map(str::trim)
                        .filter(|m| !m.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                k => {
                    // Key bindings: `key.<action>=<KeyCode name>`.
                    if let Some(action_name) = k.strip_prefix("key.") {
                        if let (Some(action), Some(code)) =
                            (GameAction::from_name(action_name), keycode_from_name(val))
                        {
                            s.keybinds.set(action, code);
                        }
                    }
                }
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
        let mut text = format!(
            "sensitivity={}\nvsync={}\nfps_cap={}\nrender_scale={}\nrender_distance={}\nadaptive_resolution={}\nsmooth_lighting={}\nfancy_graphics={}\nmipmap_levels={}\nresolution_w={}\nresolution_h={}\nfullscreen={}\nshaders={}\nshader_shadows={}\nshader_specular={}\nshader_fog={}\nshader_bloom={}\nbrightness={}\npost_vignette={}\npost_chromatic={}\npost_dof={}\npost_motion_blur={}\npost_auto_exposure={}\nvolumetric_clouds={}\nvolumetric_light={}\nshow_fps={}\nold_animations={}\nresource_pack={}\n",
            self.sensitivity,
            self.vsync,
            self.fps_cap,
            self.render_scale,
            self.render_distance,
            self.adaptive_resolution,
            self.smooth_lighting,
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
            self.show_fps,
            self.old_animations,
            self.resource_pack.as_deref().unwrap_or(""),
        );
        text.push_str(&format!("taa={}\n", self.taa));
        text.push_str(&format!("disabled_mods={}\n", self.disabled_mods.join(",")));
        text.push_str(&format!("language={}\n", self.language));
        for &(action, code) in self.keybinds.iter() {
            text.push_str(&format!(
                "key.{}={}\n",
                action.name(),
                keycode_name(code)
            ));
        }
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
            None => crate::i18n::tr("options.framerateLimit.max"),
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
            crate::i18n::tr("options.off")
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
            None => crate::i18n::tr("recraft.options.native"),
            Some((w, h)) => format!("{w}x{h}"),
        }
    }
}

// ─── Key bindings (vanilla "Controls") ───────────────────────────────────────

/// A rebindable in-game control. The variants cover the movement keys, the
/// inventory/chat/hotbar set recraft actually uses, plus the vanilla controls
/// recraft does not yet act on (drop, swap-hands, screenshot, perspective) so
/// the binding can be configured ahead of the feature landing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameAction {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Sneak,
    Sprint,
    Attack,
    Use,
    Inventory,
    Chat,
    Command,
    PlayerList,
    Drop,
    SwapHands,
    Hotbar1,
    Hotbar2,
    Hotbar3,
    Hotbar4,
    Hotbar5,
    Hotbar6,
    Hotbar7,
    Hotbar8,
    Hotbar9,
    Screenshot,
    TogglePerspective,
    Debug,
}

impl GameAction {
    /// Every action in display/serialization order (also the order the controls
    /// screen lists them in).
    pub const ALL: [GameAction; 27] = [
        GameAction::Forward,
        GameAction::Back,
        GameAction::Left,
        GameAction::Right,
        GameAction::Jump,
        GameAction::Sneak,
        GameAction::Sprint,
        GameAction::Attack,
        GameAction::Use,
        GameAction::Inventory,
        GameAction::Chat,
        GameAction::Command,
        GameAction::PlayerList,
        GameAction::Drop,
        GameAction::SwapHands,
        GameAction::Hotbar1,
        GameAction::Hotbar2,
        GameAction::Hotbar3,
        GameAction::Hotbar4,
        GameAction::Hotbar5,
        GameAction::Hotbar6,
        GameAction::Hotbar7,
        GameAction::Hotbar8,
        GameAction::Hotbar9,
        GameAction::Screenshot,
        GameAction::TogglePerspective,
        GameAction::Debug,
    ];

    /// Stable identifier used in the options file (`key.<name>=…`).
    pub fn name(self) -> &'static str {
        match self {
            GameAction::Forward => "forward",
            GameAction::Back => "back",
            GameAction::Left => "left",
            GameAction::Right => "right",
            GameAction::Jump => "jump",
            GameAction::Sneak => "sneak",
            GameAction::Sprint => "sprint",
            GameAction::Attack => "attack",
            GameAction::Use => "use",
            GameAction::Inventory => "inventory",
            GameAction::Chat => "chat",
            GameAction::Command => "command",
            GameAction::PlayerList => "playerlist",
            GameAction::Drop => "drop",
            GameAction::SwapHands => "swaphands",
            GameAction::Hotbar1 => "hotbar.1",
            GameAction::Hotbar2 => "hotbar.2",
            GameAction::Hotbar3 => "hotbar.3",
            GameAction::Hotbar4 => "hotbar.4",
            GameAction::Hotbar5 => "hotbar.5",
            GameAction::Hotbar6 => "hotbar.6",
            GameAction::Hotbar7 => "hotbar.7",
            GameAction::Hotbar8 => "hotbar.8",
            GameAction::Hotbar9 => "hotbar.9",
            GameAction::Screenshot => "screenshot",
            GameAction::TogglePerspective => "perspective",
            GameAction::Debug => "debug",
        }
    }

    fn from_name(name: &str) -> Option<GameAction> {
        GameAction::ALL.into_iter().find(|a| a.name() == name)
    }

    /// Human-readable label for the controls screen.
    pub fn label(self) -> &'static str {
        match self {
            GameAction::Forward => "Walk Forwards",
            GameAction::Back => "Walk Backwards",
            GameAction::Left => "Strafe Left",
            GameAction::Right => "Strafe Right",
            GameAction::Jump => "Jump",
            GameAction::Sneak => "Sneak",
            GameAction::Sprint => "Sprint",
            GameAction::Attack => "Attack/Destroy",
            GameAction::Use => "Use Item/Place",
            GameAction::Inventory => "Open Inventory",
            GameAction::Chat => "Open Chat",
            GameAction::Command => "Open Command",
            GameAction::PlayerList => "List Players",
            GameAction::Drop => "Drop Item",
            GameAction::SwapHands => "Swap Item In Hands",
            GameAction::Hotbar1 => "Hotbar Slot 1",
            GameAction::Hotbar2 => "Hotbar Slot 2",
            GameAction::Hotbar3 => "Hotbar Slot 3",
            GameAction::Hotbar4 => "Hotbar Slot 4",
            GameAction::Hotbar5 => "Hotbar Slot 5",
            GameAction::Hotbar6 => "Hotbar Slot 6",
            GameAction::Hotbar7 => "Hotbar Slot 7",
            GameAction::Hotbar8 => "Hotbar Slot 8",
            GameAction::Hotbar9 => "Hotbar Slot 9",
            GameAction::Screenshot => "Take Screenshot",
            GameAction::TogglePerspective => "Toggle Perspective",
            GameAction::Debug => "Debug Info",
        }
    }

    fn default_key(self) -> KeyCode {
        match self {
            GameAction::Forward => KeyCode::KeyW,
            GameAction::Back => KeyCode::KeyS,
            GameAction::Left => KeyCode::KeyA,
            GameAction::Right => KeyCode::KeyD,
            GameAction::Jump => KeyCode::Space,
            GameAction::Sneak => KeyCode::ShiftLeft,
            GameAction::Sprint => KeyCode::ControlLeft,
            // Attack/Use are mouse buttons in vanilla; recraft drives them from
            // the mouse, so these keyboard defaults are unused (configurable,
            // not yet acted on). Kept distinct from Drop's Q to avoid a default
            // conflict.
            GameAction::Attack => KeyCode::KeyZ,
            GameAction::Use => KeyCode::KeyX,
            GameAction::Inventory => KeyCode::KeyE,
            GameAction::Chat => KeyCode::KeyT,
            GameAction::Command => KeyCode::Slash,
            GameAction::PlayerList => KeyCode::Tab,
            GameAction::Drop => KeyCode::KeyQ,
            GameAction::SwapHands => KeyCode::KeyF,
            GameAction::Hotbar1 => KeyCode::Digit1,
            GameAction::Hotbar2 => KeyCode::Digit2,
            GameAction::Hotbar3 => KeyCode::Digit3,
            GameAction::Hotbar4 => KeyCode::Digit4,
            GameAction::Hotbar5 => KeyCode::Digit5,
            GameAction::Hotbar6 => KeyCode::Digit6,
            GameAction::Hotbar7 => KeyCode::Digit7,
            GameAction::Hotbar8 => KeyCode::Digit8,
            GameAction::Hotbar9 => KeyCode::Digit9,
            GameAction::Screenshot => KeyCode::F2,
            GameAction::TogglePerspective => KeyCode::F5,
            GameAction::Debug => KeyCode::F3,
        }
    }
}

/// The action → key map, with vanilla defaults. Stored as an ordered list so
/// the controls screen and the options file are deterministic.
#[derive(Debug, Clone)]
pub struct Keybinds {
    binds: Vec<(GameAction, KeyCode)>,
}

impl Default for Keybinds {
    fn default() -> Self {
        Self {
            binds: GameAction::ALL
                .into_iter()
                .map(|a| (a, a.default_key()))
                .collect(),
        }
    }
}

impl Keybinds {
    /// The key currently bound to `action`.
    pub fn key_for(&self, action: GameAction) -> KeyCode {
        self.binds
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, k)| *k)
            .unwrap_or_else(|| action.default_key())
    }

    /// The action a pressed key triggers, if any. On a conflict the first action
    /// in [`GameAction::ALL`] order wins (matching the list display order).
    pub fn action_for(&self, code: KeyCode) -> Option<GameAction> {
        self.binds
            .iter()
            .find(|(_, k)| *k == code)
            .map(|(a, _)| *a)
    }

    /// Rebind `action` to `code` (replacing any prior binding for it). The same
    /// key may end up on two actions; [`conflict`](Self::conflict) reports that.
    pub fn set(&mut self, action: GameAction, code: KeyCode) {
        if let Some(slot) = self.binds.iter_mut().find(|(a, _)| *a == action) {
            slot.1 = code;
        } else {
            self.binds.push((action, code));
        }
    }

    /// Whether the key bound to `action` is also bound to a different action.
    pub fn conflict(&self, action: GameAction) -> bool {
        let key = self.key_for(action);
        self.binds
            .iter()
            .filter(|(_, k)| *k == key)
            .nth(1)
            .is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = &(GameAction, KeyCode)> {
        self.binds.iter()
    }
}

/// `KeyCode` → stable serialization/display name. Uses the winit variant name
/// directly so the file is self-describing. Unlisted keys (rare on a keyboard)
/// fall back to their `Debug` rendering, which is the variant name in winit.
pub fn keycode_name(code: KeyCode) -> String {
    format!("{code:?}")
}

/// Parse a [`keycode_name`] back into a [`KeyCode`]. Returns `None` for an
/// unknown name (a hand-edited typo) so the default binding is kept.
pub fn keycode_from_name(name: &str) -> Option<KeyCode> {
    KEYCODE_NAMES
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(c, _)| *c)
}

/// The keys recraft can bind, paired with their `Debug`/serialization name. This
/// covers the full set a user could reasonably rebind to (letters, digits,
/// function keys, modifiers, arrows and the common punctuation/control keys).
const KEYCODE_NAMES: &[(KeyCode, &str)] = &[
    (KeyCode::KeyA, "KeyA"),
    (KeyCode::KeyB, "KeyB"),
    (KeyCode::KeyC, "KeyC"),
    (KeyCode::KeyD, "KeyD"),
    (KeyCode::KeyE, "KeyE"),
    (KeyCode::KeyF, "KeyF"),
    (KeyCode::KeyG, "KeyG"),
    (KeyCode::KeyH, "KeyH"),
    (KeyCode::KeyI, "KeyI"),
    (KeyCode::KeyJ, "KeyJ"),
    (KeyCode::KeyK, "KeyK"),
    (KeyCode::KeyL, "KeyL"),
    (KeyCode::KeyM, "KeyM"),
    (KeyCode::KeyN, "KeyN"),
    (KeyCode::KeyO, "KeyO"),
    (KeyCode::KeyP, "KeyP"),
    (KeyCode::KeyQ, "KeyQ"),
    (KeyCode::KeyR, "KeyR"),
    (KeyCode::KeyS, "KeyS"),
    (KeyCode::KeyT, "KeyT"),
    (KeyCode::KeyU, "KeyU"),
    (KeyCode::KeyV, "KeyV"),
    (KeyCode::KeyW, "KeyW"),
    (KeyCode::KeyX, "KeyX"),
    (KeyCode::KeyY, "KeyY"),
    (KeyCode::KeyZ, "KeyZ"),
    (KeyCode::Digit0, "Digit0"),
    (KeyCode::Digit1, "Digit1"),
    (KeyCode::Digit2, "Digit2"),
    (KeyCode::Digit3, "Digit3"),
    (KeyCode::Digit4, "Digit4"),
    (KeyCode::Digit5, "Digit5"),
    (KeyCode::Digit6, "Digit6"),
    (KeyCode::Digit7, "Digit7"),
    (KeyCode::Digit8, "Digit8"),
    (KeyCode::Digit9, "Digit9"),
    (KeyCode::F1, "F1"),
    (KeyCode::F2, "F2"),
    (KeyCode::F3, "F3"),
    (KeyCode::F4, "F4"),
    (KeyCode::F5, "F5"),
    (KeyCode::F6, "F6"),
    (KeyCode::F7, "F7"),
    (KeyCode::F8, "F8"),
    (KeyCode::F9, "F9"),
    (KeyCode::F10, "F10"),
    (KeyCode::F11, "F11"),
    (KeyCode::F12, "F12"),
    (KeyCode::Space, "Space"),
    (KeyCode::Enter, "Enter"),
    (KeyCode::Tab, "Tab"),
    (KeyCode::Backspace, "Backspace"),
    (KeyCode::ShiftLeft, "ShiftLeft"),
    (KeyCode::ShiftRight, "ShiftRight"),
    (KeyCode::ControlLeft, "ControlLeft"),
    (KeyCode::ControlRight, "ControlRight"),
    (KeyCode::AltLeft, "AltLeft"),
    (KeyCode::AltRight, "AltRight"),
    (KeyCode::SuperLeft, "SuperLeft"),
    (KeyCode::SuperRight, "SuperRight"),
    (KeyCode::ArrowLeft, "ArrowLeft"),
    (KeyCode::ArrowRight, "ArrowRight"),
    (KeyCode::ArrowUp, "ArrowUp"),
    (KeyCode::ArrowDown, "ArrowDown"),
    (KeyCode::Minus, "Minus"),
    (KeyCode::Equal, "Equal"),
    (KeyCode::BracketLeft, "BracketLeft"),
    (KeyCode::BracketRight, "BracketRight"),
    (KeyCode::Backslash, "Backslash"),
    (KeyCode::Semicolon, "Semicolon"),
    (KeyCode::Quote, "Quote"),
    (KeyCode::Backquote, "Backquote"),
    (KeyCode::Comma, "Comma"),
    (KeyCode::Period, "Period"),
    (KeyCode::Slash, "Slash"),
];

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
        original.old_animations = true;
        original.save_to(&path);

        let loaded = Settings::load_from(&path);
        assert!((loaded.sensitivity - 0.73).abs() < 1e-6);
        assert!(!loaded.vsync);
        assert_eq!(loaded.fps_cap, 60);
        assert!((loaded.render_scale - 0.7).abs() < 1e-6);
        assert!(!loaded.fancy_graphics);
        assert_eq!(loaded.mipmap_levels, 2);
        assert!((loaded.brightness - 0.55).abs() < 1e-6);
        assert!(loaded.old_animations);

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

    #[test]
    fn keybinds_default_lookup() {
        let binds = Keybinds::default();
        // Every action resolves to its vanilla default key both ways.
        assert_eq!(binds.key_for(GameAction::Forward), KeyCode::KeyW);
        assert_eq!(binds.key_for(GameAction::Inventory), KeyCode::KeyE);
        assert_eq!(binds.key_for(GameAction::Hotbar1), KeyCode::Digit1);
        assert_eq!(binds.action_for(KeyCode::KeyW), Some(GameAction::Forward));
        assert_eq!(binds.action_for(KeyCode::Tab), Some(GameAction::PlayerList));
        // An unbound key resolves to nothing.
        assert_eq!(binds.action_for(KeyCode::KeyB), None);
    }

    #[test]
    fn keybinds_no_default_conflicts() {
        let binds = Keybinds::default();
        for action in GameAction::ALL {
            assert!(
                !binds.conflict(action),
                "default bind for {:?} conflicts",
                action
            );
        }
    }

    #[test]
    fn keybinds_rebind_moves_the_key() {
        let mut binds = Keybinds::default();
        binds.set(GameAction::Forward, KeyCode::ArrowUp);
        assert_eq!(binds.key_for(GameAction::Forward), KeyCode::ArrowUp);
        // The old key is now unbound, the new key resolves to the action.
        assert_eq!(binds.action_for(KeyCode::KeyW), None);
        assert_eq!(binds.action_for(KeyCode::ArrowUp), Some(GameAction::Forward));
    }

    #[test]
    fn keybinds_conflict_detection() {
        let mut binds = Keybinds::default();
        // Bind Jump to W, which Forward already holds.
        binds.set(GameAction::Jump, KeyCode::KeyW);
        assert!(binds.conflict(GameAction::Forward));
        assert!(binds.conflict(GameAction::Jump));
        // A non-conflicting action is unaffected.
        assert!(!binds.conflict(GameAction::Sneak));
    }

    #[test]
    fn keycode_name_round_trip() {
        for &(code, _) in KEYCODE_NAMES {
            assert_eq!(
                keycode_from_name(&keycode_name(code)),
                Some(code),
                "round-trip failed for {code:?}"
            );
        }
        // An unknown name yields None rather than a wrong key.
        assert_eq!(keycode_from_name("NotAKey"), None);
    }

    #[test]
    fn action_name_round_trip() {
        for action in GameAction::ALL {
            assert_eq!(GameAction::from_name(action.name()), Some(action));
        }
        assert_eq!(GameAction::from_name("nope"), None);
    }

    #[test]
    fn keybinds_persist_through_disk() {
        let path = std::env::temp_dir().join("recraft_settings_keybinds.txt");
        let _ = std::fs::remove_file(&path);

        let mut original = Settings::default();
        original.keybinds.set(GameAction::Forward, KeyCode::ArrowUp);
        original.keybinds.set(GameAction::Inventory, KeyCode::KeyI);
        original.save_to(&path);

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.keybinds.key_for(GameAction::Forward), KeyCode::ArrowUp);
        assert_eq!(loaded.keybinds.key_for(GameAction::Inventory), KeyCode::KeyI);
        // Untouched binds keep their defaults.
        assert_eq!(loaded.keybinds.key_for(GameAction::Jump), KeyCode::Space);

        let _ = std::fs::remove_file(&path);
    }
}
