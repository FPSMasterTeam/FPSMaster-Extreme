mod auth;
mod chat;
mod container;
mod ext_bridge;
mod game;
mod gui;
mod item_renderer;
mod network;
mod particle;
mod player_list;
mod scoreboard;
mod singleplayer;
mod skin;
mod servers;
mod settings;
mod sound;
mod text_input;

use std::{
    env,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use anyhow::Context;
use auth::{AuthEvent, Session};
use ext_bridge::GameViews;
use game::GameState;
use recraft_ext::ReadViews;
use gui::accounts::GuiAccounts;
use gui::chat_screen::GuiChat;
use gui::game_over::GuiGameOver;
use gui::ingame::{GuiIngame, HudState};
use gui::ingame_menu::GuiIngameMenu;
use gui::inventory::GuiContainer;
use gui::main_menu::GuiMainMenu;
use gui::progress::{
    GuiAuthCode, GuiConnecting, GuiDisconnected, GuiProgress, GuiStartingServer, Parent,
};
use singleplayer::LocalServer;
use gui::{AccountEntry, DrawCtx, GuiAction, GuiScreen, ScreenCtx};
use item_renderer::ItemRenderer;
use network::{NetworkEvent, NetworkHandle};
use recraft_protocol::{net::PremiumSession, v1_8_9::packets::ServerboundPacket};
use recraft_render::{RenderStats, Renderer};
use settings::{FpsCounter, GameAction, Keybinds, Settings};

/// Dirty sections snapshotted and handed to the background mesher each frame.
/// Sections of the same column share one snapshot clone (the only main-thread
/// cost); the mesh build runs off-thread. Higher than the old per-column budget
/// because a column now contributes several sections.
const MESH_SUBMITS_PER_FRAME: usize = 40;
/// Finished background section meshes uploaded to the GPU each frame.
const MESH_UPLOADS_PER_FRAME: usize = 48;
/// Entities are culled at the terrain render distance but never beyond this many
/// chunks — distant mobs add per-frame articulated-mesh cost for little visual
/// gain, so the crowd is dropped well before the terrain horizon.
const ENTITY_RENDER_CHUNKS: u32 = 8;
use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{DeviceEvent, ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{CursorGrabMode, Window},
};

struct LaunchConfig {
    server: Option<(String, u16)>,
    username: String,
    assets: Option<PathBuf>,
    scripted_smoke_seconds: Option<f32>,
    headless_smoke_seconds: Option<f32>,
    headless_interact_seconds: Option<f32>,
    /// In-process per-pass benchmark: cycle render-pass skip configs frame by
    /// frame (fps cap off) and print an A/B table on exit. Defeats the
    /// cross-process thermal/clock drift that makes separate runs incomparable.
    bench_passes_seconds: Option<f32>,
    /// Force the window's physical-pixel inner size (`--window WxH`).
    window_size: Option<(u32, u32)>,
    demo_kind: game::DemoKind,
}

/// All mutable application state the screens and actions operate on (the
/// vanilla `Minecraft` singleton equivalent).
struct App {
    game: GameState,
    network: Option<NetworkHandle>,
    /// The open screen (`mc.currentScreen`); None = gameplay input.
    screen: Option<Box<dyn GuiScreen>>,
    /// Whether a world session (demo or server) is active.
    in_world: bool,
    /// Waiting for the server join to finish (first chunks).
    connecting: bool,
    settings: Settings,
    ms_session: Option<Session>,
    auth_rx: Option<Receiver<AuthEvent>>,
    accounts: auth::AccountStore,
    clipboard: Option<arboard::Clipboard>,
    username: String,
    /// Whether the Tab key is held — shows the player-list overlay.
    tab_open: bool,
    /// Per-player skin downloads and atlas-row allocation.
    skin_manager: skin::SkinManager,
    /// Child process for the local Paper server (singleplayer mode).
    local_server: Option<LocalServer>,
    /// Vanilla `panoramaTimer` — incremented every frame on the title screen.
    panorama_timer: f32,
    /// Reused across frames so the per-frame entity rebuild keeps its vertex/index
    /// allocations instead of reallocating from empty each frame.
    entity_model: recraft_render::ModelMesh,
    /// Enchanted worn-armor glint geometry (model-pass format), rebuilt alongside
    /// `entity_model` and drawn additively with the scrolling glint texture.
    entity_glint: recraft_render::ModelMesh,
    /// Fingerprint of the inputs that produced the currently-uploaded entity model
    /// + hand + nametags. When the next frame's fingerprint matches, the rebuild
    /// and GPU upload are skipped (the renderer keeps the previous mesh).
    last_entity_key: Option<u64>,
    /// Audio backend: resolves `sounds.json` events and plays positioned/UI
    /// sounds queued by the game. Silent if no output device is available.
    sound: sound::SoundManager,
    /// GUI/container interaction packets (ClickWindow, CloseWindow, …) produced
    /// at frame time by screen input. Vanilla emits them inside `runTick` BEFORE
    /// that tick's flying packet; we buffer them and flush right before
    /// `send_movement` so Grim's Post check sees `click_window → flying →
    /// transaction`, not the reverse (sending at frame time let a transaction
    /// land between the last flying and the click → Post "click window v1.8").
    pending_window_packets: Vec<ServerboundPacket>,
    /// Extension host: owns loaded mods, the event bus and the command queue.
    /// A sibling field of `game`, so `app.ext.method(&GameViews(&app.game))` is a
    /// disjoint two-field borrow.
    ext: recraft_ext::ExtManager,
    /// Accumulated `setBlockTint` overrides + a dirty flag so a change re-meshes
    /// the loaded world once (future chunks read the renderer's global table).
    block_tints: recraft_render::TintTable,
    tints_dirty: bool,
    /// Native render-hook geometry (latest submission) + a dirty flag to upload
    /// it to the renderer once per change.
    ext_geometry: Vec<recraft_render::ModelVertex>,
    ext_indices: Vec<u32>,
    geometry_dirty: bool,
    /// Extension preset render state: fullbright (brightness), nametag scale, and
    /// the ESP-style line overlays (target block outline / chunk borders / entity
    /// boxes). The overlays regenerate their line geometry each frame while on.
    ext_fullbright: bool,
    ext_nametag_scale: f32,
    ext_chunk_borders: bool,
    ext_entity_box: Option<[f32; 3]>,
    ext_lines_on: bool,
    quit: bool,
}

impl App {
    fn session_username(&self) -> Option<&str> {
        self.ms_session.as_ref().map(|s| s.username.as_str())
    }

    fn account_entries(&self) -> Vec<AccountEntry> {
        self.accounts
            .accounts
            .iter()
            .map(|account| AccountEntry {
                username: account.username.clone(),
                uuid: account.uuid.clone(),
                active: self
                    .ms_session
                    .as_ref()
                    .is_some_and(|session| session.uuid == account.uuid),
            })
            .collect()
    }

    /// Release held movement keys and abort any in-progress dig — called when
    /// a screen opens over gameplay.
    fn suspend_gameplay_input(&mut self, left_held: &mut bool, right_held: &mut bool) {
        self.game.input.release_all();
        self.tab_open = false;
        *left_held = false;
        *right_held = false;
        if let Some(packet) = self.game.cancel_breaking() {
            if let Some(network) = &self.network {
                network.send_packet(packet);
            }
        }
    }

    /// Start an auth flow and show its progress screen.
    fn begin_login(&mut self, token: Option<String>) {
        let (tx, rx) = mpsc::channel();
        self.auth_rx = Some(rx);
        match token {
            Some(token) => {
                auth::start_login_with_refresh_token(token, tx);
                self.screen = Some(Box::new(GuiProgress::new(
                    "Signing in...",
                    "Redeeming refresh token",
                )));
            }
            None => {
                auth::start_login(tx);
                self.screen = Some(Box::new(GuiProgress::new(
                    "Microsoft Login",
                    "Contacting Microsoft...",
                )));
            }
        }
    }
}

/// Holds all runtime state that lives across the event loop. Initialized in
/// `resumed()` once the OS hands us a surface we can render to.
struct WinitApp {
    config: LaunchConfig,
    // Initialized in resumed():
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    app: Option<App>,
    atlas_uv: recraft_render::AtlasUv,
    overlay_textures: recraft_render::OverlayTextures,
    // Input state:
    cursor_captured: bool,
    cursor_position: (f64, f64),
    modifiers: ModifiersState,
    ime_enabled: bool,
    last_ime_area: Option<(i32, i32, i32, i32)>,
    mouse_down_left: bool,
    mouse_down_right: bool,
    left_held: bool,
    right_held: bool,
    attack_pressed: bool,
    use_pressed: bool,
    slot_select: Option<i32>,
    slot_scroll: i32,
    was_dead: bool,
    f3_debug: bool,
    // Timing:
    last_frame: Instant,
    last_sim: Instant,
    app_start: Instant,
    fps_counter: FpsCounter,
    tick_accumulator: f32,
    // Adaptive resolution: smoothed GPU frame time + the current auto scale, with
    // a cooldown so the scale steps at most once a second (each step reallocates
    // the offscreen targets).
    adaptive_gpu_ms: f32,
    adaptive_scale: f32,
    adaptive_last_adjust: Instant,
    adaptive_was_on: bool,
    // Scripted smoke / benchmarks:
    scripted_smoke_seconds: Option<f32>,
    scripted_smoke_static: bool,
    scripted_smoke_done: bool,
    smoke_profile: Option<SmokeProfile>,
    pass_bench: Option<PassBench>,
    window_shown: bool,
    /// The window is fully hidden/occluded (backgrounded, minimized, covered).
    /// While set, rendering is skipped — see the note in `about_to_wait`.
    occluded: bool,
}

impl WinitApp {
    fn new(config: LaunchConfig) -> Self {
        let now = Instant::now();
        let scripted_smoke_seconds =
            config.scripted_smoke_seconds.or(config.bench_passes_seconds);
        let scripted_smoke_static = matches!(
            config.demo_kind,
            game::DemoKind::ChunkStress | game::DemoKind::Terrain | game::DemoKind::SingleCube
        );
        let smoke_profile = config.scripted_smoke_seconds.map(|_| SmokeProfile::new(now));
        let pass_bench = config
            .bench_passes_seconds
            .map(|secs| PassBench::new(now, secs));
        Self {
            config,
            window: None,
            renderer: None,
            app: None,
            atlas_uv: Default::default(),
            overlay_textures: recraft_render::OverlayTextures::load(),
            cursor_captured: false,
            cursor_position: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
            ime_enabled: false,
            last_ime_area: None,
            mouse_down_left: false,
            mouse_down_right: false,
            left_held: false,
            right_held: false,
            attack_pressed: false,
            use_pressed: false,
            slot_select: None,
            slot_scroll: 0,
            was_dead: false,
            f3_debug: false,
            last_frame: now,
            last_sim: now,
            app_start: now,
            fps_counter: FpsCounter::new(now),
            tick_accumulator: 0.0,
            adaptive_gpu_ms: 0.0,
            adaptive_scale: 1.0,
            adaptive_last_adjust: now,
            adaptive_was_on: false,
            scripted_smoke_seconds,
            scripted_smoke_static,
            scripted_smoke_done: false,
            smoke_profile,
            pass_bench,
            window_shown: false,
            occluded: false,
        }
    }
}

impl ApplicationHandler for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let initial_size: winit::dpi::Size = match self.config.window_size {
            Some((w, h)) => winit::dpi::PhysicalSize::new(w, h).into(),
            None => LogicalSize::new(1280.0, 720.0).into(),
        };
        let attrs = Window::default_attributes()
            .with_title("ReCraft - Rust Minecraft Client")
            .with_inner_size(initial_size)
            .with_visible(false);
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        release_cursor(&window);

        let mut renderer = Renderer::new(window.clone()).expect("create renderer");
        let mut settings = Settings::load();
        let auto_play = self.config.scripted_smoke_seconds
            .or(self.config.bench_passes_seconds)
            .is_some();
        if auto_play {
            settings.vsync = false;
            settings.fps_cap = u32::MAX;
        }
        renderer.set_vsync(settings.vsync);
        renderer.set_fancy_graphics(settings.fancy_graphics);
        renderer.set_mipmap_levels(settings.mipmap_levels);
        renderer.set_render_scale(settings.render_scale);
        renderer.set_render_distance(settings.render_distance);
        renderer.set_smooth_lighting(settings.smooth_lighting);
        renderer.set_shaders_enabled(settings.shaders);
        renderer.set_shadows_enabled(settings.shader_shadows);
        renderer.set_specular_enabled(settings.shader_specular);
        renderer.set_fog_enabled(settings.shader_fog);
        renderer.set_bloom_enabled(settings.shader_bloom);
        renderer.set_brightness(settings.brightness);
        renderer.set_vignette_enabled(settings.post_vignette);
        renderer.set_chromatic_enabled(settings.post_chromatic);
        renderer.set_dof_enabled(settings.post_dof);
        renderer.set_motion_blur_enabled(settings.post_motion_blur);
        renderer.set_auto_exposure_enabled(settings.post_auto_exposure);
        renderer.set_clouds_enabled(settings.volumetric_clouds);
        renderer.set_volumetric_light_enabled(settings.volumetric_light);
        if !settings.fullscreen {
            apply_display(&window, &settings);
        }
        if let Some(ref pack_name) = settings.resource_pack {
            let pack_path = std::path::PathBuf::from("resourcepacks").join(pack_name);
            if pack_path.exists() {
                renderer.reload_atlas(Some(pack_path));
            }
        }
        self.atlas_uv = renderer.atlas_uv().clone();

        let auto_connect = self.config.server.clone();
        let auto_demo = auto_play && auto_connect.is_none();
        let username = self.config.username.clone();

        let mut app = App {
            game: if auto_demo {
                GameState::demo(self.config.demo_kind, renderer.aspect())
            } else {
                GameState::empty_for_server(renderer.aspect())
            },
            network: auto_connect.as_ref().map(|(host, port)| {
                log::info!("connecting to {host}:{port} as {username}");
                NetworkHandle::connect_offline_1_8_9(host.clone(), *port, username.clone())
            }),
            screen: match &auto_connect {
                Some((host, port)) => Some(Box::new(GuiConnecting {
                    host: host.clone(),
                    port: *port,
                })),
                None if auto_demo => None,
                None => Some(Box::new(GuiMainMenu::new())),
            },
            in_world: auto_demo,
            connecting: auto_connect.is_some(),
            settings,
            ms_session: None,
            auth_rx: None,
            accounts: auth::AccountStore::load(),
            clipboard: arboard::Clipboard::new().ok(),
            username,
            tab_open: false,
            skin_manager: skin::SkinManager::new(),
            local_server: None,
            panorama_timer: 0.0,
            entity_model: recraft_render::ModelMesh::new(),
            entity_glint: recraft_render::ModelMesh::new(),
            last_entity_key: None,
            sound: sound::SoundManager::new(),
            pending_window_packets: Vec::new(),
            ext: recraft_ext::ExtManager::new(),
            block_tints: recraft_render::TintTable::new(),
            tints_dirty: false,
            ext_geometry: Vec::new(),
            ext_indices: Vec::new(),
            geometry_dirty: false,
            ext_fullbright: false,
            ext_nametag_scale: 1.0,
            ext_chunk_borders: false,
            ext_entity_box: None,
            ext_lines_on: false,
            quit: false,
        };
        renderer.upload_world(&app.game.world);
        // Opt-in in-tree demo mod that validates all four ext seams (HUD / packet
        // intercept / chat command / custom keybind).
        if std::env::var("RECRAFT_EXT_DEMO").is_ok() {
            app.ext.register(Box::new(recraft_ext::dev::DemoMod::new()));
        }
        // Load `.js` (and, later, native) mods from `mods/` next to the working
        // directory. F10 reloads them at runtime.
        let loaded = app.ext.load_mods(std::path::Path::new("mods"));
        if !loaded.is_empty() {
            log::info!("[ext] loaded {} mod(s): {:?}", loaded.len(), loaded);
        }

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.app = Some(app);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let (Some(window), Some(renderer), Some(app)) =
            (self.window.as_ref(), self.renderer.as_mut(), self.app.as_mut())
        else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Occluded(occluded) => self.occluded = occluded,
            WindowEvent::Resized(size) => {
                renderer.resize(size);
                app.game.set_aspect(renderer.aspect());
            }
            WindowEvent::ModifiersChanged(state) => {
                self.modifiers = state.state();
            }
            WindowEvent::Ime(ime) => {
                if let Some(input) = app
                    .screen
                    .as_mut()
                    .and_then(|screen| screen.focused_text_input())
                {
                    input.handle_ime(&ime);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // F10: hot-reload `mods/` (dev convenience; never consumed by mods).
                if event.state == ElementState::Pressed
                    && !event.repeat
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::F10))
                {
                    let loaded = app.ext.reload_mods(std::path::Path::new("mods"));
                    log::info!("[ext] reloaded {} mod(s): {:?}", loaded.len(), loaded);
                }
                if event.state == ElementState::Pressed
                    && !event.repeat
                    && matches!(event.physical_key, PhysicalKey::Code(code)
                        if app.settings.keybinds.action_for(code) == Some(GameAction::Debug))
                    && app.in_world
                    && app
                        .screen
                        .as_ref()
                        .is_none_or(|screen| screen.chat_input().is_none())
                {
                    self.f3_debug = !self.f3_debug;
                }
                if app.screen.is_some() {
                    let mut taken = app.screen.take();
                    let actions = if let Some(screen) = taken.as_mut() {
                        let mut ctx = ScreenCtx {
                            game: &mut app.game,
                            settings: &mut app.settings,
                            clipboard: app.clipboard.as_mut(),
                            modifiers: self.modifiers,
                            mouse: self.cursor_position,
                        };
                        screen.key_pressed(&event, &mut ctx)
                    } else {
                        Vec::new()
                    };
                    app.screen = taken;
                    handle_actions(app, renderer, window, actions, &mut self.atlas_uv);
                } else if app.in_world {
                    let pressed = event.state == ElementState::Pressed;
                    // ext on_input seam: a mod may consume the key (custom binds).
                    // Consuming skips the default gameplay handling below.
                    if let Some(name) = key_name(&event.physical_key) {
                        if app.ext.dispatch_input(
                            &recraft_ext::InputEvent::new(name, pressed),
                            &GameViews(&app.game),
                        ) {
                            sync_cursor(window, &mut self.cursor_captured, app);
                            return;
                        }
                    }
                    let action = match event.physical_key {
                        PhysicalKey::Code(code) => app.settings.keybinds.action_for(code),
                        _ => None,
                    };
                    if pressed
                        && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
                    {
                        app.suspend_gameplay_input(&mut self.left_held, &mut self.right_held);
                        app.screen = Some(Box::new(GuiIngameMenu::new()));
                    } else if pressed && action == Some(GameAction::Inventory) {
                        app.suspend_gameplay_input(&mut self.left_held, &mut self.right_held);
                        app.game.open_player_inventory();
                        app.screen = Some(Box::new(GuiContainer::new()));
                    } else if let Some(prefill) = chat_open_key(&event, &app.settings.keybinds) {
                        app.suspend_gameplay_input(&mut self.left_held, &mut self.right_held);
                        app.game.chat.reset_recall();
                        app.screen = Some(Box::new(GuiChat::new(prefill)));
                    } else if let Some(slot) = hotbar_slot_key(&event, &app.settings.keybinds) {
                        self.slot_select = Some(slot);
                    } else if action == Some(GameAction::PlayerList) {
                        app.tab_open = pressed;
                    } else {
                        app.game.input.handle_key(event, &app.settings.keybinds);
                    }
                }
                sync_cursor(window, &mut self.cursor_captured, app);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                if let Some(screen) = app.screen.as_mut() {
                    screen.mouse_scrolled(steps);
                } else if app.in_world {
                    self.slot_scroll += -steps.signum() as i32;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = (position.x, position.y);
                if self.mouse_down_left || self.mouse_down_right {
                    let mut taken = app.screen.take();
                    if let Some(screen) = taken.as_mut() {
                        let mut ctx = ScreenCtx {
                            game: &mut app.game,
                            settings: &mut app.settings,
                            clipboard: app.clipboard.as_mut(),
                            modifiers: self.modifiers,
                            mouse: self.cursor_position,
                        };
                        screen.mouse_dragged(position.x, position.y, &mut ctx);
                    }
                    app.screen = taken;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let is_left = button == MouseButton::Left;
                let is_right = button == MouseButton::Right;
                let is_middle = button == MouseButton::Middle;
                if state == ElementState::Released {
                    if is_left {
                        self.mouse_down_left = false;
                        self.left_held = false;
                    }
                    if is_right {
                        self.mouse_down_right = false;
                        self.right_held = false;
                    }
                    if app.screen.is_some() && (is_left || is_right) {
                        let mut taken = app.screen.take();
                        let actions = if let Some(screen) = taken.as_mut() {
                            let mut ctx = ScreenCtx {
                                game: &mut app.game,
                                settings: &mut app.settings,
                                clipboard: app.clipboard.as_mut(),
                                modifiers: self.modifiers,
                                mouse: self.cursor_position,
                            };
                            screen.mouse_released(
                                self.cursor_position.0,
                                self.cursor_position.1,
                                is_right,
                                &mut ctx,
                            )
                        } else {
                            Vec::new()
                        };
                        app.screen = taken;
                        handle_actions(app, renderer, window, actions, &mut self.atlas_uv);
                    }
                } else if app.screen.is_some() {
                    if is_left || is_right || is_middle {
                        if is_left {
                            self.mouse_down_left = true;
                        } else if is_right {
                            self.mouse_down_right = true;
                        }
                        let mut taken = app.screen.take();
                        let (mx, my) = self.cursor_position;
                        // Vanilla GuiScreen.mouseClicked plays gui.button.press for
                        // a left-click that lands on an enabled button (client-only;
                        // no server packet). Emit it centrally before dispatch.
                        if is_left && taken.as_ref().is_some_and(|s| s.clicks_button(mx, my)) {
                            app.game.queue_ui_sound("gui.button.press");
                        }
                        let actions = if let Some(screen) = taken.as_mut() {
                            let mut ctx = ScreenCtx {
                                game: &mut app.game,
                                settings: &mut app.settings,
                                clipboard: app.clipboard.as_mut(),
                                modifiers: self.modifiers,
                                mouse: self.cursor_position,
                            };
                            if is_left {
                                screen.mouse_clicked(mx, my, &mut ctx)
                            } else if is_right {
                                screen.mouse_right_clicked(mx, my, &mut ctx)
                            } else {
                                screen.mouse_middle_clicked(mx, my, &mut ctx)
                            }
                        } else {
                            Vec::new()
                        };
                        app.screen = taken;
                        handle_actions(app, renderer, window, actions, &mut self.atlas_uv);
                        sync_cursor(window, &mut self.cursor_captured, app);
                    }
                } else if app.in_world {
                    match button {
                        MouseButton::Left => {
                            self.mouse_down_left = true;
                            self.left_held = true;
                            self.attack_pressed = true;
                        }
                        MouseButton::Right => {
                            self.right_held = true;
                            self.use_pressed = true;
                        }
                        _ => {}
                    }
                    sync_cursor(window, &mut self.cursor_captured, app);
                }
            }
            WindowEvent::RedrawRequested => {}
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        let Some(app) = self.app.as_mut() else { return };
        if let DeviceEvent::MouseMotion { delta } = event {
            if app.in_world && app.screen.is_none() {
                app.game.rotate_view(
                    delta.0 as f32,
                    delta.1 as f32,
                    app.settings.clone().mouse_factor(),
                );
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Poll);

        let (Some(window), Some(renderer), Some(app)) =
            (self.window.as_ref(), self.renderer.as_mut(), self.app.as_mut())
        else {
            return;
        };

        poll_auth_events(app);
        pump_network(app, window, &mut self.cursor_captured);
        // Drain commands queued by the clientbound/chunk ext hooks (done here, not
        // inside pump_network, because that loop holds a borrow of app.network).
        apply_ext_commands(app);

        if app.game.take_window_open() {
            app.suspend_gameplay_input(&mut self.left_held, &mut self.right_held);
            app.screen = Some(Box::new(GuiContainer::new()));
        }
        if app.game.take_window_close() {
            app.screen = None;
        }

        let mut taken = app.screen.take();
        let actions = if let Some(screen) = taken.as_mut() {
            let mut ctx = ScreenCtx {
                game: &mut app.game,
                settings: &mut app.settings,
                clipboard: app.clipboard.as_mut(),
                modifiers: self.modifiers,
                mouse: self.cursor_position,
            };
            screen.update(&mut ctx)
        } else {
            Vec::new()
        };
        app.screen = taken;
        handle_actions(app, renderer, window, actions, &mut self.atlas_uv);

        if self.scripted_smoke_seconds.is_some() && app.game.is_dead() {
            app.game.request_respawn();
        }

        let dead = app.in_world && app.game.is_dead();
        if dead && !self.was_dead {
            let interruptible = app
                .screen
                .as_ref()
                .is_none_or(|screen| screen.chat_input().is_some());
            if interruptible {
                app.screen = Some(Box::new(GuiGameOver::new()));
            }
        }
        self.was_dead = dead;

        if app.game.take_respawn_request() {
            if let Some(network) = &app.network {
                network.send_packet(ServerboundPacket::ClientStatus { action: 0 });
            }
        }

        let _ = app.game.take_position_confirm();

        let now = Instant::now();
        let sim_dt = (now - self.last_sim).as_secs_f32().min(0.25);
        self.last_sim = now;
        if let Some(seconds) = self.scripted_smoke_seconds {
            if !self.scripted_smoke_static && self.pass_bench.is_none() {
                app.game
                    .apply_scripted_smoke_input((now - self.app_start).as_secs_f32(), seconds);
            }
        }
        if app.in_world {
            self.tick_accumulator += sim_dt;
            while self.tick_accumulator >= 0.05 {
                app.game.set_pending_actions(game::TickActions {
                    slot_select: self.slot_select,
                    slot_scroll: self.slot_scroll,
                    attack_pressed: self.attack_pressed,
                    use_pressed: self.use_pressed,
                    left_held: self.left_held,
                    right_held: self.right_held,
                    old_animations: app.settings.old_animations,
                });
                if let Some((actions, movement)) = app.game.tick(0.05) {
                    self.slot_select = None;
                    self.slot_scroll = 0;
                    self.attack_pressed = false;
                    self.use_pressed = false;
                    let abilities = app.game.take_abilities_packet();
                    if let Some(network) = &app.network {
                        if app.game.can_send_movement_packets() {
                            // Frame-time GUI packets (ClickWindow, …) first, then
                            // this tick's interactions, then abilities, then the
                            // flying packet — all BEFORE movement, matching vanilla
                            // runTick so Grim's Post check sees the right order.
                            for packet in app.pending_window_packets.drain(..) {
                                network.send_packet(packet);
                            }
                            for packet in actions {
                                network.send_packet(packet);
                            }
                            if let Some(abilities) = abilities {
                                network.send_packet(abilities);
                            }
                            network.send_movement(movement);
                        }
                    }
                }
                app.ext.dispatch_tick(&GameViews(&app.game));
                self.tick_accumulator -= 0.05;
            }
            apply_ext_commands(app);
        }
        if !self.scripted_smoke_done
            && self.scripted_smoke_seconds
                .is_some_and(|seconds| (now - self.app_start).as_secs_f32() >= seconds)
        {
            self.scripted_smoke_done = true;
            if let Some(profile) = self.smoke_profile.as_mut() {
                profile.flush(now);
            }
            if let Some(bench) = self.pass_bench.as_ref() {
                bench.report();
            }
            log::info!("scripted smoke complete");
            event_loop.exit();
        }

        if app.quit {
            event_loop.exit();
        }
        sync_cursor(window, &mut self.cursor_captured, app);

        // Don't render while the window is occluded/backgrounded. On macOS the
        // swapchain occlusion-throttles, yet rendering an invisible window still
        // rebuilds the UI, writes the per-frame uniforms and churns a Metal
        // drawable every frame — a GPU-memory leak that, with unified memory, can
        // bog the whole system down. Idle instead; the ticks/network above keep
        // running so an in-world session stays in sync, and rendering resumes on
        // un-occlude.
        if self.occluded {
            std::thread::sleep(Duration::from_millis(50));
            return;
        }

        if self.pass_bench.is_none() {
            if let Some(cap) = app.settings.clone().fps_limit() {
                let deadline = self.last_frame + Duration::from_secs_f64(1.0 / cap as f64);
                let now = Instant::now();
                if now < deadline {
                    std::thread::sleep(deadline - now);
                }
            }
        }
        if let Some(bench) = self.pass_bench.as_mut() {
            let (sky, water, ui, flat) = bench.config_for_frame();
            renderer.set_pass_skip(sky, water, ui, flat);
        }
        // Per-frame extension hook (on_frame), e.g. native render geometry.
        app.ext.dispatch_frame(&GameViews(&app.game));
        apply_ext_commands(app);
        // Apply pending extension block-tint overrides (re-meshes the loaded
        // world once; future chunks read the renderer's global tint table).
        if app.tints_dirty && app.in_world {
            renderer.set_block_tints(app.block_tints.clone(), &app.game.world);
            app.tints_dirty = false;
        }
        // Upload native render-hook geometry when a mod changed it.
        if app.geometry_dirty {
            renderer.set_extension_geometry(&app.ext_geometry, &app.ext_indices);
            app.geometry_dirty = false;
        }
        // Extension preset render toggles: fullbright (force lightmap to full),
        // nametag scale, and the ESP line overlays.
        renderer.set_fullbright(app.ext_fullbright);
        renderer.set_nametag_scale(app.ext_nametag_scale);
        // Debug overlays: the built-in targeted-block outline + chunkBorders preset
        // (thin lines), and the entityBox hitbox (thick boxes). Rebuilt each frame
        // during gameplay (the outline follows where you look).
        let show_overlays = app.in_world && app.screen.is_none();
        if show_overlays {
            renderer.set_debug_lines(&build_debug_lines(app));
            renderer.set_debug_tris(&build_debug_tris(app));
            app.ext_lines_on = true;
        } else if app.ext_lines_on {
            renderer.set_debug_lines(&[]);
            renderer.set_debug_tris(&[]);
            app.ext_lines_on = false;
        }
        render_frame(
            renderer,
            app,
            window,
            &self.atlas_uv,
            &self.overlay_textures,
            &mut self.fps_counter,
            &mut self.last_frame,
            self.tick_accumulator,
            self.cursor_position,
            self.mouse_down_left,
            self.f3_debug,
            self.smoke_profile.is_some() || self.pass_bench.is_some(),
        );
        if let Some(profile) = self.smoke_profile.as_mut() {
            profile.record(renderer.last_stats(), Instant::now());
        }
        if let Some(bench) = self.pass_bench.as_mut() {
            bench.record(renderer.last_stats(), Instant::now());
        }
        // Adaptive resolution: drive the world render scale off the occlusion-proof
        // GPU frame time toward the target budget. Steps by 0.05 at most once a
        // second (each change reallocates the offscreen targets), within
        // [RENDER_SCALE_MIN, the user's render_scale] so it only ever scales down.
        if app.settings.adaptive_resolution && app.in_world {
            // Rising edge: start auto-scaling from the user's render_scale (its max).
            if !self.adaptive_was_on {
                self.adaptive_was_on = true;
                self.adaptive_scale = app.settings.render_scale;
                self.adaptive_gpu_ms = 0.0;
                self.adaptive_last_adjust = Instant::now();
            }
            // Fold only valid samples: gpu_us reads 0 when no timestamp readback is
            // ready that frame, and averaging those in would drag the estimate down
            // and bounce the scale back up.
            let gpu_ms = renderer.last_stats().gpu_us as f32 / 1000.0;
            if gpu_ms > 0.0 {
                self.adaptive_gpu_ms = if self.adaptive_gpu_ms <= 0.0 {
                    gpu_ms
                } else {
                    self.adaptive_gpu_ms * 0.9 + gpu_ms * 0.1
                };
            }
            let now = Instant::now();
            if self.adaptive_gpu_ms > 0.0 && (now - self.adaptive_last_adjust).as_secs_f32() >= 1.0 {
                let target_fps = app.settings.clone().fps_limit().unwrap_or(60).min(120) as f32;
                let budget = 1000.0 / target_fps;
                let max_scale = app.settings.render_scale;
                let new_scale = if self.adaptive_gpu_ms > budget * 0.95 {
                    (self.adaptive_scale - 0.05).max(settings::RENDER_SCALE_MIN)
                } else if self.adaptive_gpu_ms < budget * 0.6 {
                    (self.adaptive_scale + 0.05).min(max_scale)
                } else {
                    self.adaptive_scale
                };
                if (new_scale - self.adaptive_scale).abs() > 1e-3 {
                    self.adaptive_scale = new_scale;
                    renderer.set_render_scale(new_scale);
                    self.adaptive_last_adjust = now;
                }
            }
        } else {
            self.adaptive_was_on = false;
        }
        if !self.window_shown {
            window.set_visible(true);
            window.focus_window();
            self.window_shown = true;
            if app.settings.fullscreen {
                apply_display(window, &app.settings);
            }
        }

        let focused_caret = app
            .screen
            .as_mut()
            .and_then(|screen| screen.focused_text_input())
            .map(|input| input.caret_area());
        let want_ime = focused_caret.is_some();
        if want_ime != self.ime_enabled {
            window.set_ime_allowed(want_ime);
            self.ime_enabled = want_ime;
            if !want_ime {
                self.last_ime_area = None;
            }
        }
        if let Some(area) = focused_caret.flatten() {
            if self.last_ime_area != Some(area) {
                self.last_ime_area = Some(area);
                let (cx, cy, cw, ch) = area;
                window.set_ime_cursor_area(
                    PhysicalPosition::new(cx as f64, cy as f64),
                    PhysicalSize::new(cw.max(1) as f64, ch.max(1) as f64),
                );
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let config = LaunchConfig::from_args();
    if let Some(path) = &config.assets {
        env::set_var("RECRAFT_ASSET_PATH", path);
    }

    if let Some(seconds) = config.headless_interact_seconds {
        return run_headless_interact(&config, seconds);
    }
    if let Some(seconds) = config.headless_smoke_seconds {
        return run_headless_smoke(&config, seconds);
    }

    let event_loop = EventLoop::new().context("create event loop")?;
    let mut winit_app = WinitApp::new(config);
    event_loop.run_app(&mut winit_app)?;
    Ok(())
}

/// Apply screen actions to the application (navigation, connects, auth, …).
/// Apply the windowed/fullscreen + resolution settings to the OS window. In
/// windowed mode a chosen resolution resizes the window (and thus the swapchain);
/// fullscreen picks the exclusive video mode closest to it (display hardware
/// scales, bypassing the desktop compositor). Either way the surface follows via
/// the resulting Resized event.
fn apply_display(window: &winit::window::Window, settings: &Settings) {
    use winit::window::Fullscreen;
    if settings.fullscreen {
        let target = settings.resolution;
        let mode = window.current_monitor().and_then(|m| {
            let modes = m.video_modes();
            match target {
                Some((w, h)) => {
                    let area = (w as u64) * (h as u64);
                    modes.min_by_key(|vm| {
                        let s = vm.size();
                        let a = (s.width as u64) * (s.height as u64);
                        (a.abs_diff(area), u32::MAX - vm.refresh_rate_millihertz())
                    })
                }
                None => modes.max_by_key(|vm| {
                    let s = vm.size();
                    (s.width as u64 * s.height as u64, vm.refresh_rate_millihertz())
                }),
            }
        });
        match mode {
            Some(vm) => window.set_fullscreen(Some(Fullscreen::Exclusive(vm))),
            None => window.set_fullscreen(Some(Fullscreen::Borderless(None))),
        }
    } else {
        window.set_fullscreen(None);
        if let Some((w, h)) = settings.resolution {
            let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(w, h));
        }
    }
}

fn handle_actions(
    app: &mut App,
    renderer: &mut Renderer,
    window: &winit::window::Window,
    actions: Vec<GuiAction>,
    atlas_uv: &mut recraft_render::AtlasUv,
) {
    for action in actions {
        match action {
            GuiAction::SetScreen(screen) => app.screen = Some(screen),
            GuiAction::CloseScreen => {
                if app.in_world {
                    app.screen = None;
                } else {
                    app.screen = Some(Box::new(GuiMainMenu::new()));
                }
            }
            GuiAction::StartDemo(kind) => {
                app.network = None;
                app.connecting = false;
                app.game = GameState::demo(kind, renderer.aspect());
                renderer.upload_world(&app.game.world);
                app.in_world = true;
                app.screen = None;
            }
            GuiAction::StartSingleplayer => {
                match LocalServer::start(&app.username) {
                    Ok((server, ready_rx)) => {
                        let port = server.port();
                        app.local_server = Some(server);
                        app.screen =
                            Some(Box::new(GuiStartingServer::new(ready_rx, port)));
                    }
                    Err(msg) => {
                        log::error!("singleplayer: {msg}");
                        app.screen = Some(Box::new(GuiDisconnected::new(
                            "Failed to start server",
                            msg,
                            Parent::MainMenu,
                        )));
                    }
                }
            }
            GuiAction::Connect { host, port } => {
                app.game = GameState::empty_for_server(renderer.aspect());
                renderer.upload_world(&app.game.world);
                app.network = Some(start_network(
                    host.clone(),
                    port,
                    &app.ms_session,
                    &app.username,
                ));
                app.connecting = true;
                app.in_world = false;
                app.screen = Some(Box::new(GuiConnecting { host, port }));
            }
            GuiAction::QuitToTitle => {
                app.network = None;
                app.local_server = None;
                app.connecting = false;
                app.in_world = false;
                // Drop the session world; the title screen needs none.
                app.game = GameState::empty_for_server(renderer.aspect());
                renderer.upload_world(&app.game.world);
                app.screen = Some(Box::new(GuiMainMenu::new()));
            }
            GuiAction::Quit => app.quit = true,
            GuiAction::SendChat(message) => {
                app.game.chat.record_sent(message.clone());
                match &app.network {
                    Some(network) => {
                        network.send_packet(ServerboundPacket::ChatMessage { message })
                    }
                    None => {
                        // Demo world: handle slash commands locally, else echo.
                        if let Some(cmd) = message.strip_prefix('/') {
                            let reply = app.game.run_demo_command(cmd);
                            app.game.chat.push_message(reply);
                        } else {
                            let name = app
                                .session_username()
                                .unwrap_or(&app.username)
                                .to_owned();
                            app.game.chat.push_message(format!("<{name}> {message}"));
                        }
                    }
                }
            }
            GuiAction::RequestRespawn => app.game.request_respawn(),
            GuiAction::StartMicrosoftLogin => app.begin_login(None),
            GuiAction::LoginWithToken(token) => app.begin_login(Some(token)),
            GuiAction::UseAccount(uuid) => {
                let token = app
                    .accounts
                    .accounts
                    .iter()
                    .find(|account| account.uuid == uuid)
                    .map(|account| account.refresh_token.clone());
                if let Some(token) = token {
                    app.begin_login(Some(token));
                }
            }
            GuiAction::RemoveAccount(uuid) => {
                app.accounts.remove(&uuid);
                app.accounts.save();
            }
            GuiAction::CopyActiveToken => {
                let token = app
                    .ms_session
                    .as_ref()
                    .and_then(|session| session.refresh_token.clone())
                    .or_else(|| {
                        app.accounts
                            .accounts
                            .first()
                            .map(|account| account.refresh_token.clone())
                    });
                if let (Some(token), Some(clipboard)) = (token, app.clipboard.as_mut()) {
                    if clipboard.set_text(token).is_ok() {
                        log::info!("copied refresh token to clipboard");
                    }
                }
            }
            GuiAction::SetVsync(on) => renderer.set_vsync(on),
            GuiAction::SetRenderScale(scale) => renderer.set_render_scale(scale),
            GuiAction::SetRenderDistance(chunks) => renderer.set_render_distance(chunks),
            GuiAction::SetAdaptiveResolution(on) => {
                // Turning it off restores the user's manual render scale; the loop
                // takes over (from that scale) when it's on.
                if !on {
                    renderer.set_render_scale(app.settings.render_scale);
                }
            }
            GuiAction::SetSmoothLighting(on) => {
                // Cube meshing differs (greedy/flat vs per-block/smooth), so re-mesh
                // the loaded world; the worker now reads the new flag.
                renderer.set_smooth_lighting(on);
                app.game.mark_all_sections_dirty();
            }
            GuiAction::SetFancyGraphics(on) => {
                renderer.set_fancy_graphics(on);
                // Leaf geometry depends on Fast/Fancy, so re-mesh the world (the
                // worker now reads the new flag); spread over frames by the budget.
                app.game.mark_all_sections_dirty();
            }
            GuiAction::SetMipmapLevels(levels) => renderer.set_mipmap_levels(levels),
            GuiAction::SetResolution => apply_display(window, &app.settings),
            GuiAction::SetFullscreen => apply_display(window, &app.settings),
            GuiAction::SetShaders(on) => {
                // Shaders force smooth meshing; if that flips the mesh format
                // (Smooth Light was off), re-mesh the world so the vertex format
                // matches the pipeline that will draw it.
                let was_flat = renderer.flat_meshing();
                renderer.set_shaders_enabled(on);
                if renderer.flat_meshing() != was_flat {
                    app.game.mark_all_sections_dirty();
                }
            }
            GuiAction::SetShaderShadows(on) => renderer.set_shadows_enabled(on),
            GuiAction::SetShaderSpecular(on) => renderer.set_specular_enabled(on),
            GuiAction::SetShaderFog(on) => renderer.set_fog_enabled(on),
            GuiAction::SetShaderBloom(on) => renderer.set_bloom_enabled(on),
            GuiAction::SetBrightness(v) => renderer.set_brightness(v),
            GuiAction::SetVignette(on) => renderer.set_vignette_enabled(on),
            GuiAction::SetChromatic(on) => renderer.set_chromatic_enabled(on),
            GuiAction::SetDof(on) => renderer.set_dof_enabled(on),
            GuiAction::SetMotionBlur(on) => renderer.set_motion_blur_enabled(on),
            GuiAction::SetAutoExposure(on) => renderer.set_auto_exposure_enabled(on),
            GuiAction::SetClouds(on) => renderer.set_clouds_enabled(on),
            GuiAction::SetVolumetricLight(on) => renderer.set_volumetric_light_enabled(on),
            GuiAction::SaveSettings => app.settings.save(),
            GuiAction::SendPacket(packet) => {
                // Buffered, not sent now: flushed just before the next tick's
                // flying packet (see the tick loop) so the order matches vanilla
                // runTick and Grim's Post check stays happy.
                app.pending_window_packets.push(packet);
            }
            GuiAction::OpenUrl(url) => open_url(&url),
            GuiAction::ReloadResourcePack(path) => {
                log::info!("resource pack reload requested: {:?}", path);
                *atlas_uv = renderer.reload_atlas(path);
            }
        }
    }
}

/// Drain auth-thread events into screen transitions.
fn poll_auth_events(app: &mut App) {
    let Some(rx) = &app.auth_rx else { return };
    let mut clear = false;
    loop {
        match rx.try_recv() {
            Ok(AuthEvent::DeviceCode {
                user_code,
                verification_uri,
            }) => {
                app.screen = Some(Box::new(GuiAuthCode {
                    user_code,
                    verification_uri,
                }));
            }
            Ok(AuthEvent::Status(message)) => {
                app.screen = Some(Box::new(GuiProgress::new("Signing in...", message)));
            }
            Ok(AuthEvent::Success(session)) => {
                log::info!("MS login success: {} ({})", session.username, session.uuid);
                app.accounts.record_session(&session);
                app.ms_session = Some(session);
                app.screen = Some(Box::new(GuiAccounts::new()));
                clear = true;
                break;
            }
            Ok(AuthEvent::Failed(err)) => {
                log::warn!("MS login failed: {err}");
                app.screen = Some(Box::new(GuiDisconnected::new(
                    "Login Failed",
                    err,
                    Parent::Accounts,
                )));
                clear = true;
                break;
            }
            Err(_) => break,
        }
    }
    if clear {
        app.auth_rx = None;
    }
}

/// Drain the ext command queue and apply each command on the main thread.
/// Called after each dispatch seam so a mod's enqueued commands take effect the
/// same frame.
fn apply_ext_commands(app: &mut App) {
    for cmd in app.ext.take_commands() {
        match cmd {
            recraft_ext::ExtCommand::Chat(s) => {
                if let Some(net) = &app.network {
                    net.send_packet(ServerboundPacket::ChatMessage { message: s });
                }
            }
            recraft_ext::ExtCommand::SendServerbound(b) => {
                if let Some(net) = &app.network {
                    net.send_packet(ext_bridge::build_to_serverbound(b));
                }
            }
            recraft_ext::ExtCommand::Log(level, msg) => {
                let level = match level {
                    recraft_ext::LogLevel::Error => log::Level::Error,
                    recraft_ext::LogLevel::Warn => log::Level::Warn,
                    recraft_ext::LogLevel::Info => log::Level::Info,
                    recraft_ext::LogLevel::Debug => log::Level::Debug,
                };
                log::log!(level, "[ext] {msg}");
            }
            recraft_ext::ExtCommand::SpawnParticle {
                kind,
                x,
                y,
                z,
                ox,
                oy,
                oz,
                speed,
                count,
            } => {
                app.game.ext_spawn_particle(
                    kind,
                    glam::Vec3::new(x as f32, y as f32, z as f32),
                    glam::Vec3::new(ox, oy, oz),
                    speed,
                    count,
                );
            }
            recraft_ext::ExtCommand::PlaySound {
                event,
                x,
                y,
                z,
                volume,
                pitch,
            } => {
                app.game.ext_play_sound(
                    event,
                    glam::Vec3::new(x as f32, y as f32, z as f32),
                    volume,
                    pitch,
                );
            }
            recraft_ext::ExtCommand::Render(preset) => apply_render_preset(app, preset),
            recraft_ext::ExtCommand::SubmitGeometry { vertices, indices } => {
                app.ext_geometry = vertices
                    .iter()
                    .map(|v| recraft_render::ModelVertex {
                        position: [v[0], v[1], v[2]],
                        color: [v[3], v[4], v[5], v[6]],
                        uv: [v[7], v[8]],
                    })
                    .collect();
                app.ext_indices = indices;
                app.geometry_dirty = true;
            }
            recraft_ext::ExtCommand::RegisterBlock {
                id,
                texture,
                opaque,
                alpha,
                luminance,
                tint,
            } => {
                let tint = match tint {
                    Some(rgb) => recraft_core::Tint::Rgb(rgb),
                    None => recraft_core::Tint::None,
                };
                recraft_core::blocks::register_block(
                    id,
                    recraft_core::blocks::BlockDef::mod_cube(texture, opaque, alpha, tint),
                    luminance,
                );
            }
        }
    }
}

/// Apply a preset render modification. `BlockTint` accumulates into `app` (a
/// dirty flag drives a re-mesh in `about_to_wait` where the renderer is in
/// scope); the rest are not yet wired to the renderer.
fn apply_render_preset(app: &mut App, preset: recraft_ext::RenderPreset) {
    use recraft_ext::RenderPreset as P;
    match preset {
        P::BlockTint {
            block_id,
            meta,
            color,
        } => {
            app.block_tints.set(block_id, meta, color);
            app.tints_dirty = true;
        }
        P::Fullbright(on) => app.ext_fullbright = on,
        P::NametagScale(scale) => app.ext_nametag_scale = scale,
        P::ParticleDensity(density) => app.game.set_particle_density(density),
        P::ChunkBorders(on) => app.ext_chunk_borders = on,
        P::EntityBox { color, enabled, .. } => {
            app.ext_entity_box = enabled.then_some(color);
        }
    }
}

/// Build this frame's debug-overlay line geometry from the active ESP-style
/// presets (target block outline / chunk borders / entity boxes).
fn build_debug_lines(app: &App) -> Vec<recraft_render::LineVertex> {
    let mut out = Vec::new();
    let game = &app.game;
    // Built-in vanilla-style black outline on the targeted block (always on).
    if let Some(game::InteractionTarget::Block { x, y, z, .. }) = game.pick_target() {
        let e = 0.002;
        push_box(
            &mut out,
            [x as f32 - e, y as f32 - e, z as f32 - e],
            [x as f32 + 1.0 + e, y as f32 + 1.0 + e, z as f32 + 1.0 + e],
            [0.0, 0.0, 0.0, 0.6],
        );
    }
    if app.ext_chunk_borders {
        let p = game.player_position();
        let cx = (p.x.floor() as i32).div_euclid(16) * 16;
        let cz = (p.z.floor() as i32).div_euclid(16) * 16;
        let color = [0.2, 0.8, 1.0, 0.5];
        let v = |x: f32, y: f32, z: f32| recraft_render::LineVertex {
            position: [x, y, z],
            color,
        };
        // Vertical lines along the player's chunk's two near borders (16-grid).
        for i in 0..=16 {
            let fx = (cx + i) as f32;
            out.push(v(fx, 0.0, cz as f32));
            out.push(v(fx, 256.0, cz as f32));
            let fz = (cz + i) as f32;
            out.push(v(cx as f32, 0.0, fz));
            out.push(v(cx as f32, 256.0, fz));
        }
    }
    out
}

/// Build this frame's thick `entityBox` hitbox geometry: each entity's AABB
/// wireframe rendered as thin solid boxes (tubes) so the lines look thick.
fn build_debug_tris(app: &App) -> Vec<recraft_render::LineVertex> {
    let mut out = Vec::new();
    if let Some(rgb) = app.ext_entity_box {
        let color = [rgb[0], rgb[1], rgb[2], 1.0];
        for e in GameViews(&app.game).entities() {
            let (hw, h) = match e.kind {
                recraft_ext::EntityKindView::Player | recraft_ext::EntityKindView::Mob(_) => {
                    (0.3, 1.8)
                }
                _ => (0.15, 0.3),
            };
            push_tube_box(
                &mut out,
                [e.x as f32 - hw, e.y as f32, e.z as f32 - hw],
                [e.x as f32 + hw, e.y as f32 + h, e.z as f32 + hw],
                0.04,
                color,
            );
        }
    }
    out
}

/// Push the 12 edges of an axis-aligned box as line segments (vertex pairs).
fn push_box(out: &mut Vec<recraft_render::LineVertex>, min: [f32; 3], max: [f32; 3], color: [f32; 4]) {
    let v = |x: f32, y: f32, z: f32| recraft_render::LineVertex {
        position: [x, y, z],
        color,
    };
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    let corners = [
        [x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1],
        [x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1],
    ];
    const EDGES: [(usize, usize); 12] = [
        (0, 1), (1, 2), (2, 3), (3, 0),
        (4, 5), (5, 6), (6, 7), (7, 4),
        (0, 4), (1, 5), (2, 6), (3, 7),
    ];
    for (a, b) in EDGES {
        out.push(v(corners[a][0], corners[a][1], corners[a][2]));
        out.push(v(corners[b][0], corners[b][1], corners[b][2]));
    }
}

/// Push an axis-aligned box as solid triangles (36 vertices, no culling).
fn push_solid_box(out: &mut Vec<recraft_render::LineVertex>, min: [f32; 3], max: [f32; 3], color: [f32; 4]) {
    let p = [
        [min[0], min[1], min[2]], [max[0], min[1], min[2]],
        [max[0], max[1], min[2]], [min[0], max[1], min[2]],
        [min[0], min[1], max[2]], [max[0], min[1], max[2]],
        [max[0], max[1], max[2]], [min[0], max[1], max[2]],
    ];
    // 6 faces (as quads of corner indices); winding ignored (cull_mode None).
    const FACES: [[usize; 4]; 6] = [
        [0, 1, 2, 3], [5, 4, 7, 6], [4, 0, 3, 7],
        [1, 5, 6, 2], [3, 2, 6, 7], [4, 5, 1, 0],
    ];
    for [a, b, c, d] in FACES {
        for i in [a, b, c, a, c, d] {
            out.push(recraft_render::LineVertex {
                position: p[i],
                color,
            });
        }
    }
}

/// Push the 12 edges of an axis-aligned box as thin solid boxes of width `t` —
/// a "thick wireframe" (the entityBox hitbox).
fn push_tube_box(out: &mut Vec<recraft_render::LineVertex>, min: [f32; 3], max: [f32; 3], t: f32, color: [f32; 4]) {
    let h = t * 0.5;
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    for y in [y0, y1] {
        for z in [z0, z1] {
            push_solid_box(out, [x0 - h, y - h, z - h], [x1 + h, y + h, z + h], color);
        }
    }
    for x in [x0, x1] {
        for z in [z0, z1] {
            push_solid_box(out, [x - h, y0 - h, z - h], [x + h, y1 + h, z + h], color);
        }
    }
    for x in [x0, x1] {
        for y in [y0, y1] {
            push_solid_box(out, [x - h, y - h, z0 - h], [x + h, y + h, z1 + h], color);
        }
    }
}

/// Project a winit physical key to the stable name the ext layer expects
/// (`InputEvent.key`, e.g. `"KeyW"`, `"F6"`, `"Escape"` — winit's `KeyCode`
/// debug name). Returns `None` for keys with no physical code.
fn key_name(physical: &winit::keyboard::PhysicalKey) -> Option<String> {
    match physical {
        winit::keyboard::PhysicalKey::Code(code) => Some(format!("{code:?}")),
        _ => None,
    }
}

/// Replay a mod's `draw_hud` command buffer into the frame's `UiFrame`, layered
/// over the vanilla HUD. `recraft_ext` authors coordinates in GUI pixels; the
/// vanilla HUD authors every primitive as `<gui_px> * scale` (UiFrame divides by
/// the pixel scale at rasterize), so we multiply coords and the text scale by
/// `scale`. Colors are packed `0xRRGGBBAA`.
fn replay_hud(ui: &mut recraft_render::UiFrame, draw: &recraft_ext::HudDraw, scale: i32) {
    use recraft_ext::HudCmd;
    use recraft_render::{UiColor, UiRect};

    fn color(c: u32) -> UiColor {
        UiColor::rgba((c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8)
    }

    for cmd in draw.commands() {
        match cmd {
            HudCmd::Rect { x, y, w, h, color: c } => {
                ui.rect(
                    UiRect::new(x * scale, y * scale, w * scale, h * scale),
                    color(*c),
                );
            }
            HudCmd::Text {
                x,
                y,
                scale: text_scale,
                color: c,
                text,
                shadow,
            } => {
                let s = (text_scale * scale).max(1);
                if *shadow {
                    ui.text_shadowed(x * scale, y * scale, s, color(*c), text.clone());
                } else {
                    ui.text(x * scale, y * scale, s, color(*c), text.clone());
                }
            }
            HudCmd::ItemIcon { x, y, w, h, item_id } => {
                ui.item_icon(
                    UiRect::new(x * scale, y * scale, w * scale, h * scale),
                    *item_id,
                );
            }
            HudCmd::BlockItem {
                x,
                y,
                w,
                h,
                block_id,
                meta,
            } => {
                ui.block_item(
                    UiRect::new(x * scale, y * scale, w * scale, h * scale),
                    *block_id,
                    *meta,
                );
            }
            HudCmd::Image { .. } => {
                // TODO: needs a registered host texture (TexHandle -> RGBA) to blit.
            }
        }
    }
}

/// Drain all pending network events. Transaction pongs are sent immediately
/// on the network thread, so all preceding game-state packets (especially
/// DestroyEntities) must be processed before the next tick — otherwise the
/// server (Grim) considers entities removed while the client still sees them,
/// causing BadPacketsW ("interacted with non-existent entity").
fn pump_network(app: &mut App, window: &winit::window::Window, cursor_captured: &mut bool) {
    let Some(network) = &app.network else { return };
    let mut disconnect: Option<String> = None;
    loop {
        match network.events.try_recv() {
            Ok(NetworkEvent::Connected { username, uuid }) => {
                log::info!("logged in as {username} ({uuid})");
            }
            Ok(NetworkEvent::PlayPacket(packet)) => {
                // ext seam: project a stable view + derive events BEFORE the
                // packet is moved into apply_play_packet. A mod may `Drop` it.
                // Commands queued here are drained after pump_network returns
                // (we can't take &mut App while `network` borrows app.network).
                let view = ext_bridge::clientbound_view(&packet);
                let events = ext_bridge::derive_events(&packet);
                let drop = app
                    .ext
                    .dispatch_clientbound_packet(&view, &GameViews(&app.game))
                    == recraft_ext::Verdict::Drop;
                if !drop {
                    app.game.apply_play_packet(packet);
                    for ev in events {
                        app.ext.dispatch_event(&ev, &GameViews(&app.game));
                    }
                }
            }
            Ok(NetworkEvent::ChunkColumn { x, z, column }) => {
                app.game.apply_chunk_column(x, z, &column);
                app.ext.dispatch_event(
                    &recraft_ext::ExtEvent::ChunkLoad { x, z },
                    &GameViews(&app.game),
                );
            }
            Ok(NetworkEvent::ChunkUnload { x, z }) => {
                app.game.unload_chunk(x, z);
                app.ext.dispatch_event(
                    &recraft_ext::ExtEvent::ChunkUnload { x, z },
                    &GameViews(&app.game),
                );
            }
            Ok(NetworkEvent::Disconnected(message)) => {
                log::warn!("network disconnected: {message}");
                disconnect = Some(message);
                break;
            }
            Err(_) => break,
        }
    }

    if let Some(message) = disconnect {
        let was_singleplayer = app.local_server.is_some();
        app.network = None;
        app.local_server = None;
        app.in_world = false;
        app.connecting = false;
        let parent = if was_singleplayer {
            Parent::MainMenu
        } else {
            Parent::Multiplayer
        };
        app.screen = Some(Box::new(GuiDisconnected::new(
            "Connection Lost",
            message,
            parent,
        )));
    } else if app.connecting && app.game.loaded_chunk_count() > 0 {
        // World data has arrived: enter gameplay.
        app.connecting = false;
        app.in_world = true;
        // Drop any GUI packets buffered by a previous session that never flushed
        // (e.g. a container click right before disconnect) so they can't leak out
        // on this session's first tick.
        app.pending_window_packets.clear();
        app.screen = None;
    }
    sync_cursor(window, cursor_captured, app);
}

/// Keep the OS cursor grab in sync: captured exactly while playing with no
/// screen open.
fn sync_cursor(window: &winit::window::Window, captured: &mut bool, app: &App) {
    let want = app.in_world && app.screen.is_none();
    if want != *captured {
        if want {
            capture_cursor(window);
        } else {
            release_cursor(window);
        }
        *captured = want;
    }
}

/// Accumulates `RenderStats` over a ~1s window during scripted-smoke runs and
/// logs the averaged frame breakdown, so a headed benchmark prints readable
/// profiler numbers to the terminal. `gpu_us` (timestamp query) is the
/// occlusion-proof GPU figure; a large `acquire`/`present` with tiny gpu/cpu
/// means the frame is present/swapchain-bound rather than compute-bound.
struct SmokeProfile {
    window_start: Instant,
    frames: u32,
    gpu_us: u64,
    acquire_us: u64,
    prepare_us: u64,
    encode_us: u64,
    submit_us: u64,
    present_us: u64,
    draws: u64,
    visible: u64,
    indices: u64,
}

impl SmokeProfile {
    fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            frames: 0,
            gpu_us: 0,
            acquire_us: 0,
            prepare_us: 0,
            encode_us: 0,
            submit_us: 0,
            present_us: 0,
            draws: 0,
            visible: 0,
            indices: 0,
        }
    }

    fn record(&mut self, s: RenderStats, now: Instant) {
        self.frames += 1;
        self.gpu_us += s.gpu_us as u64;
        self.acquire_us += s.acquire_us as u64;
        self.prepare_us += s.prepare_us as u64;
        self.encode_us += s.encode_us as u64;
        self.submit_us += s.submit_us as u64;
        self.present_us += s.present_us as u64;
        self.draws += s.draw_calls as u64;
        self.visible += s.visible_chunks as u64;
        self.indices += s.chunk_indices as u64;
        if (now - self.window_start).as_secs_f32() >= 1.0 {
            self.flush(now);
        }
    }

    fn flush(&mut self, now: Instant) {
        let elapsed = (now - self.window_start).as_secs_f32();
        if self.frames == 0 || elapsed <= 0.0 {
            return;
        }
        let n = self.frames as f64;
        let fps = self.frames as f32 / elapsed;
        let frame_ms = 1000.0 / fps as f64;
        let accounted_ms = (self.acquire_us + self.prepare_us + self.encode_us
            + self.submit_us + self.present_us) as f64
            / n
            / 1000.0;
        let other_ms = frame_ms - accounted_ms;
        log::info!(
            "profile: {fps:.0} fps ({frame_ms:.2} ms) | gpu {:.2}ms acquire {:.2}ms present {:.2}ms \
             prepare {:.0}us encode {:.0}us submit {:.0}us | other {other_ms:.2}ms | draws {:.0} visible {:.0} tris {}",
            self.gpu_us as f64 / n / 1000.0,
            self.acquire_us as f64 / n / 1000.0,
            self.present_us as f64 / n / 1000.0,
            self.prepare_us as f64 / n,
            self.encode_us as f64 / n,
            self.submit_us as f64 / n,
            self.draws as f64 / n,
            self.visible as f64 / n,
            (self.indices as f64 / n / 3.0) as u64,
        );
        *self = Self::new(now);
    }
}

/// Render-pass skip configurations cycled by the in-process pass benchmark, as
/// `(skip_sky, skip_water, skip_ui)`. Interleaving them frame by frame within one
/// run keeps the thermal/clock state identical across configs, so the per-config
/// averages are directly comparable (unlike separate processes, which drift).
const BENCH_PASS_CONFIGS: [(&str, (bool, bool, bool, bool)); 6] = [
    // (skip_sky, skip_water, skip_ui, flat_solid)
    ("base", (false, false, false, false)),
    ("no-sky", (true, false, false, false)),
    ("no-water", (false, true, false, false)),
    ("no-ui", (false, false, true, false)),
    ("flat", (false, false, false, true)),
    ("no-all", (true, true, true, false)),
];

/// Accumulates per-config frame time and `acquire` across a single run, cycling
/// the active config every frame. `acquire` is the GPU-time proxy on hardware
/// whose timestamp queries return nothing (the Intel iGPU); frame time (cap off)
/// is the end-to-end measure.
struct PassBench {
    start: Instant,
    last: Instant,
    warmup: f32,
    frame: u64,
    current: usize,
    frame_us: [u64; 6],
    acquire_us: [u64; 6],
    count: [u64; 6],
}

impl PassBench {
    fn new(now: Instant, _duration: f32) -> Self {
        Self {
            start: now,
            last: now,
            warmup: 1.5,
            frame: 0,
            current: 0,
            frame_us: [0; 6],
            acquire_us: [0; 6],
            count: [0; 6],
        }
    }

    /// Pick the config for the frame about to be rendered (round-robin).
    fn config_for_frame(&mut self) -> (bool, bool, bool, bool) {
        self.current = (self.frame % BENCH_PASS_CONFIGS.len() as u64) as usize;
        BENCH_PASS_CONFIGS[self.current].1
    }

    fn record(&mut self, stats: RenderStats, now: Instant) {
        let frame_us = (now - self.last).as_micros() as u64;
        self.last = now;
        self.frame += 1;
        // Skip warmup frames (chunk meshing/upload spikes) so they don't bias
        // whichever config happened to land on them.
        if (now - self.start).as_secs_f32() < self.warmup {
            return;
        }
        let i = self.current;
        self.frame_us[i] += frame_us;
        self.acquire_us[i] += stats.acquire_us as u64;
        self.count[i] += 1;
    }

    fn report(&self) {
        let avg = |sum: u64, n: u64| if n > 0 { sum as f64 / n as f64 } else { 0.0 };
        let base_frame = avg(self.frame_us[0], self.count[0]);
        log::info!("=== pass benchmark (fps cap OFF, configs interleaved per frame) ===");
        for (i, (name, _)) in BENCH_PASS_CONFIGS.iter().enumerate() {
            let n = self.count[i];
            let frame_us = avg(self.frame_us[i], n);
            let frame_ms = frame_us / 1000.0;
            let fps = if frame_ms > 0.0 { 1000.0 / frame_ms } else { 0.0 };
            let acquire_ms = avg(self.acquire_us[i], n) / 1000.0;
            let delta_ms = (frame_us - base_frame) / 1000.0;
            log::info!(
                "{name:<8} frames {n:>5} | frame {frame_ms:>6.2}ms ({fps:>3.0} fps) | \
                 acquire {acquire_ms:>5.2}ms | Δframe {delta_ms:>+6.2}ms vs base",
            );
        }
    }
}

/// Append a (vertices, indices) mesh onto an accumulator buffer, rebasing the
/// indices onto the existing vertex count. Used to merge the dropped-item,
/// projectile, falling-block and player-held geometry into the world-item pass.
fn append_mesh(
    vertices: &mut Vec<recraft_render::Vertex>,
    indices: &mut Vec<u32>,
    mesh: (Vec<recraft_render::Vertex>, Vec<u32>),
) {
    let base = vertices.len() as u32;
    vertices.extend(mesh.0);
    indices.extend(mesh.1.iter().map(|i| i + base));
}

/// Build and hand the renderer the inventory player-preview, or clear it when
/// the player-inventory window isn't open (vanilla `GuiInventory`). The biped is
/// built at feet origin with the body yaw and head pose set from the cursor; the
/// whole-model lean and projection into the panel are done by the renderer.
fn build_inventory_preview(
    renderer: &mut Renderer,
    app: &App,
    width: i32,
    height: i32,
    cursor_position: (f64, f64),
) {
    use crate::container::WindowKind;
    use recraft_core::EntityKind;
    use recraft_render::EntityAnim;

    // Only the player inventory shows the preview — server containers do not.
    let open = matches!(
        app.game.open_container().map(|c| c.kind),
        Some(WindowKind::Player)
    );
    if !app.in_world || app.screen.is_none() || !open {
        renderer.set_inventory_preview(None);
        return;
    }
    let container = app.game.open_container().expect("player container open");

    let scale = gui::gui_scale(width, height);
    // Window origin (vanilla `guiLeft`/`guiTop`): the panel is centred.
    let origin_px = (
        (width - container.x_size * scale) / 2,
        (height - container.y_size * scale) / 2,
    );
    let origin_gui = (origin_px.0 as f32 / scale as f32, origin_px.1 as f32 / scale as f32);
    let mouse_gui = (
        cursor_position.0 as f32 / scale as f32,
        cursor_position.1 as f32 / scale as f32,
    );
    let pose = gui::inventory::preview_pose(mouse_gui, origin_gui);
    let (scissor, anchor, pixels_per_block) = gui::inventory::preview_layout(origin_px, scale);

    // Build the biped at feet origin, facing +z (toward the viewer); the body
    // yaw turns it, the head tracks the cursor relative to the body. Crouch is
    // never applied here (vanilla resets the pose for the preview).
    let anim = EntityAnim {
        net_head_yaw: pose.net_head_yaw,
        head_pitch: pose.head_pitch,
        ..Default::default()
    };
    let skin_row = app
        .game
        .local_skin_row(app.session_username().unwrap_or(&app.username), app.skin_manager.rows());
    let mut mesh = recraft_render::ModelMesh::new();
    mesh.push_entity(EntityKind::LocalPlayer, glam::Vec3::ZERO, pose.body_yaw, &anim, skin_row);

    renderer.set_inventory_preview(Some(&recraft_render::InventoryPreview {
        mesh: &mesh,
        anchor,
        pixels_per_block,
        tilt_rad: pose.tilt.to_radians(),
        scissor,
    }));
}

/// Render one frame: world, entities + first-person hand, HUD and the open
/// screen. Driven from `AboutToWait` so the frame rate is paced by our own
/// vsync/FPS-cap logic instead of macOS Core Animation throttling.
#[allow(clippy::too_many_arguments)]
fn render_frame(
    renderer: &mut Renderer,
    app: &mut App,
    window: &winit::window::Window,
    atlas_uv: &recraft_render::AtlasUv,
    overlay_textures: &recraft_render::OverlayTextures,
    fps_counter: &mut FpsCounter,
    last_frame: &mut Instant,
    tick_accumulator: f32,
    cursor_position: (f64, f64),
    mouse_down: bool,
    f3_debug: bool,
    smoke_active: bool,
) {
    let now = Instant::now();
    let frame_dt = (now - *last_frame).as_secs_f32().min(0.1);
    fps_counter.tick(now);
    *last_frame = now;

    // The GPU-time readback costs ~0.04 ms/frame, so only measure it when its
    // number is actually shown: the F3 overlay, or a scripted benchmark run.
    renderer.set_gpu_timing(f3_debug || smoke_active || app.settings.adaptive_resolution);

    // Render-distance safety net: free the GPU meshes of columns that drifted
    // out of view (keeping their block data) so resident VRAM stays bounded even
    // on servers that never send ChunkUnload. Runs only on a chunk-boundary
    // crossing; any re-mesh it needs is queued through the dirty budget below.
    let evicted = app.game.enforce_render_distance(app.settings.render_distance);
    if !evicted.is_empty() {
        renderer.drop_chunk_sections(&evicted);
    }

    // Local block prediction gets submitted to the background mesher first, but
    // never rebuilt on the render thread; placing/breaking must not stall a
    // frame.
    let urgent_chunks = app.game.take_urgent_remesh();
    if !urgent_chunks.is_empty() {
        renderer.queue_chunk_meshes(&app.game.world, urgent_chunks.iter().copied());
    }
    let dirty_budget = MESH_SUBMITS_PER_FRAME.saturating_sub(urgent_chunks.len());
    let dirty_chunks = app.game.take_dirty_chunks_budget(dirty_budget);
    if !dirty_chunks.is_empty() {
        renderer.queue_chunk_meshes(&app.game.world, dirty_chunks);
    }
    renderer.process_ready_meshes(&app.game.world, MESH_UPLOADS_PER_FRAME);

    let hud_visible = app.in_world
        && app
            .screen
            .as_ref()
            .is_none_or(|screen| screen.draws_over_hud());
    let tick_alpha = (tick_accumulator / 0.05).clamp(0.0, 1.0);
    if app.in_world {
        app.game.update_camera(tick_alpha);
        // Anchor the audio listener to the camera and play this frame's queued
        // sounds (from packets and local block prediction).
        app.sound.set_listener(sound::Listener {
            position: app.game.camera.position,
            yaw: app.game.camera.yaw,
        });
        for queued in app.game.take_sounds() {
            app.sound.play(&queued);
        }
        // Start skin downloads for newly-seen textured players, then upload any
        // that finished, so the entity model can sample their atlas rows.
        let new_skins: Vec<([u8; 16], String)> = app
            .game
            .player_list
            .iter()
            .filter(|(uuid, _)| !app.skin_manager.is_requested(uuid))
            .filter_map(|(uuid, info)| info.texture_property().map(|t| (*uuid, t.to_owned())))
            .collect();
        for (uuid, property) in new_skins {
            app.skin_manager.request(uuid, &property);
        }
        app.skin_manager.poll(renderer);

        // Skip the whole entity-model + hand + nametag rebuild and its GPU upload
        // when nothing that feeds them changed since last frame — the renderer
        // keeps the previously uploaded mesh. Saves the per-frame vertex generation
        // (the dominant entity cost) and upload in crowded-but-idle scenes.
        // Cull mobs shorter than terrain (capped at ENTITY_RENDER_CHUNKS) so weak
        // machines skip the distant crowd's per-frame articulated build.
        let entity_chunks = app.settings.render_distance.min(ENTITY_RENDER_CHUNKS);
        let entity_max_dist_sq = (entity_chunks as f64 * 16.0).powi(2);
        let entity_key = app.game.entity_render_fingerprint(
            tick_alpha,
            app.settings.brightness,
            hud_visible,
            app.skin_manager.rows(),
            entity_max_dist_sq,
        );
        if app.last_entity_key != Some(entity_key) {
            app.last_entity_key = Some(entity_key);
            let first_person = app.game.first_person_view(tick_alpha);
            app.game.build_entity_model(
                &mut app.entity_model,
                &mut app.entity_glint,
                tick_alpha,
                app.settings.brightness,
                app.skin_manager.rows(),
                entity_max_dist_sq,
                app.settings.old_animations,
            );
            // Chest block-entities draw in the same model pass (entity atlas).
            app.game.build_chest_models(
                &mut app.entity_model,
                app.settings.brightness,
                tick_alpha,
                entity_max_dist_sq,
            );
            // Signs, enchanting-table books and end-portal surfaces share the
            // model pass too; sign text is drawn separately on the board faces.
            let sign_texts = app.game.build_block_entity_models(
                &mut app.entity_model,
                app.settings.brightness,
                tick_alpha,
                entity_max_dist_sq,
            );
            renderer.set_sign_text(&sign_texts);
            if hud_visible {
                // Light the first-person hand + held item by the lightmap at the eye,
                // so they darken at night/in caves like the rest of the scene.
                let hand_light = app.game.world_light_factor(
                    app.game.camera.position,
                    app.settings.brightness,
                    tick_alpha,
                );
                let arm_start = app.entity_model.vertices.len();
                ItemRenderer::render_arm(&mut app.entity_model, &app.game.camera, &first_person);
                for v in &mut app.entity_model.vertices[arm_start..] {
                    v.color[0] *= hand_light;
                    v.color[1] *= hand_light;
                    v.color[2] *= hand_light;
                }
                let mut held = ItemRenderer::build_held_item(
                    &app.game.camera,
                    &first_person,
                    atlas_uv,
                    app.settings.old_animations,
                );
                for v in &mut held.vertices {
                    v.color[0] *= hand_light;
                    v.color[1] *= hand_light;
                    v.color[2] *= hand_light;
                }
                renderer.set_first_person_item(&held.vertices, &held.indices);
                renderer
                    .set_first_person_item_glint(&held.glint_vertices, &held.glint_indices);
                // 3D world-space player nametags (billboarded, depth-occluded).
                let nametags = app.game.player_nametags(tick_alpha);
                renderer.set_nametags(&app.game.camera, &nametags);
            } else {
                renderer.set_first_person_item(&[], &[]);
                renderer.set_first_person_item_glint(&[], &[]);
                renderer.set_nametags(&app.game.camera, &[]);
            }
            renderer.upload_model(&app.entity_model);
            renderer.set_entity_glint(&app.entity_glint);
        }
        // Dropped items, projectile sprites and falling-block cubes all share
        // the world-item pass (it binds the block/item atlas). Projectiles reuse
        // the dropped-item sprite path mapped to an item id.
        let mut dropped = app.game.dropped_items(tick_alpha);
        dropped.extend(app.game.projectiles(tick_alpha));
        let mut world_items =
            ItemRenderer::build_world_items(&app.game.camera, &dropped, atlas_uv);
        let mut falling = app.game.falling_block_cubes(tick_alpha);
        falling.extend(app.game.primed_tnt_cubes(tick_alpha));
        append_mesh(
            &mut world_items.vertices,
            &mut world_items.indices,
            ItemRenderer::build_falling_blocks(&falling, atlas_uv),
        );
        let held = app.game.player_held_items(tick_alpha, app.settings.old_animations);
        world_items.extend(ItemRenderer::build_player_held_items(&held, atlas_uv));
        renderer.set_world_items(&world_items.vertices, &world_items.indices);
        renderer
            .set_world_items_glint(&world_items.glint_vertices, &world_items.glint_indices);
        // Particle billboards (rebuilt every frame; not cached by entity_key
        // since particles move/age continuously).
        let particles = app.game.particle_billboards(tick_alpha);
        let (particle_v, particle_i) =
            recraft_render::build_particle_mesh(&app.game.camera, &particles);
        renderer.set_particles(&particle_v, &particle_i);
        // Block-break debris (vanilla EntityDiggingFX): billboards sampling the
        // broken block's terrain tile, drawn against the block atlas.
        let debris = app.game.block_debris(tick_alpha);
        let (debris_v, debris_i) =
            ItemRenderer::build_block_particles(&app.game.camera, &debris, atlas_uv);
        renderer.set_block_particles(&debris_v, &debris_i);
        // Experience-orb billboards (colour-cycle continuously, so also rebuilt
        // each frame against their own texture).
        let orbs = app.game.xp_orbs(tick_alpha);
        let (orb_v, orb_i) = recraft_render::build_particle_mesh(&app.game.camera, &orbs);
        renderer.set_xp_orbs(&orb_v, &orb_i);
    } else {
        app.last_entity_key = None;
        renderer.upload_model(&recraft_render::ModelMesh::new());
        renderer.set_entity_glint(&recraft_render::ModelMesh::new());
        renderer.set_first_person_item(&[], &[]);
        renderer.set_first_person_item_glint(&[], &[]);
        renderer.set_world_items(&[], &[]);
        renderer.set_world_items_glint(&[], &[]);
        renderer.set_particles(&[], &[]);
        renderer.set_block_particles(&[], &[]);
        renderer.set_xp_orbs(&[], &[]);
        renderer.set_nametags(&app.game.camera, &[]);
        renderer.set_sign_text(&[]);
    }
    // Mining crack overlay (vanilla destroy_stage_N textures over the dig target).
    renderer.set_break_overlay(app.game.breaking_overlay());

    // Build the frame's UI: HUD beneath, screen on top.
    let size = window.inner_size();
    let (width, height) = (size.width as i32, size.height as i32);
    let account_entries = app.account_entries();
    let mut ui = recraft_render::UiFrame::new();

    // Inventory player preview (vanilla `GuiInventory.drawEntityOnScreen`): when
    // the player-inventory window is open, render the local biped looking toward
    // the cursor in the top-left panel. Built into its own short-lived mesh, then
    // projected + scissored to the panel by the renderer.
    build_inventory_preview(renderer, app, width, height, cursor_position);

    let App {
        screen,
        game,
        settings,
        ms_session,
        tab_open,
        ext,
        ..
    } = app;
    // The overlay only makes sense in pure gameplay, never under an open screen.
    let tab_open = *tab_open && screen.is_none();
    let hud = HudState {
        health: game.health(),
        food: game.food(),
        vitals: game.hud_vitals(),
        armor: game.armor(),
        xp_bar: game.xp_bar(),
        xp_level: game.xp_level(),
        selected_slot: game.selected_slot(),
        hotbar: game.hotbar_items(),
        inventory: game.inventory_slots(),
        container: game.open_container(),
        cursor_item: game.cursor_item(),
        chat: &game.chat,
        scoreboard: &game.scoreboard,
        player_list: &game.player_list,
        tab_open,
        title: game.title_overlay(tick_accumulator / 0.05),
        screen_overlay: game.screen_overlay(),
        overlay_textures: &overlay_textures,
        boss: game.boss_bar(),
    };
    let wants_panorama = screen
        .as_ref()
        .is_some_and(|s| s.wants_panorama());
    let has_panorama = wants_panorama && renderer.has_panorama();
    let screen_open = screen.is_some();
    let dim_world = screen.as_ref().is_some_and(|s| s.dims_world());

    // Three-layer composite so an open overlay screen never hides the HUD:
    //   1. a dim scrim over the world (focus),
    //   2. the HUD on top of it (hotbar / status bars / chat / scoreboard, full
    //      brightness — all live in GuiIngame, so they share this layer),
    //   3. the screen's own panel / tooltips / held item on top of the HUD.
    if dim_world {
        gui::draw_world_scrim(&mut ui, width, height);
    }
    if hud_visible {
        // The render stats are the previous frame's (collected after the draw);
        // one frame stale is fine for a live debug readout.
        let debug = f3_debug.then(|| gui::ingame::DebugInfo {
            pos: game.player_position().to_array(),
            on_ground: game.player_on_ground(),
            yaw: game.camera.yaw,
            pitch: game.camera.pitch,
            stats: renderer.last_stats(),
        });
        let chat_input = screen.as_mut().and_then(|screen| screen.chat_input_mut());
        GuiIngame::render(
            &mut ui,
            width,
            height,
            fps_counter.fps(),
            game.loaded_chunk_count(),
            &hud,
            chat_input,
            debug.as_ref(),
            settings.show_fps,
            screen_open,
        );
    }
    // ext draw_hud seam: mod HUD draws over the vanilla HUD, under the open screen.
    if hud_visible {
        let scale = gui::gui_scale(width, height);
        let hud_ctx = recraft_ext::HudCtx {
            width: width / scale,
            height: height / scale,
            scale,
            screen_open,
        };
        let mut hud_draw = recraft_ext::HudDraw::new();
        ext.draw_hud(&mut hud_draw, &hud_ctx, &GameViews(&*game));
        replay_hud(&mut ui, &hud_draw, scale);
    }
    if let Some(screen) = screen.as_mut() {
        let ctx = DrawCtx {
            width,
            height,
            scale: gui::gui_scale(width, height),
            mouse: cursor_position,
            mouse_down,
            chunk_count: game.loaded_chunk_count(),
            in_world: app.in_world,
            has_panorama,
            settings,
            session_username: ms_session.as_ref().map(|s| s.username.as_str()),
            accounts: &account_entries,
            hud: Some(&hud),
        };
        screen.draw(&mut ui, &ctx);
    }

    // Drive the day/night sky and lightmap from the world clock (interpolated
    // for smooth motion between ticks).
    renderer.set_world_time(app.game.world_time(tick_alpha));
    if has_panorama {
        // Vanilla increments panoramaTimer once per tick (20 Hz).
        app.panorama_timer += frame_dt * 20.0;
        if let Err(err) = renderer.render_panorama(&ui, app.panorama_timer) {
            log::error!("render error: {err}");
        }
    } else if let Err(err) = renderer.render_with_ui(&app.game.camera, &ui) {
        log::error!("render error: {err}");
    }
}

/// Start a network connection, choosing premium or offline mode based on the
/// available session.
fn start_network(
    host: String,
    port: u16,
    ms_session: &Option<Session>,
    offline_username: &str,
) -> NetworkHandle {
    if let Some(session) = ms_session {
        log::info!(
            "connecting to {host}:{port} as {} (premium)",
            session.username
        );
        NetworkHandle::connect_premium_1_8_9(
            host,
            port,
            PremiumSession {
                access_token: session.access_token.clone(),
                uuid: session.uuid.clone(),
                username: session.username.clone(),
            },
        )
    } else {
        log::info!("connecting to {host}:{port} as {offline_username} (offline)");
        NetworkHandle::connect_offline_1_8_9(host, port, offline_username.to_owned())
    }
}

impl LaunchConfig {
    fn from_args() -> Self {
        let mut server = None;
        let mut username = "ReCraft".to_owned();
        let mut assets = None;
        let mut scripted_smoke_seconds = None;
        let mut headless_smoke_seconds = None;
        let mut headless_interact_seconds = None;
        let mut bench_passes_seconds = None;
        let mut window_size = None;
        let mut demo_kind = game::DemoKind::Landscape;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--connect" => {
                    if let Some(value) = args.next() {
                        server = servers::parse_server_address(&value);
                    }
                }
                "--username" => {
                    if let Some(value) = args.next() {
                        username = value;
                    }
                }
                "--assets" => {
                    if let Some(value) = args.next() {
                        assets = Some(PathBuf::from(value));
                    }
                }
                "--scripted-smoke" => {
                    scripted_smoke_seconds =
                        args.next().and_then(|value| value.parse::<f32>().ok());
                }
                "--headless-smoke" => {
                    headless_smoke_seconds =
                        args.next().and_then(|value| value.parse::<f32>().ok());
                }
                "--headless-interact" => {
                    headless_interact_seconds =
                        args.next().and_then(|value| value.parse::<f32>().ok());
                }
                "--bench-passes" => {
                    bench_passes_seconds =
                        args.next().and_then(|value| value.parse::<f32>().ok());
                }
                "--window" => {
                    window_size = args.next().and_then(|value| {
                        let (w, h) = value.split_once('x')?;
                        Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
                    });
                }
                "--demo" => {
                    if let Some(value) = args.next() {
                        demo_kind = match value.as_str() {
                            "chunk" => game::DemoKind::ChunkStress,
                            "entity" => game::DemoKind::EntityStress,
                            "terrain" => game::DemoKind::Terrain,
                            "cube" => game::DemoKind::SingleCube,
                            _ => game::DemoKind::Landscape,
                        };
                    }
                }
                _ => {}
            }
        }
        Self {
            server,
            username,
            assets,
            scripted_smoke_seconds,
            headless_smoke_seconds,
            headless_interact_seconds,
            bench_passes_seconds,
            window_size,
            demo_kind,
        }
    }
}

