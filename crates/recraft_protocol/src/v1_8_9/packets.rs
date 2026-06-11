use crate::{codec::PacketFrame, io::{PacketReader, PacketWriter, ProtocolError, Result}};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextState {
    Status = 1,
    Login = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerboundPacket {
    Handshake {
        protocol_version: i32,
        host: String,
        port: u16,
        next_state: NextState,
    },
    LoginStart {
        username: String,
    },
    KeepAlive {
        id: i32,
    },
    PlayerPosition {
        x: f64,
        y: f64,
        z: f64,
        on_ground: bool,
    },
    PlayerLook {
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },
    PlayerPositionLook {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientboundLoginPacket {
    Disconnect { reason_json: String },
    EncryptionRequest {
        server_id: String,
        public_key: Vec<u8>,
        verify_token: Vec<u8>,
    },
    LoginSuccess { uuid: String, username: String },
    SetCompression { threshold: i32 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientboundPlayPacket {
    KeepAlive {
        id: i32,
    },
    JoinGame {
        entity_id: i32,
        game_mode: u8,
        dimension: i8,
        difficulty: u8,
        max_players: u8,
        level_type: String,
        reduced_debug_info: bool,
    },
    PlayerPositionLook {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        flags: i8,
    },
    ChunkData {
        x: i32,
        z: i32,
        ground_up: bool,
        primary_bit_mask: u16,
        data: Vec<u8>,
    },
    Disconnect { reason_json: String },
    Unknown { id: i32, body: Vec<u8> },
}

impl ServerboundPacket {
    pub fn into_frame(self) -> PacketFrame {
        match self {
            Self::Handshake { protocol_version, host, port, next_state } => {
                let mut body = PacketWriter::new();
                body.write_var_i32(protocol_version);
                body.write_string(&host);
                body.write_u16(port);
                body.write_var_i32(next_state as i32);
                PacketFrame::new(0x00, body.into_inner())
            }
            Self::LoginStart { username } => {
                let mut body = PacketWriter::new();
                body.write_string(&username);
                PacketFrame::new(0x00, body.into_inner())
            }
            Self::KeepAlive { id } => {
                let mut body = PacketWriter::new();
                body.write_var_i32(id);
                PacketFrame::new(0x00, body.into_inner())
            }
            Self::PlayerPosition { x, y, z, on_ground } => {
                let mut body = PacketWriter::new();
                body.write_f64(x);
                body.write_f64(y);
                body.write_f64(z);
                body.write_bool(on_ground);
                PacketFrame::new(0x04, body.into_inner())
            }
            Self::PlayerLook { yaw, pitch, on_ground } => {
                let mut body = PacketWriter::new();
                body.write_f32(yaw);
                body.write_f32(pitch);
                body.write_bool(on_ground);
                PacketFrame::new(0x05, body.into_inner())
            }
            Self::PlayerPositionLook { x, y, z, yaw, pitch, on_ground } => {
                let mut body = PacketWriter::new();
                body.write_f64(x);
                body.write_f64(y);
                body.write_f64(z);
                body.write_f32(yaw);
                body.write_f32(pitch);
                body.write_bool(on_ground);
                PacketFrame::new(0x06, body.into_inner())
            }
        }
    }
}

impl ClientboundLoginPacket {
    pub fn from_frame(frame: PacketFrame) -> Result<Self> {
        let mut body = PacketReader::new(&frame.body);
        match frame.id {
            0x00 => Ok(Self::Disconnect { reason_json: body.read_string(32767)? }),
            0x01 => {
                let server_id = body.read_string(20)?;
                let public_key_len = body.read_var_i32()? as usize;
                let public_key = body.read_bytes(public_key_len)?.to_vec();
                let verify_token_len = body.read_var_i32()? as usize;
                let verify_token = body.read_bytes(verify_token_len)?.to_vec();
                Ok(Self::EncryptionRequest { server_id, public_key, verify_token })
            }
            0x02 => Ok(Self::LoginSuccess {
                uuid: body.read_string(36)?,
                username: body.read_string(16)?,
            }),
            0x03 => Ok(Self::SetCompression { threshold: body.read_var_i32()? }),
            id => Err(ProtocolError::InvalidPacketId(id, "login/clientbound")),
        }
    }
}

impl ClientboundPlayPacket {
    pub fn from_frame(frame: PacketFrame) -> Result<Self> {
        let mut body = PacketReader::new(&frame.body);
        match frame.id {
            0x00 => Ok(Self::KeepAlive { id: body.read_var_i32()? }),
            0x01 => Ok(Self::JoinGame {
                entity_id: body.read_i32()?,
                game_mode: body.read_u8()?,
                dimension: body.read_i8()?,
                difficulty: body.read_u8()?,
                max_players: body.read_u8()?,
                level_type: body.read_string(16)?,
                reduced_debug_info: body.read_bool()?,
            }),
            0x08 => Ok(Self::PlayerPositionLook {
                x: body.read_f64()?,
                y: body.read_f64()?,
                z: body.read_f64()?,
                yaw: body.read_f32()?,
                pitch: body.read_f32()?,
                flags: body.read_i8()?,
            }),
            0x21 => Ok(Self::ChunkData {
                x: body.read_i32()?,
                z: body.read_i32()?,
                ground_up: body.read_bool()?,
                primary_bit_mask: body.read_u16()?,
                data: {
                    let len = body.read_var_i32()? as usize;
                    body.read_bytes(len)?.to_vec()
                },
            }),
            0x40 => Ok(Self::Disconnect { reason_json: body.read_string(32767)? }),
            id => Ok(Self::Unknown { id, body: frame.body }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1_8_9::PROTOCOL_VERSION;

    #[test]
    fn handshake_packet_matches_protocol_47_layout() {
        let frame = ServerboundPacket::Handshake {
            protocol_version: PROTOCOL_VERSION,
            host: "localhost".to_owned(),
            port: 25565,
            next_state: NextState::Login,
        }
        .into_frame();

        assert_eq!(frame.id, 0x00);
        let mut reader = PacketReader::new(&frame.body);
        assert_eq!(reader.read_var_i32().unwrap(), 47);
        assert_eq!(reader.read_string(255).unwrap(), "localhost");
        assert_eq!(reader.read_u16().unwrap(), 25565);
        assert_eq!(reader.read_var_i32().unwrap(), 2);
        assert!(reader.is_empty());
    }

    #[test]
    fn clientbound_join_game_decodes() {
        let mut body = PacketWriter::new();
        body.write_i32(123);
        body.write_u8(0);
        body.write_i8(0);
        body.write_u8(2);
        body.write_u8(20);
        body.write_string("default");
        body.write_bool(false);

        let packet = ClientboundPlayPacket::from_frame(PacketFrame::new(0x01, body.into_inner())).unwrap();
        assert_eq!(packet, ClientboundPlayPacket::JoinGame {
            entity_id: 123,
            game_mode: 0,
            dimension: 0,
            difficulty: 2,
            max_players: 20,
            level_type: "default".to_owned(),
            reduced_debug_info: false,
        });
    }
}
