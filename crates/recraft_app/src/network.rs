use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use recraft_protocol::{
    io::ProtocolError,
    net::BlockingClient,
    v1_8_9::packets::{ClientboundPlayPacket, ServerboundPacket},
};

use crate::game::MovementSnapshot;

#[derive(Debug)]
pub enum NetworkEvent {
    Connected { username: String, uuid: String },
    PlayPacket(ClientboundPlayPacket),
    Disconnected(String),
}

#[derive(Debug)]
pub enum NetworkCommand {
    Move(MovementSnapshot),
}

#[derive(Debug, Default)]
struct WalkingPacketState {
    last_reported_x: f64,
    last_reported_y: f64,
    last_reported_z: f64,
    last_reported_yaw: f32,
    last_reported_pitch: f32,
    position_update_ticks: i32,
}

impl WalkingPacketState {
    fn next_packet(&mut self, movement: MovementSnapshot) -> ServerboundPacket {
        let dx = movement.x - self.last_reported_x;
        let dy = movement.y - self.last_reported_y;
        let dz = movement.z - self.last_reported_z;
        let dyaw = movement.yaw - self.last_reported_yaw;
        let dpitch = movement.pitch - self.last_reported_pitch;
        let moved = dx * dx + dy * dy + dz * dz > 9.0e-4 || self.position_update_ticks >= 20;
        let rotated = dyaw != 0.0 || dpitch != 0.0;

        let packet = if moved && rotated {
            ServerboundPacket::PlayerPositionLook {
                x: movement.x,
                y: movement.y,
                z: movement.z,
                yaw: movement.yaw,
                pitch: movement.pitch,
                on_ground: movement.on_ground,
            }
        } else if moved {
            ServerboundPacket::PlayerPosition {
                x: movement.x,
                y: movement.y,
                z: movement.z,
                on_ground: movement.on_ground,
            }
        } else if rotated {
            ServerboundPacket::PlayerLook {
                yaw: movement.yaw,
                pitch: movement.pitch,
                on_ground: movement.on_ground,
            }
        } else {
            ServerboundPacket::Player {
                on_ground: movement.on_ground,
            }
        };

        self.position_update_ticks += 1;
        if moved {
            self.last_reported_x = movement.x;
            self.last_reported_y = movement.y;
            self.last_reported_z = movement.z;
            self.position_update_ticks = 0;
        }
        if rotated {
            self.last_reported_yaw = movement.yaw;
            self.last_reported_pitch = movement.pitch;
        }

        packet
    }
}

pub struct NetworkHandle {
    pub events: Receiver<NetworkEvent>,
    commands: Sender<NetworkCommand>,
}

impl NetworkHandle {
    pub fn connect_offline_1_8_9(host: String, port: u16, username: String) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        thread::spawn(move || network_thread(host, port, username, event_tx, command_rx));
        Self {
            events: event_rx,
            commands: command_tx,
        }
    }

    pub fn send_movement(&self, movement: MovementSnapshot) {
        let _ = self.commands.send(NetworkCommand::Move(movement));
    }
}

fn network_thread(
    host: String,
    port: u16,
    username: String,
    events: Sender<NetworkEvent>,
    commands: Receiver<NetworkCommand>,
) {
    let addr = format!("{host}:{port}");
    let mut client = match BlockingClient::connect(addr.as_str()) {
        Ok(client) => client,
        Err(err) => {
            let _ = events.send(NetworkEvent::Disconnected(format!("connect failed: {err}")));
            return;
        }
    };

    let login = match client.login_offline_1_8_9(&host, port, &username) {
        Ok(login) => login,
        Err(err) => {
            let _ = events.send(NetworkEvent::Disconnected(format!("login failed: {err}")));
            return;
        }
    };
    let _ = events.send(NetworkEvent::Connected {
        username: login.username,
        uuid: login.uuid,
    });
    let _ = client.set_read_timeout(Some(Duration::from_millis(10)));
    let mut walking_packets = WalkingPacketState::default();

    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                NetworkCommand::Move(movement) => {
                    let frame = walking_packets.next_packet(movement).into_frame();
                    if let Err(err) = client.write_packet(frame) {
                        let _ =
                            events.send(NetworkEvent::Disconnected(format!("write failed: {err}")));
                        return;
                    }
                }
            }
        }

        match client.read_play_packet_1_8_9() {
            Ok(ClientboundPlayPacket::KeepAlive { id }) => {
                if let Err(err) =
                    client.write_packet(ServerboundPacket::KeepAlive { id }.into_frame())
                {
                    let _ = events.send(NetworkEvent::Disconnected(format!(
                        "keepalive write failed: {err}"
                    )));
                    return;
                }
            }
            Ok(packet) => {
                let _ = events.send(NetworkEvent::PlayPacket(packet));
            }
            Err(ProtocolError::Io(message)) if is_timeout(&message) => {}
            Err(err) => {
                let _ = events.send(NetworkEvent::Disconnected(format!("read failed: {err}")));
                return;
            }
        }
    }
}

fn is_timeout(message: &str) -> bool {
    message.contains("timed out")
        || message.contains("would block")
        || message.contains("Resource temporarily unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movement(x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> MovementSnapshot {
        MovementSnapshot {
            x,
            y,
            z,
            yaw,
            pitch,
            on_ground: true,
        }
    }

    #[test]
    fn walking_packets_match_vanilla_position_rotation_decision() {
        let mut state = WalkingPacketState::default();

        assert!(matches!(
            state.next_packet(movement(0.0, 0.0, 0.0, 0.0, 0.0)),
            ServerboundPacket::Player { on_ground: true }
        ));
        assert!(matches!(
            state.next_packet(movement(0.0, 0.0, 0.0, 15.0, 0.0)),
            ServerboundPacket::PlayerLook { yaw: 15.0, .. }
        ));
        assert!(matches!(
            state.next_packet(movement(0.04, 0.0, 0.0, 15.0, 0.0)),
            ServerboundPacket::PlayerPosition { x, .. } if (x - 0.04).abs() < f64::EPSILON
        ));
        assert!(matches!(
            state.next_packet(movement(0.08, 0.0, 0.0, 20.0, 0.0)),
            ServerboundPacket::PlayerPositionLook { x, yaw: 20.0, .. } if (x - 0.08).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn walking_packets_force_position_after_twenty_ticks() {
        let mut state = WalkingPacketState::default();
        for _ in 0..20 {
            assert!(matches!(
                state.next_packet(movement(0.0, 0.0, 0.0, 0.0, 0.0)),
                ServerboundPacket::Player { .. }
            ));
        }

        assert!(matches!(
            state.next_packet(movement(0.0, 0.0, 0.0, 0.0, 0.0)),
            ServerboundPacket::PlayerPosition { .. }
        ));
    }
}