/// Stand still, aim down at the ground, and continuously mine the targeted block
/// — to exercise the block-interaction checks (rotation/reach) in isolation from
/// movement. The look is fixed and sent every tick, so any rotation-place/break
/// flag points at the interaction packets, not movement timing.
fn run_headless_interact(config: &LaunchConfig, seconds: f32) -> anyhow::Result<()> {
    let (host, port) = config
        .server
        .clone()
        .context("--headless-interact requires --connect <host:port>")?;
    log::info!(
        "headless-interact: connecting to {host}:{port} as {}",
        config.username
    );
    let network = NetworkHandle::connect_offline_1_8_9(host, port, config.username.clone());
    let mut game = GameState::empty_for_server(1.0);

    let start = Instant::now();
    let tick = Duration::from_millis(50);
    let mut next_tick = start;
    let mut in_game = false;
    let mut started_attack = false;
    let mut tick_count: u64 = 0;
    loop {
        let now = Instant::now();
        if (now - start).as_secs_f32() >= seconds {
            log::info!("headless-interact complete; FINAL {}", game.debug_state());
            return Ok(());
        }

        let mut budget = 40;
        while budget > 0 {
            match network.events.try_recv() {
                Ok(NetworkEvent::Connected { username, uuid }) => {
                    log::info!("logged in as {username} ({uuid})")
                }
                Ok(NetworkEvent::PlayPacket(packet)) => {
                    game.apply_play_packet(packet);
                    budget -= 1;
                }
                Ok(NetworkEvent::ChunkColumn { x, z, column }) => {
                    game.apply_chunk_column(x, z, &column);
                    budget -= 1;
                }
                Ok(NetworkEvent::ChunkUnload { x, z }) => game.unload_chunk(x, z),
                Ok(NetworkEvent::Disconnected(message)) => {
                    log::warn!("network disconnected: {message}");
                    return Ok(());
                }
                Err(_) => break,
            }
        }
        if !in_game && game.loaded_chunk_count() > 0 {
            in_game = true;
        }

        if game.is_dead() {
            game.request_respawn();
        }
        if game.take_respawn_request() {
            network.send_packet(ServerboundPacket::ClientStatus { action: 0 });
        }
        let _ = game.take_position_confirm();

        // Aim down (Minecraft: positive pitch looks down) until the crosshair
        // finds a ground block, sweeping pitch a little if needed. RECRAFT_DIG_DOWN
        // aims straight down (punch through dirt into stone) and disables the hotbar
        // switching so a long break can actually complete — for FastBreak testing.
        let dig_down = std::env::var("RECRAFT_DIG_DOWN").is_ok();
        if in_game {
            let mut pitch = if dig_down { 88.0 } else { 55.0 };
            game.debug_set_look(0.0, pitch);
            while !game.debug_has_block_target() && pitch < 88.0 {
                pitch += 5.0;
                game.debug_set_look(0.0, pitch);
            }

            // Drive the same per-tick action pipeline the windowed loop uses:
            // continuous mining (held), plus a hotbar switch every ~1.5 s to
            // exercise HeldItemChange ordering and the dig-reset-on-switch. The
            // intents are staged before the tick, which resolves them in order.
            let want_mine = game.debug_has_block_target();
            let attack_pressed = want_mine && !started_attack;
            if want_mine {
                started_attack = true;
            }
            game.set_pending_actions(game::TickActions {
                slot_select: if tick_count % 30 == 15 {
                    Some((tick_count / 30 % 9) as i32)
                } else {
                    None
                },
                slot_scroll: 0,
                attack_pressed,
                use_pressed: false,
                left_held: want_mine,
                right_held: false,
                old_animations: false,
            });
            if let Some((actions, movement)) = game.tick(0.05) {
                if game.can_send_movement_packets() {
                    for packet in actions {
                        network.send_packet(packet);
                    }
                    network.send_movement(movement);
                }
            }
            tick_count += 1;
        }

        next_tick += tick;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else {
            next_tick = now;
        }
    }
}

