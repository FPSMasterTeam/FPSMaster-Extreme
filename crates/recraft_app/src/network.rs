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

    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                NetworkCommand::Move(movement) => {
                    let frame = ServerboundPacket::PlayerPositionLook {
                        x: movement.x,
                        y: movement.y,
                        z: movement.z,
                        yaw: movement.yaw,
                        pitch: movement.pitch,
                        on_ground: movement.on_ground,
                    }
                    .into_frame();
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
