use std::time::Instant;

use anyhow::Context;
use glam::Vec3;
use recraft_core::{BlockState, World};
use recraft_render::{Camera, Renderer};
use winit::{
    event::{ElementState, Event, KeyEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::WindowBuilder,
};

#[derive(Default)]
struct InputState {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
}

impl InputState {
    fn apply(&self, camera: &mut Camera, dt: f32) {
        let speed = 12.0 * dt;
        let forward = camera.direction();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        if self.forward {
            camera.position += forward * speed;
        }
        if self.backward {
            camera.position -= forward * speed;
        }
        if self.right {
            camera.position += right * speed;
        }
        if self.left {
            camera.position -= right * speed;
        }
        if self.up {
            camera.position += Vec3::Y * speed;
        }
        if self.down {
            camera.position -= Vec3::Y * speed;
        }
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let event_loop = EventLoop::new().context("create event loop")?;
    let window = WindowBuilder::new()
        .with_title("ReCraft - Rust Minecraft Client")
        .build(&event_loop)
        .context("create window")?;
    let window: &'static winit::window::Window = Box::leak(Box::new(window));

    let mut renderer = pollster::block_on(Renderer::new(window)).context("create renderer")?;
    let world = demo_world();
    renderer.upload_world(&world);
    let mut camera = Camera::new(Vec3::new(8.0, 14.0, 28.0), renderer.aspect());
    let mut input = InputState::default();
    let mut last_frame = Instant::now();

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);
        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    renderer.resize(size);
                    camera.aspect = renderer.aspect();
                }
                WindowEvent::KeyboardInput { event, .. } => handle_key(&mut input, event),
                WindowEvent::RedrawRequested => {
                    let now = Instant::now();
                    let dt = (now - last_frame).as_secs_f32();
                    last_frame = now;
                    input.apply(&mut camera, dt);
                    if let Err(err) = renderer.render(&camera) {
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

fn handle_key(input: &mut InputState, event: KeyEvent) {
    let pressed = event.state == ElementState::Pressed;
    match event.physical_key {
        PhysicalKey::Code(KeyCode::KeyW) => input.forward = pressed,
        PhysicalKey::Code(KeyCode::KeyS) => input.backward = pressed,
        PhysicalKey::Code(KeyCode::KeyA) => input.left = pressed,
        PhysicalKey::Code(KeyCode::KeyD) => input.right = pressed,
        PhysicalKey::Code(KeyCode::Space) => input.up = pressed,
        PhysicalKey::Code(KeyCode::ShiftLeft) => input.down = pressed,
        _ => {}
    }
}

fn demo_world() -> World {
    let mut world = World::new();
    for x in -16..32 {
        for z in -16..32 {
            world.set_block(x, 0, z, BlockState::GRASS);
            for y in -3..0 {
                world.set_block(x, y, z, BlockState::DIRT);
            }
        }
    }
    for x in 4..10 {
        for y in 1..5 {
            for z in 4..10 {
                if x == 4 || x == 9 || z == 4 || z == 9 || y == 4 {
                    world.set_block(x, y, z, BlockState::STONE);
                }
            }
        }
    }
    for y in 1..7 {
        world.set_block(-4, y, -4, BlockState::new(17, 0));
    }
    for x in -7..0 {
        for y in 5..9 {
            for z in -7..0 {
                let dx = x + 4;
                let dy = y - 7;
                let dz = z + 4;
                if dx * dx + dy * dy + dz * dz < 12 {
                    world.set_block(x, y, z, BlockState::new(18, 0));
                }
            }
        }
    }
    world
}