fn run_headless_smoke(config: &LaunchConfig, seconds: f32) -> anyhow::Result<()> {
    let (host, port) = config
        .server
        .clone()
        .context("--headless-smoke requires --connect <host:port>")?;
    log::info!(
        "headless: connecting to {host}:{port} as {}",
        config.username
    );
    let network = NetworkHandle::connect_offline_1_8_9(host, port, config.username.clone());
    let mut game = GameState::empty_for_server(1.0);

    let start = Instant::now();
    let tick = Duration::from_millis(50);
    let mut next_tick = start;
    let mut in_game = false;
    loop {
        let now = Instant::now();
        let elapsed = (now - start).as_secs_f32();
        if elapsed >= seconds {
            log::info!("headless smoke complete; FINAL {}", game.debug_state());
            return Ok(());
        }

        // Process at most a bounded number of packets per tick so a burst of
        // chunk data on join can't stall the simulation.
        let mut budget = 40;
        while budget > 0 {
            match network.events.try_recv() {
                Ok(NetworkEvent::Connected { username, uuid }) => {
                    log::info!("logged in as {username} ({uuid})")
                }
                Ok(NetworkEvent::PlayPacket(packet)) => {
                    game.apply_play_packet(packet);
                    budget -= 1;
                }
                Ok(NetworkEvent::ChunkColumn { x, z, column }) => {
                    game.apply_chunk_column(x, z, &column);
                    budget -= 1;
                }
                Ok(NetworkEvent::ChunkUnload { x, z }) => {
                    game.unload_chunk(x, z);
                }
                Ok(NetworkEvent::Disconnected(message)) => {
                    log::warn!("network disconnected: {message}");
                    return Ok(());
                }
                Err(_) => break,
            }
        }
        if !in_game && game.loaded_chunk_count() > 0 {
            in_game = true;
        }

        // Headless runs auto-respawn so the driver keeps exercising movement.
        if game.is_dead() {
            game.request_respawn();
        }
        if game.take_respawn_request() {
            network.send_packet(ServerboundPacket::ClientStatus { action: 0 });
        }

        let _ = game.take_position_confirm();
        game.apply_scripted_smoke_input(elapsed, seconds);
        if in_game {
            if let Some((_actions, movement)) = game.tick(0.05) {
                if game.can_send_movement_packets() {
                    network.send_movement(movement);
                }
            }
        }

        next_tick += tick;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else {
            next_tick = now;
        }
    }
}

