use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use recraft_protocol::{
    io::ProtocolError,
    net::{BlockingClient, PremiumSession},
    v1_8_9::{
        chunk::{decode_chunk_column, ChunkColumnData},
        packets::{ClientboundPlayPacket, EntityAction, ServerboundPacket},
    },
};

use crate::game::MovementSnapshot;

#[derive(Debug)]
pub enum NetworkEvent {
    Connected {
        username: String,
        uuid: String,
    },
    PlayPacket(ClientboundPlayPacket),
    /// A chunk decoded on the network thread, ready to load with no main-thread
    /// decode cost (keeps the render loop unblocked during the join burst).
    ChunkColumn {
        x: i32,
        z: i32,
        column: ChunkColumnData,
    },
    /// The server unloaded a chunk (empty ground-up column).
    ChunkUnload {
        x: i32,
        z: i32,
    },
    Disconnected(String),
}

#[derive(Debug)]
pub enum NetworkCommand {
    Move(MovementSnapshot),
    Send(ServerboundPacket),
}

#[derive(Debug, Default)]
struct WalkingPacketState {
    last_reported_x: f64,
    last_reported_y: f64,
    last_reported_z: f64,
    last_reported_yaw: f32,
    last_reported_pitch: f32,
    position_update_ticks: i32,
    server_sneak_state: bool,
    server_sprint_state: bool,
}

impl WalkingPacketState {
    fn next_packets(&mut self, movement: MovementSnapshot) -> Vec<ServerboundPacket> {
        let mut packets = Vec::new();
        if movement.sprinting != self.server_sprint_state {
            packets.push(ServerboundPacket::EntityAction {
                entity_id: movement.entity_id,
                action: if movement.sprinting {
                    EntityAction::StartSprinting
                } else {
                    EntityAction::StopSprinting
                },
                aux_data: 0,
            });
            self.server_sprint_state = movement.sprinting;
        }
        if movement.sneaking != self.server_sneak_state {
            packets.push(ServerboundPacket::EntityAction {
                entity_id: movement.entity_id,
                action: if movement.sneaking {
                    EntityAction::StartSneaking
                } else {
                    EntityAction::StopSneaking
                },
                aux_data: 0,
            });
            self.server_sneak_state = movement.sneaking;
        }
        packets.push(self.next_packet(movement));
        packets
    }

    /// Build the packet that confirms a server teleport/correction. The 1.8
    /// server (PlayerConnection) clears `checkMovement` ONLY when it receives a
    /// packet that carries a position within 0.5 of the teleport target; a
    /// look-only or flags-only packet leaves the player frozen and the server
    /// resends the correction every tick. So always emit a full position+look,
    /// regardless of how small the delta is, and reset our reporting baseline so
    /// the following movement delta is measured from the confirmed position.
    fn confirm_packet(&mut self, movement: MovementSnapshot) -> ServerboundPacket {
        self.last_reported_x = movement.x;
        self.last_reported_y = movement.y;
        self.last_reported_z = movement.z;
        self.last_reported_yaw = movement.yaw;
        self.last_reported_pitch = movement.pitch;
        self.position_update_ticks = 0;
        ServerboundPacket::PlayerPositionLook {
            x: movement.x,
            y: movement.y,
            z: movement.z,
            yaw: movement.yaw,
            pitch: movement.pitch,
            on_ground: movement.on_ground,
        }
    }

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
        thread::spawn(move || network_thread(host, port, username, None, event_tx, command_rx));
        Self {
            events: event_rx,
            commands: command_tx,
        }
    }

    /// Connect using a premium (Microsoft) session for online-mode servers.
    pub fn connect_premium_1_8_9(host: String, port: u16, session: PremiumSession) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let username = session.username.clone();
        thread::spawn(move || {
            network_thread(host, port, username, Some(session), event_tx, command_rx)
        });
        Self {
            events: event_rx,
            commands: command_tx,
        }
    }

    pub fn send_movement(&self, movement: MovementSnapshot) {
        let _ = self.commands.send(NetworkCommand::Move(movement));
    }

    /// Queue an arbitrary serverbound packet (interaction, held-item change…).
    pub fn send_packet(&self, packet: ServerboundPacket) {
        let _ = self.commands.send(NetworkCommand::Send(packet));
    }
}

