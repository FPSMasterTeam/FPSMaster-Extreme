mod auth;
mod chat;
mod container;
mod game;
mod gui;
mod item_renderer;
mod network;
mod player_list;
mod scoreboard;
mod skin;
mod servers;
mod settings;
mod text_input;

use std::{
    env,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use anyhow::Context;
use auth::{AuthEvent, Session};
use game::GameState;
use gui::accounts::GuiAccounts;
use gui::chat_screen::GuiChat;
use gui::game_over::GuiGameOver;
use gui::ingame::{GuiIngame, HudState};
use gui::ingame_menu::GuiIngameMenu;
use gui::inventory::GuiContainer;
use gui::main_menu::GuiMainMenu;
use gui::progress::{GuiAuthCode, GuiConnecting, GuiDisconnected, GuiProgress, Parent};
use gui::{AccountEntry, DrawCtx, GuiAction, GuiScreen, ScreenCtx};
use item_renderer::ItemRenderer;
use network::{NetworkEvent, NetworkHandle};
use recraft_protocol::{net::PremiumSession, v1_8_9::packets::ServerboundPacket};
use recraft_render::{RenderStats, Renderer};
use settings::{FpsCounter, Settings};

/// Dirty sections snapshotted and handed to the background mesher each frame.
/// Sections of the same column share one snapshot clone (the only main-thread
/// cost); the mesh build runs off-thread. Higher than the old per-column budget
/// because a column now contributes several sections.
const MESH_SUBMITS_PER_FRAME: usize = 40;
/// Finished background section meshes uploaded to the GPU each frame.
const MESH_UPLOADS_PER_FRAME: usize = 48;
use winit::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{DeviceEvent, ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{CursorGrabMode, WindowBuilder},
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
    /// Vanilla `panoramaTimer` — incremented every frame on the title screen.
    panorama_timer: f32,
    /// Reused across frames so the per-frame entity rebuild keeps its vertex/index
    /// allocations instead of reallocating from empty each frame.
    entity_model: recraft_render::ModelMesh,
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

fn main() -> anyhow::Result<()> {
    // Default to showing info-level logs so diagnostics are visible without
    // having to set RUST_LOG; still overridable via RUST_LOG.
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
    let window = WindowBuilder::new()
        .with_title("ReCraft - Rust Minecraft Client")
        .with_inner_size(LogicalSize::new(1280.0, 720.0))
        // Start hidden so the OS never shows an empty white window while the
        // renderer loads textures; revealed after the first frame is drawn.
        .with_visible(false)
        .build(&event_loop)
        .context("create window")?;
    let window: &'static winit::window::Window = Box::leak(Box::new(window));
    release_cursor(window);

    let mut renderer = pollster::block_on(Renderer::new(window)).context("create renderer")?;
    let mut settings = Settings::default();
    // Both scripted-smoke and the pass benchmark auto-load the demo world and run
    // unthrottled.
    let auto_play = config
        .scripted_smoke_seconds
        .or(config.bench_passes_seconds)
        .is_some();
    if auto_play {
        settings.vsync = false;
        settings.fps_cap = u32::MAX;
    }
    renderer.set_vsync(settings.vsync);
    // Atlas UV table snapshot for first-person item geometry (cheap clone of
    // the name→tile map, taken once).
    let atlas_uv = renderer.atlas_uv().clone();

    let auto_connect = config.server.clone();
    let auto_demo = auto_play && auto_connect.is_none();
    let username = config.username.clone();

    let mut app = App {
        // The demo world is only built when actually entering it; menus run
        // over an empty world (hidden behind the dirt background anyway).
        game: if auto_demo {
            GameState::demo(config.demo_kind, renderer.aspect())
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
        panorama_timer: 0.0,
        entity_model: recraft_render::ModelMesh::new(),
        quit: false,
    };
    renderer.upload_world(&app.game.world);

    let mut cursor_captured = false;
    let mut cursor_position = (0.0f64, 0.0f64);
    // Held modifier keys (for text-field shortcuts like Ctrl/Cmd+V).
    let mut modifiers = ModifiersState::empty();
    // Whether IME input is currently enabled on the window. We turn it on only
    // while a text field is focused, so gameplay keys stay raw and no candidate
    // window pops up mid-game. `last_ime_area` avoids redundant area updates.
    let mut ime_enabled = false;
    let mut last_ime_area: Option<(i32, i32, i32, i32)> = None;
    let mut mouse_down_left = false;
    // Right button held over a screen, for right-button inventory paint-drag.
    let mut mouse_down_right = false;
    // Whether the left mouse button is held in gameplay (continuous mining).
    let mut left_held = false;
    // Whether the right mouse button is held in gameplay (sword blocking holds
    // the item in use until released).
    let mut right_held = false;
    // Per-tick input intents. Frame events only RECORD intent here; the tick loop
    // turns them into packets in vanilla order (held-item, then click actions, then
    // the flying packet) using that tick's player state — exactly how vanilla's
    // runTick processes input before onUpdateWalkingPlayer sends movement. This is
    // why interactions never land in Grim's "post-flying" window.
    let mut attack_pressed = false;
    let mut use_pressed = false;
    let mut slot_select: Option<i32> = None;
    let mut slot_scroll = 0i32;
    let mut was_dead = false;
    // F3 debug overlay (coords, chunk info, render profiler).
    let mut f3_debug = false;

    let mut last_frame = Instant::now();
    // The physics/network simulation advances on its own wall-clock so it runs
    // at a fixed 20 Hz regardless of how often the window actually redraws.
    let mut last_sim = Instant::now();
    let app_start = Instant::now();
    let mut fps_counter = FpsCounter::new(app_start);
    let mut tick_accumulator = 0.0f32;
    // Both scripted-smoke and the pass benchmark drive the scripted camera and
    // auto-exit, so combine them for those two purposes.
    let scripted_smoke_seconds = config.scripted_smoke_seconds.or(config.bench_passes_seconds);
    let scripted_smoke_static = matches!(
        config.demo_kind,
        game::DemoKind::ChunkStress | game::DemoKind::Terrain | game::DemoKind::SingleCube
    );
    let mut scripted_smoke_done = false;
    // During a scripted-smoke run, aggregate RenderStats over ~1s windows and log
    // the breakdown so headed benchmark runs print readable profiler numbers to
    // the terminal (instead of only the on-screen F3 overlay).
    let mut smoke_profile = config.scripted_smoke_seconds.map(|_| SmokeProfile::new(app_start));
    // In-process per-pass A/B benchmark (interleaves skip configs frame by frame).
    let mut pass_bench = config
        .bench_passes_seconds
        .map(|secs| PassBench::new(app_start, secs));
    // The window starts hidden; revealed after the first frame is presented so
    // the user never sees an empty white window during renderer/asset load.
    let mut window_shown = false;

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);

        // ── Poll auth events ────────────────────────────────────────────────
        poll_auth_events(&mut app);

        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    renderer.resize(size);
                    app.game.set_aspect(renderer.aspect());
                }
                WindowEvent::ModifiersChanged(state) => {
                    modifiers = state.state();
                }
                WindowEvent::Ime(ime) => {
                    // Route IME composition/commit to the focused text field.
                    if let Some(input) = app
                        .screen
                        .as_mut()
                        .and_then(|screen| screen.focused_text_input())
                    {
                        input.handle_ime(&ime);
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    // F3 toggles the debug overlay in-world, regardless of any
                    // open screen, but not while typing in a chat field.
                    if event.state == ElementState::Pressed
                        && !event.repeat
                        && matches!(event.physical_key, PhysicalKey::Code(KeyCode::F3))
                        && app.in_world
                        && app
                            .screen
                            .as_ref()
                            .is_none_or(|screen| screen.chat_input().is_none())
                    {
                        f3_debug = !f3_debug;
                    }
                    if app.screen.is_some() {
                        // Screen input: route through the screen, collect actions.
                        let mut taken = app.screen.take();
                        let actions = if let Some(screen) = taken.as_mut() {
                            let mut ctx = ScreenCtx {
                                game: &mut app.game,
                                settings: &mut app.settings,
                                clipboard: app.clipboard.as_mut(),
                                modifiers,
                                mouse: cursor_position,
                            };
                            screen.key_pressed(&event, &mut ctx)
                        } else {
                            Vec::new()
                        };
                        app.screen = taken;
                        handle_actions(&mut app, &mut renderer, actions);
                    } else if app.in_world {
                        // Gameplay input.
                        let pressed = event.state == ElementState::Pressed;
                        if pressed
                            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
                        {
                            app.suspend_gameplay_input(&mut left_held, &mut right_held);
                            app.screen = Some(Box::new(GuiIngameMenu::new()));
                        } else if pressed
                            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyE))
                        {
                            app.suspend_gameplay_input(&mut left_held, &mut right_held);
                            app.game.open_player_inventory();
                            app.screen = Some(Box::new(GuiContainer::new()));
                        } else if let Some(prefill) = chat_open_key(&event) {
                            app.suspend_gameplay_input(&mut left_held, &mut right_held);
                            app.game.chat.reset_recall();
                            app.screen = Some(Box::new(GuiChat::new(prefill)));
                        } else if let Some(slot) = hotbar_slot_key(&event) {
                            slot_select = Some(slot);
                        } else if matches!(
                            event.physical_key,
                            PhysicalKey::Code(KeyCode::Tab)
                        ) {
                            // Hold Tab to show the player-list overlay.
                            app.tab_open = pressed;
                        } else {
                            app.game.input.handle_key(event);
                        }
                    }
                    sync_cursor(window, &mut cursor_captured, &app);
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    let steps = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                        winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                    };
                    if let Some(screen) = app.screen.as_mut() {
                        screen.mouse_scrolled(steps);
                    } else if app.in_world {
                        // Scroll wheel cycles the hotbar (down = next slot); the
                        // packet is sent inside the tick.
                        slot_scroll += -steps.signum() as i32;
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor_position = (position.x, position.y);
                    if mouse_down_left || mouse_down_right {
                        let mut taken = app.screen.take();
                        if let Some(screen) = taken.as_mut() {
                            let mut ctx = ScreenCtx {
                                game: &mut app.game,
                                settings: &mut app.settings,
                                clipboard: app.clipboard.as_mut(),
                                modifiers,
                                mouse: cursor_position,
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
                            mouse_down_left = false;
                            left_held = false;
                        }
                        if is_right {
                            mouse_down_right = false;
                            right_held = false;
                        }
                        if app.screen.is_some() && (is_left || is_right) {
                            // Route the release to the screen (commits an
                            // inventory paint-drag, or collapses it to a click).
                            let mut taken = app.screen.take();
                            let actions = if let Some(screen) = taken.as_mut() {
                                let mut ctx = ScreenCtx {
                                    game: &mut app.game,
                                    settings: &mut app.settings,
                                    clipboard: app.clipboard.as_mut(),
                                    modifiers,
                                    mouse: cursor_position,
                                };
                                screen.mouse_released(
                                    cursor_position.0,
                                    cursor_position.1,
                                    is_right,
                                    &mut ctx,
                                )
                            } else {
                                Vec::new()
                            };
                            app.screen = taken;
                            handle_actions(&mut app, &mut renderer, actions);
                        }
                    } else if app.screen.is_some() {
                        if is_left || is_right || is_middle {
                            if is_left {
                                mouse_down_left = true;
                            } else if is_right {
                                mouse_down_right = true;
                            }
                            let mut taken = app.screen.take();
                            let actions = if let Some(screen) = taken.as_mut() {
                                let mut ctx = ScreenCtx {
                                    game: &mut app.game,
                                    settings: &mut app.settings,
                                    clipboard: app.clipboard.as_mut(),
                                    modifiers,
                                    mouse: cursor_position,
                                };
                                let (mx, my) = cursor_position;
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
                            handle_actions(&mut app, &mut renderer, actions);
                            sync_cursor(window, &mut cursor_captured, &app);
                        }
                    } else if app.in_world {
                        // Record the click edge; the tick turns it into the
                        // dig/use packets in vanilla order.
                        match button {
                            MouseButton::Left => {
                                mouse_down_left = true;
                                left_held = true;
                                attack_pressed = true;
                            }
                            MouseButton::Right => {
                                right_held = true;
                                use_pressed = true;
                            }
                            _ => {}
                        }
                        sync_cursor(window, &mut cursor_captured, &app);
                    }
                }
                WindowEvent::RedrawRequested => {
                    // Rendering is driven from AboutToWait (see `render_frame`).
                }
                _ => {}
            },
            Event::DeviceEvent {
                event: DeviceEvent::MouseMotion { delta },
                ..
            } if app.in_world && app.screen.is_none() => app.game.rotate_view(
                delta.0 as f32,
                delta.1 as f32,
                app.settings.mouse_factor(),
            ),
            Event::AboutToWait => {
                // --- Network event pump (bounded per iteration). ---
                pump_network(&mut app, window, &mut cursor_captured);

                // The server opened (S2D) or force-closed (S2E) a window: push or
                // pop the container screen to match.
                if app.game.take_window_open() {
                    app.suspend_gameplay_input(&mut left_held, &mut right_held);
                    app.screen = Some(Box::new(GuiContainer::new()));
                }
                if app.game.take_window_close() {
                    app.screen = None;
                }

                // Per-frame screen upkeep (ping results, auto-close).
                let mut taken = app.screen.take();
                let actions = if let Some(screen) = taken.as_mut() {
                    let mut ctx = ScreenCtx {
                        game: &mut app.game,
                        settings: &mut app.settings,
                        clipboard: app.clipboard.as_mut(),
                        modifiers,
                        mouse: cursor_position,
                    };
                    screen.update(&mut ctx)
                } else {
                    Vec::new()
                };
                app.screen = taken;
                handle_actions(&mut app, &mut renderer, actions);

                // Scripted runs auto-respawn so the smoke driver keeps moving.
                if scripted_smoke_seconds.is_some() && app.game.is_dead() {
                    app.game.request_respawn();
                }

                // Death screen on the rising edge (gameplay or chat overlay).
                let dead = app.in_world && app.game.is_dead();
                if dead && !was_dead {
                    let interruptible = app
                        .screen
                        .as_ref()
                        .is_none_or(|screen| screen.chat_input().is_some());
                    if interruptible {
                        app.screen = Some(Box::new(GuiGameOver::new()));
                    }
                }
                was_dead = dead;

                // If we asked to respawn, tell the server — a dead player is
                // frozen server-side and cannot move until it respawns.
                if app.game.take_respawn_request() {
                    if let Some(network) = &app.network {
                        network.send_packet(ServerboundPacket::ClientStatus { action: 0 });
                    }
                }

                // Teleport acks are sent synchronously by the network thread;
                // just drain the game-side pending flag.
                let _ = app.game.take_position_confirm();

                let now = Instant::now();
                let sim_dt = (now - last_sim).as_secs_f32().min(0.25);
                last_sim = now;
                if let Some(seconds) = scripted_smoke_seconds {
                    // The pass benchmark keeps the camera still so every config
                    // renders an identical frame — otherwise scene-to-scene
                    // variance from a moving camera swamps the per-pass deltas.
                    if !scripted_smoke_static && pass_bench.is_none() {
                        app.game
                            .apply_scripted_smoke_input((now - app_start).as_secs_f32(), seconds);
                    }
                }
                // The world keeps ticking (and reporting movement) while any
                // overlay screen is open, exactly like vanilla multiplayer.
                if app.in_world {
                    tick_accumulator += sim_dt;
                    while tick_accumulator >= 0.05 {
                        // Stage this tick's input intents; the tick resolves them
                        // (in vanilla order) BEFORE the move, so a sprint-attack or
                        // sword-block slowdown lands on this tick's flying packet.
                        app.game.set_pending_actions(game::TickActions {
                            slot_select,
                            slot_scroll,
                            attack_pressed,
                            use_pressed,
                            left_held,
                            right_held,
                        });
                        // `None` on the teleport-ack tick: hold, send no movement
                        // and keep the intents for the next tick.
                        if let Some((actions, movement)) = app.game.tick(0.05) {
                            slot_select = None;
                            slot_scroll = 0;
                            attack_pressed = false;
                            use_pressed = false;
                            // Abilities echo (C13) goes out after the click
                            // actions and before the flying packet, matching
                            // vanilla's runTick → onLivingUpdate →
                            // onUpdateWalkingPlayer ordering.
                            let abilities = app.game.take_abilities_packet();
                            if let Some(network) = &app.network {
                                if app.game.can_send_movement_packets() {
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
                        tick_accumulator -= 0.05;
                    }
                }
                if !scripted_smoke_done
                    && scripted_smoke_seconds
                        .is_some_and(|seconds| (now - app_start).as_secs_f32() >= seconds)
                {
                    scripted_smoke_done = true;
                    if let Some(profile) = smoke_profile.as_mut() {
                        profile.flush(now);
                    }
                    if let Some(bench) = pass_bench.as_ref() {
                        bench.report();
                    }
                    log::info!("scripted smoke complete");
                    target.exit();
                }

                if app.quit {
                    target.exit();
                }
                sync_cursor(window, &mut cursor_captured, &app);

                // --- Render + pacing. ---
                // The pass benchmark runs uncapped so the frame time reflects GPU
                // cost (a cap would just turn saved GPU time into sleep).
                if pass_bench.is_none() {
                    if let Some(cap) = app.settings.fps_limit() {
                        let deadline = last_frame + Duration::from_secs_f64(1.0 / cap as f64);
                        let now = Instant::now();
                        if now < deadline {
                            std::thread::sleep(deadline - now);
                        }
                    }
                }
                if let Some(bench) = pass_bench.as_mut() {
                    let (sky, water, ui, flat) = bench.config_for_frame();
                    renderer.set_pass_skip(sky, water, ui, flat);
                }
                render_frame(
                    &mut renderer,
                    &mut app,
                    window,
                    &atlas_uv,
                    &mut fps_counter,
                    &mut last_frame,
                    tick_accumulator,
                    cursor_position,
                    mouse_down_left,
                    f3_debug,
                    smoke_profile.is_some() || pass_bench.is_some(),
                );
                if let Some(profile) = smoke_profile.as_mut() {
                    profile.record(renderer.last_stats(), Instant::now());
                }
                if let Some(bench) = pass_bench.as_mut() {
                    bench.record(renderer.last_stats(), Instant::now());
                }
                // Reveal the window once the first frame has actually been drawn.
                if !window_shown {
                    window.set_visible(true);
                    window.focus_window();
                    window_shown = true;
                }

                // Keep the OS IME in sync with whichever field is focused: enable
                // it only while editing text (so gameplay keys stay raw and no
                // candidate window pops up mid-game), and pin the candidate
                // window to the caret recorded during this frame's draw.
                let focused_caret = app
                    .screen
                    .as_mut()
                    .and_then(|screen| screen.focused_text_input())
                    .map(|input| input.caret_area());
                let want_ime = focused_caret.is_some();
                if want_ime != ime_enabled {
                    window.set_ime_allowed(want_ime);
                    ime_enabled = want_ime;
                    if !want_ime {
                        last_ime_area = None;
                    }
                }
                if let Some(area) = focused_caret.flatten() {
                    if last_ime_area != Some(area) {
                        last_ime_area = Some(area);
                        let (cx, cy, cw, ch) = area;
                        window.set_ime_cursor_area(
                            PhysicalPosition::new(cx as f64, cy as f64),
                            PhysicalSize::new(cw.max(1) as f64, ch.max(1) as f64),
                        );
                    }
                }

                target.set_control_flow(ControlFlow::Poll);
            }
            _ => {}
        }
    })?;
    #[allow(unreachable_code)]
    Ok(())
}

/// Apply screen actions to the application (navigation, connects, auth, …).
fn handle_actions(app: &mut App, renderer: &mut Renderer, actions: Vec<GuiAction>) {
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
                        // Demo world: echo locally so the chat stays usable.
                        let name = app
                            .session_username()
                            .unwrap_or(&app.username)
                            .to_owned();
                        app.game.chat.push_message(format!("<{name}> {message}"));
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
            GuiAction::SendPacket(packet) => {
                if let Some(network) = &app.network {
                    network.send_packet(packet);
                }
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

/// Process a bounded number of network events per iteration so the join-time
/// chunk burst can't stall the loop; the rest drain on following iterations.
fn pump_network(app: &mut App, window: &winit::window::Window, cursor_captured: &mut bool) {
    let Some(network) = &app.network else { return };
    let mut packet_budget = 64;
    let mut disconnect: Option<String> = None;
    while packet_budget > 0 {
        match network.events.try_recv() {
            Ok(NetworkEvent::Connected { username, uuid }) => {
                log::info!("logged in as {username} ({uuid})");
            }
            Ok(NetworkEvent::PlayPacket(packet)) => {
                app.game.apply_play_packet(packet);
                packet_budget -= 1;
            }
            Ok(NetworkEvent::ChunkColumn { x, z, column }) => {
                app.game.apply_chunk_column(x, z, &column);
                packet_budget -= 1;
            }
            Ok(NetworkEvent::ChunkUnload { x, z }) => {
                app.game.unload_chunk(x, z);
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
        app.network = None;
        app.in_world = false;
        app.connecting = false;
        app.screen = Some(Box::new(GuiDisconnected::new(
            "Connection Lost",
            message,
            Parent::Multiplayer,
        )));
    } else if app.connecting && app.game.loaded_chunk_count() > 0 {
        // World data has arrived: enter gameplay.
        app.connecting = false;
        app.in_world = true;
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

/// Render one frame: world, entities + first-person hand, HUD and the open
/// screen. Driven from `AboutToWait` so the frame rate is paced by our own
/// vsync/FPS-cap logic instead of macOS Core Animation throttling.
#[allow(clippy::too_many_arguments)]
fn render_frame(
    renderer: &mut Renderer,
    app: &mut App,
    window: &winit::window::Window,
    atlas_uv: &recraft_render::AtlasUv,
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
    renderer.set_gpu_timing(f3_debug || smoke_active);

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

        let first_person = app.game.first_person_view(tick_alpha);
        app.game
            .build_entity_model(&mut app.entity_model, tick_alpha, app.skin_manager.rows());
        if hud_visible {
            ItemRenderer::render_arm(&mut app.entity_model, &app.game.camera, &first_person);
            let (vertices, indices) =
                ItemRenderer::build_held_item(&app.game.camera, &first_person, atlas_uv);
            renderer.set_first_person_item(&vertices, &indices);
            // 3D world-space player nametags (billboarded, depth-occluded).
            let nametags = app.game.player_nametags(tick_alpha);
            renderer.set_nametags(&app.game.camera, &nametags);
        } else {
            renderer.set_first_person_item(&[], &[]);
            renderer.set_nametags(&app.game.camera, &[]);
        }
        renderer.upload_model(&app.entity_model);
        let dropped = app.game.dropped_items(tick_alpha);
        let (item_vertices, item_indices) =
            ItemRenderer::build_world_items(&app.game.camera, &dropped, atlas_uv);
        renderer.set_world_items(&item_vertices, &item_indices);
    } else {
        renderer.upload_model(&recraft_render::ModelMesh::new());
        renderer.set_first_person_item(&[], &[]);
        renderer.set_world_items(&[], &[]);
        renderer.set_nametags(&app.game.camera, &[]);
    }
    // Mining crack overlay (vanilla destroy_stage_N textures over the dig target).
    renderer.set_break_overlay(app.game.breaking_overlay());

    // Build the frame's UI: HUD beneath, screen on top.
    let size = window.inner_size();
    let (width, height) = (size.width as i32, size.height as i32);
    let account_entries = app.account_entries();
    let mut ui = recraft_render::UiFrame::new();

    let App {
        screen,
        game,
        settings,
        ms_session,
        tab_open,
        ..
    } = app;
    // The overlay only makes sense in pure gameplay, never under an open screen.
    let tab_open = *tab_open && screen.is_none();
    let hud = HudState {
        health: game.health(),
        food: game.food(),
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
    };
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
        );
    }
    let wants_panorama = screen
        .as_ref()
        .is_some_and(|s| s.wants_panorama());
    let has_panorama = wants_panorama && renderer.has_panorama();

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

/// Map a number-row key (1-9) to a 0-based hotbar slot.
fn hotbar_slot_key(event: &winit::event::KeyEvent) -> Option<i32> {
    if event.state != ElementState::Pressed {
        return None;
    }
    match event.physical_key {
        PhysicalKey::Code(KeyCode::Digit1) => Some(0),
        PhysicalKey::Code(KeyCode::Digit2) => Some(1),
        PhysicalKey::Code(KeyCode::Digit3) => Some(2),
        PhysicalKey::Code(KeyCode::Digit4) => Some(3),
        PhysicalKey::Code(KeyCode::Digit5) => Some(4),
        PhysicalKey::Code(KeyCode::Digit6) => Some(5),
        PhysicalKey::Code(KeyCode::Digit7) => Some(6),
        PhysicalKey::Code(KeyCode::Digit8) => Some(7),
        PhysicalKey::Code(KeyCode::Digit9) => Some(8),
        _ => None,
    }
}

/// If this key press opens the chat box, the text to pre-fill it with:
/// T opens empty, '/' opens with a leading slash for commands.
fn chat_open_key(event: &winit::event::KeyEvent) -> Option<String> {
    if event.state != ElementState::Pressed {
        return None;
    }
    match event.physical_key {
        PhysicalKey::Code(KeyCode::KeyT) => Some(String::new()),
        PhysicalKey::Code(KeyCode::Slash) => Some("/".to_owned()),
        _ => None,
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
