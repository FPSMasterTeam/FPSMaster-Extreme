mod game;
mod network;

use std::{env, time::Instant};

use anyhow::Context;
use game::GameState;
use network::{NetworkEvent, NetworkHandle};
use recraft_render::Renderer;
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

struct LaunchConfig {
    server: Option<(String, u16)>,
    username: String,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let config = LaunchConfig::from_args();

    let event_loop = EventLoop::new().context("create event loop")?;
    let window = WindowBuilder::new()
        .with_title("ReCraft - Rust Minecraft Client")
        .build(&event_loop)
        .context("create window")?;
    let window: &'static winit::window::Window = Box::leak(Box::new(window));

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
    let mut tick_accumulator = 0.0f32;

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);
        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    renderer.resize(size);
                    game.set_aspect(renderer.aspect());
                }
                WindowEvent::KeyboardInput { event, .. } => game.input.handle_key(event),
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
                        renderer.upload_world(&game.world);
                    }

                    let now = Instant::now();
                    let dt = (now - last_frame).as_secs_f32().min(0.1);
                    last_frame = now;
                    tick_accumulator += dt;
                    while tick_accumulator >= 0.05 {
                        let movement = game.tick(0.05);
                        if let Some(network) = &network {
                            network.send_movement(movement);
                        }
                        tick_accumulator -= 0.05;
                    }

                    if let Err(err) = renderer.render(&game.camera) {
                        log::error!("render error: {err}");
                    }
                }
                _ => {}
            },
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
                _ => {}
            }
        }
        Self { server, username }
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
