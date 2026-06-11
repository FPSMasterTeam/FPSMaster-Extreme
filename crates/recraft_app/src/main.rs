mod game;
mod network;

use std::{env, path::PathBuf, time::Instant};

use anyhow::Context;
use game::GameState;
use network::{NetworkEvent, NetworkHandle};
use recraft_render::Renderer;
use winit::{
    event::{DeviceEvent, ElementState, Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorGrabMode, WindowBuilder},
};

struct LaunchConfig {
    server: Option<(String, u16)>,
    username: String,
    assets: Option<PathBuf>,
    scripted_smoke_seconds: Option<f32>,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let config = LaunchConfig::from_args();
    if let Some(path) = &config.assets {
        env::set_var("RECRAFT_ASSET_PATH", path);
    }

    let event_loop = EventLoop::new().context("create event loop")?;
    let window = WindowBuilder::new()
        .with_title("ReCraft - Rust Minecraft Client")
        .build(&event_loop)
        .context("create window")?;
    let window: &'static winit::window::Window = Box::leak(Box::new(window));
    capture_cursor(window);

    let mut renderer = pollster::block_on(Renderer::new(window)).context("create renderer")?;
    let mut game = if config.server.is_some() {
        GameState::empty_for_server(renderer.aspect())
    } else {
        GameState::demo(renderer.aspect())
    };
    renderer.upload_world(&game.world);

    let network = config.server.map(|(host, port)| {
        log::info!("connecting to {host}:{port} as {}", config.username);
        NetworkHandle::connect_offline_1_8_9(host, port, config.username)
    });

    let mut last_frame = Instant::now();
    let app_start = Instant::now();
    let mut tick_accumulator = 0.0f32;
    let scripted_smoke_seconds = config.scripted_smoke_seconds;
    let mut scripted_smoke_done = false;

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);
        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    renderer.resize(size);
                    game.set_aspect(renderer.aspect());
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if event.state == ElementState::Pressed
                        && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
                    {
                        release_cursor(window);
                    } else {
                        game.input.handle_key(event);
                    }
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } => capture_cursor(window),
                WindowEvent::RedrawRequested => {
                    let mut mesh_dirty = false;
                    if let Some(network) = &network {
                        while let Ok(event) = network.events.try_recv() {
                            match event {
                                NetworkEvent::Connected { username, uuid } => {
                                    log::info!("logged in as {username} ({uuid})")
                                }
                                NetworkEvent::PlayPacket(packet) => {
                                    mesh_dirty |= game.apply_play_packet(packet)
                                }
                                NetworkEvent::Disconnected(message) => {
                                    log::warn!("network disconnected: {message}")
                                }
                            }
                        }
                    }
                    if mesh_dirty {
                        let dirty_chunks = game.take_dirty_chunks();
                        renderer.upload_dirty_chunks(&game.world, dirty_chunks);
                    }

                    let now = Instant::now();
                    let dt = (now - last_frame).as_secs_f32().min(0.1);
                    last_frame = now;
                    if let Some(seconds) = scripted_smoke_seconds {
                        game.apply_scripted_smoke_input((now - app_start).as_secs_f32(), seconds);
                    }
                    tick_accumulator += dt;
                    while tick_accumulator >= 0.05 {
                        let movement = game.tick(0.05);
                        if let Some(network) = &network {
                            network.send_movement(movement);
                        }
                        tick_accumulator -= 0.05;
                    }
                    game.update_camera(tick_accumulator / 0.05);

                    if let Err(err) = renderer.render(&game.camera) {
                        log::error!("render error: {err}");
                    }
                    if !scripted_smoke_done
                        && scripted_smoke_seconds
                            .is_some_and(|seconds| (now - app_start).as_secs_f32() >= seconds)
                    {
                        scripted_smoke_done = true;
                        log::info!("scripted smoke complete");
                        target.exit();
                    }
                }
                _ => {}
            },
            Event::DeviceEvent {
                event: DeviceEvent::MouseMotion { delta },
                ..
            } => game.rotate_view(delta.0 as f32, delta.1 as f32),
            Event::AboutToWait => window.request_redraw(),
            _ => {}
        }
    })?;
    #[allow(unreachable_code)]
    Ok(())
}

impl LaunchConfig {
    fn from_args() -> Self {
        let mut server = None;
        let mut username = "ReCraft".to_owned();
        let mut assets = None;
        let mut scripted_smoke_seconds = None;
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
                _ => {}
            }
        }
        Self {
            server,
            username,
            assets,
            scripted_smoke_seconds,
        }
    }
}

fn parse_server(value: &str) -> Option<(String, u16)> {
    let (host, port) = value
        .rsplit_once(':')
        .map_or((value, 25565), |(host, port)| {
            (host, port.parse().unwrap_or(25565))
        });
    Some((host.to_owned(), port))
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