/// Map a hotbar key (defaults 1-9) to a 0-based hotbar slot via the keybinds.
fn hotbar_slot_key(event: &winit::event::KeyEvent, keybinds: &Keybinds) -> Option<i32> {
    if event.state != ElementState::Pressed {
        return None;
    }
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };
    match keybinds.action_for(code) {
        Some(GameAction::Hotbar1) => Some(0),
        Some(GameAction::Hotbar2) => Some(1),
        Some(GameAction::Hotbar3) => Some(2),
        Some(GameAction::Hotbar4) => Some(3),
        Some(GameAction::Hotbar5) => Some(4),
        Some(GameAction::Hotbar6) => Some(5),
        Some(GameAction::Hotbar7) => Some(6),
        Some(GameAction::Hotbar8) => Some(7),
        Some(GameAction::Hotbar9) => Some(8),
        _ => None,
    }
}

/// If this key press opens the chat box, the text to pre-fill it with:
/// the Chat bind opens empty, the Command bind opens with a leading slash.
fn chat_open_key(event: &winit::event::KeyEvent, keybinds: &Keybinds) -> Option<String> {
    if event.state != ElementState::Pressed {
        return None;
    }
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };
    match keybinds.action_for(code) {
        Some(GameAction::Chat) => Some(String::new()),
        Some(GameAction::Command) => Some("/".to_owned()),
        _ => None,
    }
}

/// Open a URL from a chat `open_url` clickEvent using the OS opener. Only
/// http(s) links are opened (vanilla refuses other schemes); failures are
/// logged rather than surfaced.
fn open_url(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        log::warn!("ignoring non-http chat url: {url}");
        return;
    }
    #[cfg(target_os = "macos")]
    let opener = ("open", &[] as &[&str]);
    #[cfg(target_os = "windows")]
    let opener = ("cmd", &["/C", "start", ""] as &[&str]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let opener = ("xdg-open", &[] as &[&str]);

    let (program, args) = opener;
    let result = std::process::Command::new(program)
        .args(args)
        .arg(url)
        .spawn();
    if let Err(err) = result {
        log::warn!("failed to open url {url}: {err}");
    }
}

fn capture_cursor(window: &winit::window::Window) {
    if let Err(err) = window
        .set_cursor_grab(CursorGrabMode::Locked)
        .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
    {
        log::warn!("failed to grab cursor: {err}");
    }
    window.set_cursor_visible(false);
}

fn release_cursor(window: &winit::window::Window) {
    if let Err(err) = window.set_cursor_grab(CursorGrabMode::None) {
        log::warn!("failed to release cursor: {err}");
    }
    window.set_cursor_visible(true);
}