fn network_thread(
    host: String,
    port: u16,
    username: String,
    session: Option<PremiumSession>,
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

    let login = if let Some(sess) = session {
        match client.login_premium_1_8_9(&host, port, &sess) {
            Ok(login) => login,
            Err(err) => {
                let _ = events.send(NetworkEvent::Disconnected(format!(
                    "premium login failed: {err}"
                )));
                return;
            }
        }
    } else {
        match client.login_offline_1_8_9(&host, port, &username) {
            Ok(login) => login,
            Err(err) => {
                let _ = events.send(NetworkEvent::Disconnected(format!("login failed: {err}")));
                return;
            }
        }
    };
    let _ = events.send(NetworkEvent::Connected {
        username: login.username,
        uuid: login.uuid,
    });
    let _ = client.set_read_timeout(Some(Duration::from_millis(10)));

    let mut walking_packets = WalkingPacketState::default();
    // The brand / client-settings handshake must be sent only once the server
    // is in the play state — i.e. after it sends JoinGame. Sending earlier (e.g.
    // through a proxy still in login) yields "Bad packet id".
    let mut sent_join_handshake = false;
    // Sky light presence is dimension-derived (overworld only); tracked here so
    // chunks can be decoded on this thread without the game state.
    let mut has_sky_light = true;

    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                NetworkCommand::Move(movement) => {
                    for packet in walking_packets.next_packets(movement) {
                        log::debug!("sending {}", packet_debug_name(&packet));
                        let frame = packet.into_frame();
                        if let Err(err) = client.write_packet(frame) {
                            let _ = events
                                .send(NetworkEvent::Disconnected(format!("write failed: {err}")));
                            return;
                        }
                    }
                }
                NetworkCommand::Send(packet) => {
                    log::debug!("sending {}", packet_debug_name(&packet));
                    if let Err(err) = client.write_packet(packet.into_frame()) {
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
            Ok(ClientboundPlayPacket::ConfirmTransaction {
                window_id,
                action_number,
                accepted,
            }) => {
                // Vanilla echoes an unaccepted server transaction straight back
                // on the network thread. Anti-cheats (e.g. Grim) inject these as
                // a timing/ack ping and gate their movement setbacks on the
                // reply, so a faithful 1.8 client must pong them promptly.
                if !accepted {
                    if let Err(err) = client.write_packet(
                        ServerboundPacket::ConfirmTransaction {
                            window_id,
                            action_number,
                            accepted: true,
                        }
                        .into_frame(),
                    ) {
                        let _ = events.send(NetworkEvent::Disconnected(format!(
                            "transaction write failed: {err}"
                        )));
                        return;
                    }
                }
            }
            Ok(packet) => {
                // Track dimension-derived sky light and send the vanilla join
                // handshake (brand + client settings) once we reach the play
                // state (JoinGame).
                match &packet {
                    ClientboundPlayPacket::JoinGame { dimension, .. } => {
                        has_sky_light = *dimension == 0;
                        if !sent_join_handshake {
                            sent_join_handshake = true;
                            for handshake in initial_play_packets() {
                                if let Err(err) = client.write_packet(handshake.into_frame()) {
                                    let _ = events.send(NetworkEvent::Disconnected(format!(
                                        "handshake write failed: {err}"
                                    )));
                                    return;
                                }
                            }
                        }
                    }
                    ClientboundPlayPacket::Respawn { dimension, .. } => {
                        has_sky_light = *dimension == 0;
                    }
                    ClientboundPlayPacket::PlayerPositionLook {
                        x,
                        y,
                        z,
                        yaw,
                        pitch,
                        flags,
                    } => {
                        // Ack the teleport synchronously here, in packet order —
                        // BEFORE ponging any later transaction. Vanilla does this on
                        // the netty thread; deferring it to the game tick lets a
                        // newer transaction pong go out first, so a strict anti-cheat
                        // (Grim) sees the player "skip past" the setback and
                        // setback-loops forever. Relative axes resolve against the
                        // last reported position the network thread already tracks.
                        let rx = if flags & 0x01 != 0 {
                            walking_packets.last_reported_x + x
                        } else {
                            *x
                        };
                        let ry = if flags & 0x02 != 0 {
                            walking_packets.last_reported_y + y
                        } else {
                            *y
                        };
                        let rz = if flags & 0x04 != 0 {
                            walking_packets.last_reported_z + z
                        } else {
                            *z
                        };
                        let ryaw = if flags & 0x08 != 0 {
                            walking_packets.last_reported_yaw + yaw
                        } else {
                            *yaw
                        };
                        let rpitch = if flags & 0x10 != 0 {
                            walking_packets.last_reported_pitch + pitch
                        } else {
                            *pitch
                        };
                        let confirm = walking_packets.confirm_packet(MovementSnapshot {
                            x: rx,
                            y: ry,
                            z: rz,
                            yaw: ryaw,
                            pitch: rpitch,
                            // Vanilla teleport-ack reports on_ground=false; with the
                            // transaction now in order Grim recognises the ack and
                            // exempts it from the ground check.
                            on_ground: false,
                            entity_id: 0,
                            sneaking: false,
                            sprinting: false,
                        });
                        if let Err(err) = client.write_packet(confirm.into_frame()) {
                            let _ = events.send(NetworkEvent::Disconnected(format!(
                                "teleport confirm write failed: {err}"
                            )));
                            return;
                        }
                    }
                    _ => {}
                }

                // Intercept chunk packets and decode them here, off the render
                // thread, delivering ready-to-load columns; forward everything
                // else (in receipt order, so block-change ordering is preserved).
                match packet {
                    ClientboundPlayPacket::ChunkData {
                        x,
                        z,
                        ground_up,
                        primary_bit_mask,
                        data,
                    } => {
                        if ground_up && primary_bit_mask == 0 {
                            let _ = events.send(NetworkEvent::ChunkUnload { x, z });
                        } else {
                            match decode_chunk_column(
                                &data,
                                primary_bit_mask,
                                ground_up,
                                has_sky_light,
                            ) {
                                Ok(column) => {
                                    let _ = events.send(NetworkEvent::ChunkColumn { x, z, column });
                                }
                                Err(err) => log::warn!("decode chunk {x},{z} failed: {err}"),
                            }
                        }
                    }
                    ClientboundPlayPacket::ChunkBulk {
                        sky_light_sent,
                        chunks,
                    } => {
                        for chunk in chunks {
                            match decode_chunk_column(
                                &chunk.data,
                                chunk.primary_bit_mask,
                                true,
                                sky_light_sent,
                            ) {
                                Ok(column) => {
                                    let _ = events.send(NetworkEvent::ChunkColumn {
                                        x: chunk.x,
                                        z: chunk.z,
                                        column,
                                    });
                                }
                                Err(err) => {
                                    log::warn!(
                                        "decode bulk chunk {},{} failed: {err}",
                                        chunk.x,
                                        chunk.z
                                    )
                                }
                            }
                        }
                    }
                    other => {
                        let _ = events.send(NetworkEvent::PlayPacket(other));
                    }
                }
            }
            Err(ProtocolError::Io(message)) if is_timeout(&message) => {}
            Err(err) => {
                let _ = events.send(NetworkEvent::Disconnected(format!("read failed: {err}")));
                return;
            }
        }
    }
}

