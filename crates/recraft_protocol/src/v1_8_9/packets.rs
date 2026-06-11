use crate::{
    codec::PacketFrame,
    io::{PacketReader, PacketWriter, ProtocolError, Result},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextState {
    Status = 1,
    Login = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityAction {
    StartSneaking = 0,
    StopSneaking = 1,
    StopSleeping = 2,
    StartSprinting = 3,
    StopSprinting = 4,
    RidingJump = 5,
    OpenInventory = 6,
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
    Player {
        on_ground: bool,
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
    EntityAction {
        entity_id: i32,
        action: EntityAction,
        aux_data: i32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientboundLoginPacket {
    Disconnect {
        reason_json: String,
    },
    EncryptionRequest {
        server_id: String,
        public_key: Vec<u8>,
        verify_token: Vec<u8>,
    },
    LoginSuccess {
        uuid: String,
        username: String,
    },
    SetCompression {
        threshold: i32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct BulkChunkData {
    pub x: i32,
    pub z: i32,
    pub primary_bit_mask: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockChangeRecord {
    pub x: u8,
    pub y: u8,
    pub z: u8,
    pub id: u16,
    pub meta: u8,
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
    MultiBlockChange {
        chunk_x: i32,
        chunk_z: i32,
        changes: Vec<BlockChangeRecord>,
    },
    BlockChange {
        x: i32,
        y: i32,
        z: i32,
        id: u16,
        meta: u8,
    },
    ChunkBulk {
        sky_light_sent: bool,
        chunks: Vec<BulkChunkData>,
    },
    Disconnect {
        reason_json: String,
    },
    Unknown {
        id: i32,
        body: Vec<u8>,
    },
}

impl ServerboundPacket {
    pub fn into_frame(self) -> PacketFrame {
        match self {
            Self::Handshake {
                protocol_version,
                host,
                port,
                next_state,
            } => {
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
            Self::Player { on_ground } => {
                let mut body = PacketWriter::new();
                body.write_bool(on_ground);
                PacketFrame::new(0x03, body.into_inner())
            }
            Self::PlayerPosition { x, y, z, on_ground } => {
                let mut body = PacketWriter::new();
                body.write_f64(x);
                body.write_f64(y);
                body.write_f64(z);
                body.write_bool(on_ground);
                PacketFrame::new(0x04, body.into_inner())
            }
            Self::PlayerLook {
                yaw,
                pitch,
                on_ground,
            } => {
                let mut body = PacketWriter::new();
                body.write_f32(yaw);
                body.write_f32(pitch);
                body.write_bool(on_ground);
                PacketFrame::new(0x05, body.into_inner())
            }
            Self::PlayerPositionLook {
                x,
                y,
                z,
                yaw,
                pitch,
                on_ground,
            } => {
                let mut body = PacketWriter::new();
                body.write_f64(x);
                body.write_f64(y);
                body.write_f64(z);
                body.write_f32(yaw);
                body.write_f32(pitch);
                body.write_bool(on_ground);
                PacketFrame::new(0x06, body.into_inner())
            }
            Self::EntityAction {
                entity_id,
                action,
                aux_data,
            } => {
                let mut body = PacketWriter::new();
                body.write_var_i32(entity_id);
                body.write_var_i32(action as i32);
                body.write_var_i32(aux_data);
                PacketFrame::new(0x0b, body.into_inner())
            }
        }
    }
}

impl ClientboundLoginPacket {
    pub fn from_frame(frame: PacketFrame) -> Result<Self> {
        let mut body = PacketReader::new(&frame.body);
        match frame.id {
            0x00 => Ok(Self::Disconnect {
                reason_json: body.read_string(32767)?,
            }),
            0x01 => {
                let server_id = body.read_string(20)?;
                let public_key_len = body.read_var_i32()? as usize;
                let public_key = body.read_bytes(public_key_len)?.to_vec();
                let verify_token_len = body.read_var_i32()? as usize;
                let verify_token = body.read_bytes(verify_token_len)?.to_vec();
                Ok(Self::EncryptionRequest {
                    server_id,
                    public_key,
                    verify_token,
                })
            }
            0x02 => Ok(Self::LoginSuccess {
                uuid: body.read_string(36)?,
                username: body.read_string(16)?,
            }),
            0x03 => Ok(Self::SetCompression {
                threshold: body.read_var_i32()?,
            }),
            id => Err(ProtocolError::InvalidPacketId(id, "login/clientbound")),
        }
    }
}

impl ClientboundPlayPacket {
    pub fn from_frame(frame: PacketFrame) -> Result<Self> {
        let mut body = PacketReader::new(&frame.body);
        match frame.id {
            0x00 => Ok(Self::KeepAlive {
                id: body.read_var_i32()?,
            }),
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
            0x22 => {
                let chunk_x = body.read_i32()?;
                let chunk_z = body.read_i32()?;
                let count = body.read_var_i32()? as usize;
                let mut changes = Vec::with_capacity(count);
                for _ in 0..count {
                    let packed = body.read_u16()?;
                    let (id, meta) = decode_legacy_block_state_id(body.read_var_i32()?)?;
                    changes.push(BlockChangeRecord {
                        x: ((packed >> 12) & 15) as u8,
                        y: (packed & 255) as u8,
                        z: ((packed >> 8) & 15) as u8,
                        id,
                        meta,
                    });
                }
                Ok(Self::MultiBlockChange {
                    chunk_x,
                    chunk_z,
                    changes,
                })
            }
            0x23 => {
                let (x, y, z) = read_block_pos(&mut body)?;
                let (id, meta) = decode_legacy_block_state_id(body.read_var_i32()?)?;
                Ok(Self::BlockChange { x, y, z, id, meta })
            }
            0x26 => {
                let sky_light_sent = body.read_bool()?;
                let count = body.read_var_i32()? as usize;
                let mut meta = Vec::with_capacity(count);
                for _ in 0..count {
                    meta.push((body.read_i32()?, body.read_i32()?, body.read_u16()?));
                }
                let mut chunks = Vec::with_capacity(count);
                for (x, z, primary_bit_mask) in meta {
                    let len = bulk_chunk_data_len(primary_bit_mask, sky_light_sent);
                    chunks.push(BulkChunkData {
                        x,
                        z,
                        primary_bit_mask,
                        data: body.read_bytes(len)?.to_vec(),
                    });
                }
                Ok(Self::ChunkBulk {
                    sky_light_sent,
                    chunks,
                })
            }
            0x40 => Ok(Self::Disconnect {
                reason_json: body.read_string(32767)?,
            }),
            id => Ok(Self::Unknown {
                id,
                body: frame.body,
            }),
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

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x01, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::JoinGame {
                entity_id: 123,
                game_mode: 0,
                dimension: 0,
                difficulty: 2,
                max_players: 20,
                level_type: "default".to_owned(),
                reduced_debug_info: false,
            }
        );
    }

    #[test]
    fn clientbound_block_change_decodes_position_and_legacy_state() {
        let mut body = PacketWriter::new();
        body.write_bytes(&encoded_block_pos(-1, 64, 2));
        body.write_var_i32((1 << 4) | 0);

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x23, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::BlockChange {
                x: -1,
                y: 64,
                z: 2,
                id: 1,
                meta: 0,
            }
        );
    }

    #[test]
    fn clientbound_multi_block_change_decodes_crammed_positions() {
        let mut body = PacketWriter::new();
        body.write_i32(-2);
        body.write_i32(3);
        body.write_var_i32(2);
        body.write_u16((1 << 12) | (2 << 8) | 64);
        body.write_var_i32((2 << 4) | 0);
        body.write_u16((15 << 12) | 255);
        body.write_var_i32((5 << 4) | 3);

        let packet =
            ClientboundPlayPacket::from_frame(PacketFrame::new(0x22, body.into_inner())).unwrap();
        assert_eq!(
            packet,
            ClientboundPlayPacket::MultiBlockChange {
                chunk_x: -2,
                chunk_z: 3,
                changes: vec![
                    BlockChangeRecord {
                        x: 1,
                        y: 64,
                        z: 2,
                        id: 2,
                        meta: 0,
                    },
                    BlockChangeRecord {
                        x: 15,
                        y: 255,
                        z: 0,
                        id: 5,
                        meta: 3,
                    },
                ],
            }
        );
    }

    #[test]
    fn entity_action_packet_writes_vanilla_enum_ordinal() {
        let frame = ServerboundPacket::EntityAction {
            entity_id: 42,
            action: EntityAction::StartSprinting,
            aux_data: 0,
        }
        .into_frame();

        assert_eq!(frame.id, 0x0b);
        let mut reader = PacketReader::new(&frame.body);
        assert_eq!(reader.read_var_i32().unwrap(), 42);
        assert_eq!(reader.read_var_i32().unwrap(), 3);
        assert_eq!(reader.read_var_i32().unwrap(), 0);
        assert!(reader.is_empty());
    }

    #[test]
    fn player_packet_writes_on_ground_only() {
        let frame = ServerboundPacket::Player { on_ground: true }.into_frame();

        assert_eq!(frame.id, 0x03);
        assert_eq!(frame.body, vec![1]);
    }

    fn encoded_block_pos(x: i32, y: i32, z: i32) -> [u8; 8] {
        let value = ((x as u64) & 0x03ff_ffff) << 38
            | ((y as u64) & 0x0fff) << 26
            | ((z as u64) & 0x03ff_ffff);
        value.to_be_bytes()
    }
}

fn bulk_chunk_data_len(primary_bit_mask: u16, sky_light_sent: bool) -> usize {
    let sections = primary_bit_mask.count_ones() as usize;
    let mut len = sections * (4096 * 2 + 2048);
    if sky_light_sent {
        len += sections * 2048;
    }
    len + 256
}

fn read_block_pos(reader: &mut PacketReader<'_>) -> Result<(i32, i32, i32)> {
    let value = reader.read_i64()? as u64;
    let x = sign_extend((value >> 38) & 0x03ff_ffff, 26);
    let y = sign_extend((value >> 26) & 0x0fff, 12);
    let z = sign_extend(value & 0x03ff_ffff, 26);
    Ok((x, y, z))
}

fn sign_extend(value: u64, bits: u32) -> i32 {
    let shift = 64 - bits;
    ((value << shift) as i64 >> shift) as i32
}

fn decode_legacy_block_state_id(state_id: i32) -> Result<(u16, u8)> {
    if !(0..=0xffff).contains(&state_id) {
        return Err(ProtocolError::InvalidData(
            "legacy block state id out of range",
        ));
    }
    let value = state_id as u16;
    Ok((value >> 4, (value & 15) as u8))
}
