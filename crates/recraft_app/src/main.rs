mod auth;
mod chat;
mod game;
mod network;
mod scoreboard;
mod ui;

use std::{
    cmp::Reverse,
    env,
    net::IpAddr,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use anyhow::Context;
use auth::{AuthEvent, Session};
use game::GameState;
use network::{NetworkEvent, NetworkHandle};
use recraft_protocol::{net::PremiumSession, v1_8_9::packets::ServerboundPacket};
use recraft_render::Renderer;
use ui::{AppScreen, FpsCounter, HudState, Settings, SettingsSlider};

/// Dirty chunks snapshotted and handed to the background mesher each frame. The
/// snapshot clone is the only main-thread cost; the mesh build runs off-thread.
const MESH_SUBMITS_PER_FRAME: usize = 8;
/// Finished background meshes uploaded to the GPU each frame.
const MESH_UPLOADS_PER_FRAME: usize = 12;
use winit::{
    dpi::LogicalSize,
    event::{DeviceEvent, ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, KeyCode, PhysicalKey},
    window::{CursorGrabMode, WindowBuilder},
};

struct LaunchConfig {
    server: Option<(String, u16)>,
    username: String,
    assets: Option<PathBuf>,
    scripted_smoke_seconds: Option<f32>,
    headless_smoke_seconds: Option<f32>,
    headless_interact_seconds: Option<f32>,
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
        .build(&event_loop)
        .context("create window")?;
    let window: &'static winit::window::Window = Box::leak(Box::new(window));
    release_cursor(window);

    let mut renderer = pollster::block_on(Renderer::new(window)).context("create renderer")?;
    let mut settings = Settings::default();
    renderer.set_vsync(settings.vsync);
    let auto_connect = config.server.clone();
    let auto_demo = config.scripted_smoke_seconds.is_some() && auto_connect.is_none();
    let mut game = if auto_connect.is_some() {
        GameState::empty_for_server(renderer.aspect())
    } else {
        GameState::demo(renderer.aspect())
    };
    renderer.upload_world(&game.world);

    // ── Auth state ────────────────────────────────────────────────────────────
    // The currently logged-in session, if any.
    let mut ms_session: Option<Session> = None;
    // Channel receiving auth events from the background login thread.
    let mut auth_rx: Option<Receiver<AuthEvent>> = None;
    // Saved accounts (persisted refresh tokens) and the system clipboard.
    let mut accounts = auth::AccountStore::load();
    let mut clipboard = arboard::Clipboard::new().ok();

    let username = config.username.clone();
    let mut network = auto_connect.as_ref().map(|(host, port)| {
        log::info!("connecting to {host}:{port} as {username}");
        NetworkHandle::connect_offline_1_8_9(host.clone(), *port, username.clone())
    });
    let mut screen = if let Some((host, port)) = auto_connect {
        AppScreen::Connecting { host, port }
    } else if auto_demo {
        capture_cursor(window);
        AppScreen::InGame
    } else {
        AppScreen::MainMenu
    };
    let mut cursor_position = (0.0f64, 0.0f64);
    let mut dragging: Option<SettingsSlider> = None;
    // Whether the left mouse button is held (drives continuous block mining).
    let mut left_held = false;
    // Per-tick input intents. Frame events only RECORD intent here; the tick loop
    // turns them into packets in vanilla order (held-item, then click actions, then
    // the flying packet) using that tick's player state — exactly how vanilla's
    // runTick processes input before onUpdateWalkingPlayer sends movement. This is
    // why interactions never land in Grim's "post-flying" window.
    let mut attack_pressed = false;
    let mut attack_released = false;
    let mut use_pressed = false;
    let mut slot_select: Option<i32> = None;
    let mut slot_scroll = 0i32;

    let mut last_frame = Instant::now();
    // The physics/network simulation advances on its own wall-clock so it runs
    // at a fixed 20 Hz regardless of how often the window actually redraws (the
    // OS throttles redraws for unfocused windows, which must not slow physics).
    let mut last_sim = Instant::now();
    let app_start = Instant::now();
    let mut fps_counter = FpsCounter::new(app_start);
    let mut tick_accumulator = 0.0f32;
    let scripted_smoke_seconds = config.scripted_smoke_seconds;
    let mut scripted_smoke_done = false;

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);

        // ── Poll auth events ────────────────────────────────────────────────
        {
            let mut clear_auth = false;
            if let Some(rx) = &auth_rx {
                loop {
                    match rx.try_recv() {
                        Ok(AuthEvent::DeviceCode {
                            user_code,
                            verification_uri,
                        }) => {
                            screen = AppScreen::Authenticating {
                                user_code,
                                verification_uri,
                            };
                        }
                        Ok(AuthEvent::Status(message)) => {
                            screen = AppScreen::AuthProgress { message };
                        }
                        Ok(AuthEvent::Success(session)) => {
                            log::info!("MS login success: {} ({})", session.username, session.uuid);
                            // Persist the (rotated) refresh token for this account.
                            accounts.record_session(&session);
                            ms_session = Some(session);
                            clear_auth = true;
                            screen = AppScreen::Accounts;
                            break;
                        }
                        Ok(AuthEvent::Failed(err)) => {
                            log::warn!("MS login failed: {err}");
                            clear_auth = true;
                            screen = AppScreen::Error { message: err };
                            break;
                        }
                        Err(_) => break,
                    }
                }
            }
            if clear_auth {
                auth_rx = None;
            }
        }

        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    renderer.resize(size);
                    game.set_aspect(renderer.aspect());
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    let escape_pressed = event.state == ElementState::Pressed
                        && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape));
                    if escape_pressed {
                        match &screen {
                            AppScreen::InGame => {
                                screen = AppScreen::Paused;
                                game.input.release_all();
                                left_held = false;
                                if let (Some(packet), Some(network)) =
                                    (game.cancel_breaking(), &network)
                                {
                                    network.send_packet(packet);
                                }
                                dragging = None;
                                release_cursor(window);
                            }
                            AppScreen::Paused => {
                                screen = AppScreen::InGame;
                                capture_cursor(window);
                            }
                            AppScreen::Inventory => {
                                screen = AppScreen::InGame;
                                capture_cursor(window);
                            }
                            AppScreen::Chat { .. } => {
                                screen = AppScreen::InGame;
                                capture_cursor(window);
                            }
                            AppScreen::Settings => {
                                dragging = None;
                                screen = AppScreen::Paused;
                            }
                            AppScreen::ServerSelect { .. } => {
                                screen = AppScreen::MainMenu;
                            }
                            AppScreen::Accounts => {
                                screen = AppScreen::MainMenu;
                            }
                            AppScreen::AddAccountToken { .. } => {
                                screen = AppScreen::Accounts;
                            }
                            _ => {}
                        }
                    } else if inventory_key_pressed(&event)
                        && matches!(screen, AppScreen::InGame | AppScreen::Inventory)
                    {
                        // 'E' opens/closes the inventory — but only in-game, so it
                        // doesn't get swallowed while typing in a text field.
                        match screen {
                            AppScreen::InGame => {
                                screen = AppScreen::Inventory;
                                game.input.release_all();
                                left_held = false;
                                if let (Some(packet), Some(network)) =
                                    (game.cancel_breaking(), &network)
                                {
                                    network.send_packet(packet);
                                }
                                release_cursor(window);
                            }
                            AppScreen::Inventory => {
                                screen = AppScreen::InGame;
                                capture_cursor(window);
                            }
                            _ => {}
                        }
                    } else if let AppScreen::ServerSelect { ref mut input } = screen {
                        // Handle text input for the server address field.
                        if event.state == ElementState::Pressed {
                            match event.physical_key {
                                PhysicalKey::Code(KeyCode::Backspace) => {
                                    input.pop();
                                }
                                PhysicalKey::Code(KeyCode::Enter)
                                | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                                    let addr = input.clone();
                                    if !addr.trim().is_empty() {
                                        let (host, port) = parse_server(&addr)
                                            .unwrap_or_else(|| ("127.0.0.1".to_owned(), 25565));
                                        game = GameState::empty_for_server(renderer.aspect());
                                        renderer.upload_world(&game.world);
                                        network = Some(start_network(
                                            host.clone(),
                                            port,
                                            &ms_session,
                                            &username,
                                        ));
                                        screen = AppScreen::Connecting { host, port };
                                        release_cursor(window);
                                    }
                                }
                                _ => {
                                    // Append printable characters.
                                    if let Key::Character(s) = &event.logical_key {
                                        for c in s.chars() {
                                            if !c.is_control() {
                                                input.push(c);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if let AppScreen::AddAccountToken { ref mut input } = screen {
                        // Refresh-token entry (usually pre-filled from the clipboard).
                        if event.state == ElementState::Pressed {
                            match event.physical_key {
                                PhysicalKey::Code(KeyCode::Backspace) => {
                                    input.pop();
                                }
                                PhysicalKey::Code(KeyCode::Enter)
                                | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                                    let token = input.trim().to_owned();
                                    if !token.is_empty() {
                                        let (tx, rx) = mpsc::channel();
                                        auth_rx = Some(rx);
                                        auth::start_login_with_refresh_token(token, tx);
                                        screen = AppScreen::Accounts;
                                    }
                                }
                                _ => {
                                    if let Key::Character(s) = &event.logical_key {
                                        for c in s.chars() {
                                            if !c.is_control() {
                                                input.push(c);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if let AppScreen::Chat { ref mut input } = screen {
                        // Chat box: type, recall history, send with Enter.
                        if event.state == ElementState::Pressed {
                            match event.physical_key {
                                PhysicalKey::Code(KeyCode::Backspace) => {
                                    input.pop();
                                }
                                PhysicalKey::Code(KeyCode::Enter)
                                | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                                    let message = input.trim().to_owned();
                                    if !message.is_empty() {
                                        game.chat.record_sent(message.clone());
                                        match &network {
                                            Some(network) => network.send_packet(
                                                ServerboundPacket::ChatMessage { message },
                                            ),
                                            None => {
                                                // Demo world: echo locally so the
                                                // chat box stays usable offline.
                                                let name = ms_session
                                                    .as_deref()
                                                    .unwrap_or(&username)
                                                    .to_owned();
                                                game.chat
                                                    .push_message(format!("<{name}> {message}"));
                                            }
                                        }
                                    }
                                    screen = AppScreen::InGame;
                                    capture_cursor(window);
                                }
                                PhysicalKey::Code(KeyCode::ArrowUp) => {
                                    if let Some(previous) = game.chat.recall_previous() {
                                        *input = previous;
                                    }
                                }
                                PhysicalKey::Code(KeyCode::ArrowDown) => {
                                    if let Some(next) = game.chat.recall_next() {
                                        *input = next;
                                    }
                                }
                                // Space arrives as Key::Named(Space), not as a
                                // Character — handle it explicitly.
                                PhysicalKey::Code(KeyCode::Space) => {
                                    if input.chars().count() < chat::MAX_CHAT_INPUT {
                                        input.push(' ');
                                    }
                                }
                                _ => {
                                    if let Key::Character(s) = &event.logical_key {
                                        for c in s.chars() {
                                            if !c.is_control()
                                                && input.chars().count() < chat::MAX_CHAT_INPUT
                                            {
                                                input.push(c);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if matches!(screen, AppScreen::InGame) {
                        // Hotbar number keys (1-9) request a slot; the actual
                        // HeldItemChange is sent inside the tick (before the flying
                        // packet). Everything else is movement/look input.
                        if let Some(slot) = hotbar_slot_key(&event) {
                            slot_select = Some(slot);
                        } else if let Some(prefill) = chat_open_key(&event) {
                            // T or '/' opens the chat box; the world keeps running
                            // but movement keys release and mining stops.
                            screen = AppScreen::Chat { input: prefill };
                            game.input.release_all();
                            left_held = false;
                            if let (Some(packet), Some(network)) =
                                (game.cancel_breaking(), &network)
                            {
                                network.send_packet(packet);
                            }
                            game.chat.reset_recall();
                            release_cursor(window);
                        } else {
                            game.input.handle_key(event);
                        }
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    if matches!(screen, AppScreen::InGame) {
                        // Scroll wheel cycles the hotbar (down = next slot); the
                        // packet is sent inside the tick.
                        let steps = match delta {
                            winit::event::MouseScrollDelta::LineDelta(_, y) => -y.signum() as i32,
                            winit::event::MouseScrollDelta::PixelDelta(pos) => {
                                -(pos.y.signum() as i32)
                            }
                        };
                        slot_scroll += steps;
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor_position = (position.x, position.y);
                    if let (AppScreen::Settings, Some(slider)) = (&screen, dragging) {
                        let controls = ui::settings_controls(window.inner_size());
                        match slider {
                            SettingsSlider::Sensitivity => settings.set_sensitivity_from01(
                                ui::slider_fraction(controls.sensitivity, position.x),
                            ),
                            SettingsSlider::FpsCap => settings
                                .set_fps_from01(ui::slider_fraction(controls.fps_cap, position.x)),
                        }
                    }
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if state == ElementState::Released {
                        dragging = None;
                        if button == MouseButton::Left {
                            left_held = false;
                            // Record the release; the cancel-dig packet (if any) is
                            // sent inside the tick, before the flying packet.
                            if matches!(screen, AppScreen::InGame) {
                                attack_released = true;
                            }
                        }
                    } else {
                        match &screen.clone() {
                            AppScreen::MainMenu => {
                                let buttons = ui::menu_buttons(window.inner_size());
                                let (cx, cy) = cursor_position;
                                if buttons.login.contains(cx, cy) {
                                    // Open the account-management screen.
                                    screen = AppScreen::Accounts;
                                } else if buttons.demo.contains(cx, cy) {
                                    network = None;
                                    game = GameState::demo(renderer.aspect());
                                    renderer.upload_world(&game.world);
                                    screen = AppScreen::InGame;
                                    capture_cursor(window);
                                } else if buttons.multiplayer.contains(cx, cy) {
                                    screen = AppScreen::ServerSelect {
                                        input: String::new(),
                                    };
                                } else if buttons.quit.contains(cx, cy) {
                                    target.exit();
                                }
                            }
                            AppScreen::ServerSelect { input } => {
                                let btns = ui::server_select_buttons(window.inner_size());
                                let (cx, cy) = cursor_position;
                                if btns.join.contains(cx, cy) {
                                    let addr = input.clone();
                                    if !addr.trim().is_empty() {
                                        let (host, port) = parse_server(&addr)
                                            .unwrap_or_else(|| ("127.0.0.1".to_owned(), 25565));
                                        game = GameState::empty_for_server(renderer.aspect());
                                        renderer.upload_world(&game.world);
                                        network = Some(start_network(
                                            host.clone(),
                                            port,
                                            &ms_session,
                                            &username,
                                        ));
                                        screen = AppScreen::Connecting { host, port };
                                        release_cursor(window);
                                    }
                                } else if btns.back.contains(cx, cy) {
                                    screen = AppScreen::MainMenu;
                                }
                            }
                            AppScreen::Error { .. } => {
                                let buttons = ui::error_buttons(window.inner_size());
                                if buttons.back.contains(cursor_position.0, cursor_position.1) {
                                    network = None;
                                    game = GameState::demo(renderer.aspect());
                                    renderer.upload_world(&game.world);
                                    screen = AppScreen::MainMenu;
                                    release_cursor(window);
                                }
                            }
                            AppScreen::Paused => {
                                let buttons = ui::pause_buttons(window.inner_size());
                                if buttons
                                    .resume
                                    .contains(cursor_position.0, cursor_position.1)
                                {
                                    screen = AppScreen::InGame;
                                    capture_cursor(window);
                                } else if buttons
                                    .settings
                                    .contains(cursor_position.0, cursor_position.1)
                                {
                                    screen = AppScreen::Settings;
                                } else if buttons
                                    .quit
                                    .contains(cursor_position.0, cursor_position.1)
                                {
                                    network = None;
                                    game = GameState::demo(renderer.aspect());
                                    renderer.upload_world(&game.world);
                                    screen = AppScreen::MainMenu;
                                }
                            }
                            AppScreen::Settings => {
                                let controls = ui::settings_controls(window.inner_size());
                                let (cx, cy) = cursor_position;
                                if controls.sensitivity.contains(cx, cy) {
                                    settings.set_sensitivity_from01(ui::slider_fraction(
                                        controls.sensitivity,
                                        cx,
                                    ));
                                    dragging = Some(SettingsSlider::Sensitivity);
                                } else if controls.fps_cap.contains(cx, cy) {
                                    settings
                                        .set_fps_from01(ui::slider_fraction(controls.fps_cap, cx));
                                    dragging = Some(SettingsSlider::FpsCap);
                                } else if controls.vsync.contains(cx, cy) {
                                    settings.vsync = !settings.vsync;
                                    renderer.set_vsync(settings.vsync);
                                } else if controls.done.contains(cx, cy) {
                                    screen = AppScreen::Paused;
                                }
                            }
                            AppScreen::InGame => {
                                // Keep the pointer grabbed and run the click as
                                // an interaction. Left = attack/dig, right = use.
                                capture_cursor(window);
                                // Record the click edge; the tick turns it into the
                                // dig/use packets in vanilla order.
                                match button {
                                    MouseButton::Left => {
                                        left_held = true;
                                        attack_pressed = true;
                                    }
                                    MouseButton::Right => use_pressed = true,
                                    _ => {}
                                }
                            }
                            AppScreen::Dead => {
                                let buttons = ui::dead_buttons(window.inner_size());
                                if buttons
                                    .respawn
                                    .contains(cursor_position.0, cursor_position.1)
                                {
                                    // Ask the server to respawn us and return to
                                    // the world (the world is already loaded).
                                    game.request_respawn();
                                    screen = AppScreen::InGame;
                                    capture_cursor(window);
                                } else if buttons
                                    .title
                                    .contains(cursor_position.0, cursor_position.1)
                                {
                                    network = None;
                                    game = GameState::demo(renderer.aspect());
                                    renderer.upload_world(&game.world);
                                    screen = AppScreen::MainMenu;
                                    release_cursor(window);
                                }
                            }
                            AppScreen::Accounts => {
                                let (cx, cy) = cursor_position;
                                let btns = ui::account_buttons(
                                    window.inner_size(),
                                    accounts.accounts.len(),
                                );
                                if btns.add_microsoft.contains(cx, cy) {
                                    // Interactive device-code login.
                                    let (tx, rx) = mpsc::channel();
                                    auth_rx = Some(rx);
                                    auth::start_login(tx);
                                } else if btns.add_token.contains(cx, cy) {
                                    // Pre-fill the token field from the clipboard.
                                    let input = clipboard
                                        .as_mut()
                                        .and_then(|c| c.get_text().ok())
                                        .map(|t| t.trim().to_owned())
                                        .unwrap_or_default();
                                    screen = AppScreen::AddAccountToken { input };
                                } else if btns.copy_token.contains(cx, cy) {
                                    // Copy the latest (active) refresh token.
                                    let token = ms_session
                                        .as_ref()
                                        .and_then(|s| s.refresh_token.clone())
                                        .or_else(|| {
                                            accounts
                                                .accounts
                                                .first()
                                                .map(|a| a.refresh_token.clone())
                                        });
                                    if let (Some(token), Some(cb)) = (token, clipboard.as_mut()) {
                                        if cb.set_text(token).is_ok() {
                                            log::info!("copied refresh token to clipboard");
                                        }
                                    }
                                } else if btns.back.contains(cx, cy) {
                                    screen = AppScreen::MainMenu;
                                } else {
                                    // Per-account USE / DEL buttons.
                                    for (i, row) in btns.rows.iter().enumerate() {
                                        if row.use_btn.contains(cx, cy) {
                                            if let Some(acc) = accounts.accounts.get(i) {
                                                let (tx, rx) = mpsc::channel();
                                                auth_rx = Some(rx);
                                                auth::start_login_with_refresh_token(
                                                    acc.refresh_token.clone(),
                                                    tx,
                                                );
                                            }
                                            break;
                                        }
                                        if row.remove_btn.contains(cx, cy) {
                                            if let Some(acc) = accounts.accounts.get(i) {
                                                let uuid = acc.uuid.clone();
                                                accounts.remove(&uuid);
                                                accounts.save();
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                            AppScreen::AddAccountToken { input } => {
                                let (cx, cy) = cursor_position;
                                let btns = ui::add_token_buttons(window.inner_size());
                                if btns.add.contains(cx, cy) {
                                    let token = input.trim().to_owned();
                                    if !token.is_empty() {
                                        let (tx, rx) = mpsc::channel();
                                        auth_rx = Some(rx);
                                        auth::start_login_with_refresh_token(token, tx);
                                        screen = AppScreen::Accounts;
                                    }
                                } else if btns.back.contains(cx, cy) {
                                    screen = AppScreen::Accounts;
                                }
                            }
                            // The inventory is display-only; clicks don't move items.
                            AppScreen::Inventory => {}
                            // Clicks don't interact while the chat box is open.
                            AppScreen::Chat { .. } => {}
                            AppScreen::Connecting { .. }
                            | AppScreen::LoadingWorld { .. }
                            | AppScreen::Authenticating { .. }
                            | AppScreen::AuthProgress { .. } => {}
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    // Rendering is driven from AboutToWait (see `render_frame`) so
                    // the frame rate isn't pinned to the display refresh by macOS
                    // Core Animation throttling RedrawRequested.
                }
                _ => {}
            },
            Event::DeviceEvent {
                event: DeviceEvent::MouseMotion { delta },
                ..
            } if matches!(screen, AppScreen::InGame) => {
                game.rotate_view(delta.0 as f32, delta.1 as f32, settings.mouse_factor())
            }
            Event::AboutToWait => {
                // --- Simulation step: network + physics at a fixed 20 Hz. ---
                if let Some(network) = &network {
                    // Process a bounded number of packets per iteration so the
                    // chunk-data burst on join can't stall the loop for seconds;
                    // the rest drain on following iterations.
                    let mut packet_budget = 64;
                    while packet_budget > 0 {
                        match network.events.try_recv() {
                            Ok(NetworkEvent::Connected { username, uuid }) => {
                                log::info!("logged in as {username} ({uuid})");
                                if let AppScreen::Connecting { host, port } = &screen {
                                    screen = AppScreen::LoadingWorld {
                                        host: host.clone(),
                                        port: *port,
                                    };
                                }
                            }
                            Ok(NetworkEvent::PlayPacket(packet)) => {
                                game.apply_play_packet(packet);
                                packet_budget -= 1;
                                if matches!(screen, AppScreen::LoadingWorld { .. })
                                    && game.loaded_chunk_count() > 0
                                {
                                    screen = AppScreen::InGame;
                                    capture_cursor(window);
                                }
                            }
                            Ok(NetworkEvent::ChunkColumn { x, z, column }) => {
                                game.apply_chunk_column(x, z, &column);
                                packet_budget -= 1;
                                if matches!(screen, AppScreen::LoadingWorld { .. })
                                    && game.loaded_chunk_count() > 0
                                {
                                    screen = AppScreen::InGame;
                                    capture_cursor(window);
                                }
                            }
                            Ok(NetworkEvent::ChunkUnload { x, z }) => {
                                game.unload_chunk(x, z);
                            }
                            Ok(NetworkEvent::Disconnected(message)) => {
                                log::warn!("network disconnected: {message}");
                                screen = AppScreen::Error { message };
                                release_cursor(window);
                            }
                            Err(_) => break,
                        }
                    }
                }

                // Scripted runs auto-respawn so the smoke driver keeps moving;
                // interactive play shows the death screen and waits for a click.
                if scripted_smoke_seconds.is_some() && game.is_dead() {
                    game.request_respawn();
                }

                // Enter the death screen when the server reports we died, and
                // leave it once we're alive again (clicked respawn or revived).
                if game.is_dead() && matches!(screen, AppScreen::InGame | AppScreen::Chat { .. }) {
                    screen = AppScreen::Dead;
                    game.input.release_all();
                    left_held = false;
                    if let (Some(packet), Some(network)) = (game.cancel_breaking(), &network) {
                        network.send_packet(packet);
                    }
                    release_cursor(window);
                } else if !game.is_dead() && matches!(screen, AppScreen::Dead) {
                    screen = AppScreen::InGame;
                    capture_cursor(window);
                }

                // If we asked to respawn, tell the server — a dead player is
                // frozen server-side and cannot move until it respawns.
                if game.take_respawn_request() {
                    if let Some(network) = &network {
                        network.send_packet(ServerboundPacket::ClientStatus { action: 0 });
                    }
                }

                // Teleport acks are now sent synchronously by the network thread
                // (in packet order); just drain the game-side pending flag.
                let _ = game.take_position_confirm();

                let now = Instant::now();
                let sim_dt = (now - last_sim).as_secs_f32().min(0.25);
                last_sim = now;
                if let Some(seconds) = scripted_smoke_seconds {
                    game.apply_scripted_smoke_input((now - app_start).as_secs_f32(), seconds);
                }
                // The world keeps ticking (and reporting movement) while the
                // chat box is open, exactly like vanilla.
                if matches!(screen, AppScreen::InGame | AppScreen::Chat { .. }) {
                    tick_accumulator += sim_dt;
                    while tick_accumulator >= 0.05 {
                        // `None` on the teleport-ack tick: hold, send no movement.
                        if let Some(movement) = game.tick(0.05) {
                            // Build this tick's gameplay packets in vanilla order
                            // (held-item, then click actions) using the tick's
                            // player state, then the flying packet last — mirroring
                            // runTick → onUpdateWalkingPlayer. The edges are consumed
                            // once; while held, mining continues via on_attack_hold.
                            let actions = collect_tick_actions(
                                &mut game,
                                slot_select.take(),
                                &mut slot_scroll,
                                &mut attack_pressed,
                                &mut attack_released,
                                &mut use_pressed,
                                left_held,
                            );
                            if let Some(network) = &network {
                                if game.can_send_movement_packets() {
                                    for packet in actions {
                                        network.send_packet(packet);
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
                    log::info!("scripted smoke complete");
                    target.exit();
                }

                // --- Render + pacing. ---
                // Render here (not from RedrawRequested) so the frame rate is
                // paced by our cap / present mode, not pinned to the display
                // refresh. Enforce a finite cap with an explicit sleep and keep
                // ControlFlow::Poll, rather than ControlFlow::WaitUntil: on macOS
                // the run-loop timer behind WaitUntil gets coalesced with the
                // display vblank, which intermittently re-pins FPS to the refresh
                // rate (the "sometimes capped at 60" symptom).
                let session_username = ms_session.as_deref();
                // Only the accounts screen needs the account list; build it lazily.
                let account_entries: Vec<ui::AccountEntry> =
                    if matches!(screen, AppScreen::Accounts) {
                        accounts
                            .accounts
                            .iter()
                            .map(|a| ui::AccountEntry {
                                username: a.username.clone(),
                                active: ms_session.as_ref().is_some_and(|s| s.uuid == a.uuid),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                if let Some(cap) = settings.fps_limit() {
                    let deadline = last_frame + Duration::from_secs_f64(1.0 / cap as f64);
                    let now = Instant::now();
                    if now < deadline {
                        std::thread::sleep(deadline - now);
                    }
                }
                render_frame(
                    &mut renderer,
                    &mut game,
                    window,
                    &screen,
                    &settings,
                    &mut fps_counter,
                    &mut last_frame,
                    tick_accumulator,
                    session_username,
                    &account_entries,
                );
                target.set_control_flow(ControlFlow::Poll);
            }
            _ => {}
        }
    })?;
    #[allow(unreachable_code)]
    Ok(())
}

/// Render one frame. Driven from `AboutToWait` rather than `RedrawRequested` so
/// the frame rate is paced by our own vsync/FPS-cap logic instead of being
/// pinned to the display refresh by macOS Core Animation (which throttles
/// `RedrawRequested`). This is what lets vsync-off / a raised cap exceed the
/// display's refresh rate.
#[allow(clippy::too_many_arguments)]
fn render_frame(
    renderer: &mut Renderer,
    game: &mut GameState,
    window: &winit::window::Window,
    screen: &AppScreen,
    settings: &Settings,
    fps_counter: &mut FpsCounter,
    last_frame: &mut Instant,
    tick_accumulator: f32,
    session_username: Option<&str>,
    accounts: &[ui::AccountEntry],
) {
    let now = Instant::now();
    fps_counter.tick(now);
    let frame_dt = (now - *last_frame).as_secs_f32().min(0.1);
    *last_frame = now;

    // Hand a bounded number of dirty chunks to the background mesher, then
    // upload whatever finished — both off the frame's critical path.
    let dirty_chunks = game.take_dirty_chunks_budget(MESH_SUBMITS_PER_FRAME);
    if !dirty_chunks.is_empty() {
        renderer.queue_chunk_meshes(&game.world, dirty_chunks);
    }
    renderer.process_ready_meshes(&game.world, MESH_UPLOADS_PER_FRAME);

    // Entities stay visible behind menu/death overlays; only the in-game state
    // animates the camera and draws the hand.
    let in_world = matches!(
        screen,
        AppScreen::InGame
            | AppScreen::Chat { .. }
            | AppScreen::Paused
            | AppScreen::Settings
            | AppScreen::Inventory
            | AppScreen::Dead
    );
    // The chat screen keeps simulating, so keep interpolating the camera too.
    if matches!(screen, AppScreen::InGame | AppScreen::Chat { .. }) {
        game.update_camera(tick_accumulator / 0.05);
        game.advance_animations(frame_dt);
    }
    if in_world {
        let show_hand = matches!(screen, AppScreen::InGame | AppScreen::Chat { .. });
        renderer.upload_model(&game.build_entity_model(show_hand));
    } else {
        renderer.upload_model(&recraft_render::ModelMesh::new());
    }
    // Mining crack overlay (vanilla destroy_stage_N textures over the dig target).
    renderer.set_break_overlay(game.breaking_overlay());

    let hud = HudState {
        health: game.health(),
        food: game.food(),
        xp_bar: game.xp_bar(),
        xp_level: game.xp_level(),
        selected_slot: game.selected_slot(),
        hotbar: game.hotbar_items(),
        inventory: game.inventory_slots(),
        chat: &game.chat,
        scoreboard: &game.scoreboard,
    };
    let ui_frame = ui::build_ui(
        screen,
        window.inner_size(),
        fps_counter.fps(),
        game.loaded_chunk_count(),
        settings,
        hud,
        session_username,
        accounts,
    );

    if let Err(err) = renderer.render_with_ui(&game.camera, &ui_frame) {
        log::error!("render error: {err}");
    }
}

/// Start a network connection, choosing premium or offline mode based on the
/// available session.  The `ms_session` type is `Option<Session>` from `auth`.
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
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--connect" => {
                    if let Some(value) = args.next() {
                        server = parse_server(&value);
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
        }
    }
}

/// Run the network + physics simulation in a plain fixed-rate loop with no
/// window or renderer. macOS app-naps unfocused windows (throttling both
/// redraws *and* the event loop), so a windowed smoke test can't exercise
/// movement at a real 20 Hz; this can. Connects, joins, walks via the scripted
/// driver, and reports server corrections — the ground truth for join/movement.
/// Build a tick's serverbound gameplay packets in vanilla order — held-item
/// change first (which, like `PlayerControllerMP.resetBlockRemoving`, cancels any
/// in-progress dig when the item changes), then the click actions. The caller
/// sends these before the tick's flying packet, matching runTick →
/// onUpdateWalkingPlayer. Input edges are consumed here.
fn collect_tick_actions(
    game: &mut GameState,
    slot_select: Option<i32>,
    slot_scroll: &mut i32,
    attack_pressed: &mut bool,
    attack_released: &mut bool,
    use_pressed: &mut bool,
    left_held: bool,
) -> Vec<ServerboundPacket> {
    let mut packets = Vec::new();

    let slot_packet = if let Some(slot) = slot_select {
        game.set_selected_slot(slot)
    } else if *slot_scroll != 0 {
        let p = game.cycle_slot(*slot_scroll);
        *slot_scroll = 0;
        p
    } else {
        None
    };
    if let Some(slot_packet) = slot_packet {
        if let Some(cancel) = game.cancel_breaking() {
            packets.push(cancel);
        }
        packets.push(slot_packet);
    }

    if *attack_pressed {
        packets.extend(game.on_attack_press());
    } else if left_held {
        packets.extend(game.on_attack_hold());
    }
    if *attack_released {
        packets.extend(game.on_attack_release());
    }
    if *use_pressed {
        packets.extend(game.on_use());
    }
    *attack_pressed = false;
    *attack_released = false;
    *use_pressed = false;

    packets
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
            // exercise HeldItemChange ordering and the dig-reset-on-switch.
            if let Some(movement) = game.tick(0.05) {
                if game.can_send_movement_packets() {
                    let want_mine = game.debug_has_block_target();
                    let mut attack_pressed = want_mine && !started_attack;
                    let mut attack_released = false;
                    let mut use_pressed = false;
                    if want_mine {
                        started_attack = true;
                    }
                    let left_held = want_mine;
                    // Switch hotbar slot periodically — mid-dig on slow (stone)
                    // blocks this aborts the dig, exercising the cancel→start
                    // sequence Grim's PositionBreakB validates.
                    let slot_select = if tick_count % 30 == 15 {
                        Some((tick_count / 30 % 9) as i32)
                    } else {
                        None
                    };
                    let mut slot_scroll = 0;
                    let actions = collect_tick_actions(
                        &mut game,
                        slot_select,
                        &mut slot_scroll,
                        &mut attack_pressed,
                        &mut attack_released,
                        &mut use_pressed,
                        left_held,
                    );
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
        // chunk data on join can't stall the simulation: we keep ticking and
        // sending movement/teleport-confirms promptly instead of blocking for
        // seconds while the whole chunk flood decodes.
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

        // Teleport acks are sent synchronously by the network thread now.
        let _ = game.take_position_confirm();
        game.apply_scripted_smoke_input(elapsed, seconds);
        if in_game {
            // `None` on the teleport-ack tick: hold, send no movement.
            if let Some(movement) = game.tick(0.05) {
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

fn parse_server(value: &str) -> Option<(String, u16)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if let Some((host, port)) = value.rsplit_once(':') {
        return Some((host.to_owned(), port.parse().unwrap_or(25565)));
    }

    resolve_minecraft_srv(value).or_else(|| Some((value.to_owned(), 25565)))
}

fn resolve_minecraft_srv(host: &str) -> Option<(String, u16)> {
    if host.parse::<IpAddr>().is_ok() {
        return None;
    }

    let resolver = hickory_resolver::Resolver::from_system_conf().ok()?;
    let lookup = resolver
        .srv_lookup(format!("_minecraft._tcp.{host}.").as_str())
        .ok()?;
    let record = lookup
        .iter()
        .min_by_key(|record| (record.priority(), Reverse(record.weight())))?;
    let target = record.target().to_utf8();
    let target = target.trim_end_matches('.');
    if target.is_empty() {
        return None;
    }

    log::info!(
        "resolved Minecraft SRV {host} -> {target}:{}",
        record.port()
    );
    Some((target.to_owned(), record.port()))
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

/// Whether this key event is a press of the inventory key ('E').
fn inventory_key_pressed(event: &winit::event::KeyEvent) -> bool {
    event.state == ElementState::Pressed
        && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyE))
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

// ─── `Option<Session>` deref helper so we can pass `Option<&str>` for the
//     username display without a full `use` import everywhere.
trait OptionSessionExt {
    fn as_deref(&self) -> Option<&str>;
}

impl OptionSessionExt for Option<Session> {
    fn as_deref(&self) -> Option<&str> {
        self.as_ref().map(|s| s.username.as_str())
    }
}