/// The packets a vanilla 1.8 client sends right after entering the play state:
/// the client brand on `MC|Brand`, then client settings.
fn initial_play_packets() -> Vec<ServerboundPacket> {
    let mut brand = recraft_protocol::io::PacketWriter::new();
    brand.write_string("recraft");
    vec![
        ServerboundPacket::PluginMessage {
            channel: "MC|Brand".to_owned(),
            data: brand.into_inner(),
        },
        ServerboundPacket::ClientSettings {
            locale: "en_US".to_owned(),
            view_distance: 8,
            chat_mode: 0,
            chat_colors: true,
            skin_parts: 0x7f,
        },
    ]
}

fn is_timeout(message: &str) -> bool {
    message.contains("timed out")
        || message.contains("would block")
        || message.contains("Resource temporarily unavailable")
}

fn packet_debug_name(packet: &ServerboundPacket) -> &'static str {
    match packet {
        ServerboundPacket::Player { .. } => "C03 Player",
        ServerboundPacket::PlayerPosition { .. } => "C04 PlayerPosition",
        ServerboundPacket::PlayerLook { .. } => "C05 PlayerLook",
        ServerboundPacket::PlayerPositionLook { .. } => "C06 PlayerPositionLook",
        ServerboundPacket::EntityAction { action, .. } => match action {
            EntityAction::StartSneaking => "C0B EntityAction START_SNEAKING",
            EntityAction::StopSneaking => "C0B EntityAction STOP_SNEAKING",
            EntityAction::StopSleeping => "C0B EntityAction STOP_SLEEPING",
            EntityAction::StartSprinting => "C0B EntityAction START_SPRINTING",
            EntityAction::StopSprinting => "C0B EntityAction STOP_SPRINTING",
            EntityAction::RidingJump => "C0B EntityAction RIDING_JUMP",
            EntityAction::OpenInventory => "C0B EntityAction OPEN_INVENTORY",
        },
        ServerboundPacket::UseEntity { .. } => "C02 UseEntity",
        ServerboundPacket::PlayerDigging { .. } => "C07 PlayerDigging",
        ServerboundPacket::PlayerBlockPlacement { .. } => "C08 PlayerBlockPlacement",
        ServerboundPacket::HeldItemChange { .. } => "C09 HeldItemChange",
        ServerboundPacket::SwingArm => "C0A Animation",
        ServerboundPacket::ClientStatus { .. } => "C16 ClientStatus",
        ServerboundPacket::ChatMessage { .. } => "C01 ChatMessage",
        ServerboundPacket::PlayerAbilities { .. } => "C13 PlayerAbilities",
        ServerboundPacket::ConfirmTransaction { .. } => "C0F ConfirmTransaction",
        ServerboundPacket::ClientSettings { .. } => "C15 ClientSettings",
        ServerboundPacket::PluginMessage { .. } => "C17 PluginMessage",
        ServerboundPacket::Handshake { .. } => "Handshake",
        ServerboundPacket::LoginStart { .. } => "LoginStart",
        ServerboundPacket::KeepAlive { .. } => "KeepAlive",
    }
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
            entity_id: 123,
            sneaking: false,
            sprinting: false,
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
    fn walking_packets_send_sprint_then_sneak_actions_before_movement() {
        let mut state = WalkingPacketState::default();
        let mut movement = movement(0.0, 0.0, 0.0, 0.0, 0.0);
        movement.sprinting = true;
        movement.sneaking = true;

        let packets = state.next_packets(movement);
        assert!(matches!(
            packets.as_slice(),
            [
                ServerboundPacket::EntityAction {
                    entity_id: 123,
                    action: EntityAction::StartSprinting,
                    aux_data: 0,
                },
                ServerboundPacket::EntityAction {
                    entity_id: 123,
                    action: EntityAction::StartSneaking,
                    aux_data: 0,
                },
                ServerboundPacket::Player { .. },
            ]
        ));

        let packets = state.next_packets(movement);
        assert!(matches!(
            packets.as_slice(),
            [ServerboundPacket::Player { .. }]
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
