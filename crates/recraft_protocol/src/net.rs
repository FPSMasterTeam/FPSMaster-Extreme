use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::{
    codec::{decode_frame, encode_frame, Compression, PacketFrame},
    io::{PacketReader, ProtocolError, Result},
    v1_8_9::{packets::{ClientboundLoginPacket, ClientboundPlayPacket, NextState, ServerboundPacket}, PROTOCOL_VERSION},
};

#[derive(Debug)]
pub struct OfflineLoginResult {
    pub uuid: String,
    pub username: String,
    pub compression: Compression,
}

pub struct BlockingClient {
    stream: TcpStream,
    compression: Compression,
}

impl BlockingClient {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        Ok(Self { stream, compression: Compression::Disabled })
    }

    pub fn login_offline_1_8_9(&mut self, host: &str, port: u16, username: &str) -> Result<OfflineLoginResult> {
        self.write_packet(ServerboundPacket::Handshake {
            protocol_version: PROTOCOL_VERSION,
            host: host.to_owned(),
            port,
            next_state: NextState::Login,
        }.into_frame())?;
        self.write_packet(ServerboundPacket::LoginStart { username: username.to_owned() }.into_frame())?;

        loop {
            let frame = self.read_frame()?;
            match ClientboundLoginPacket::from_frame(frame)? {
                ClientboundLoginPacket::SetCompression { threshold } => {
                    self.compression = Compression::Enabled { threshold };
                }
                ClientboundLoginPacket::LoginSuccess { uuid, username } => {
                    return Ok(OfflineLoginResult { uuid, username, compression: self.compression });
                }
                ClientboundLoginPacket::Disconnect { reason_json } => {
                    return Err(ProtocolError::Compression(reason_json));
                }
                ClientboundLoginPacket::EncryptionRequest { .. } => {
                    return Err(ProtocolError::InvalidData("online-mode encryption is not implemented yet"));
                }
            }
        }
    }

    pub fn read_play_packet_1_8_9(&mut self) -> Result<ClientboundPlayPacket> {
        ClientboundPlayPacket::from_frame(self.read_frame()?)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        self.stream.set_read_timeout(timeout)?;
        Ok(())
    }

    pub fn write_packet(&mut self, frame: PacketFrame) -> Result<()> {
        let encoded = encode_frame(&frame, self.compression)?;
        self.stream.write_all(&encoded)?;
        Ok(())
    }

    fn read_frame(&mut self) -> Result<PacketFrame> {
        let length = read_var_i32_from_stream(&mut self.stream)?;
        if length < 0 {
            return Err(ProtocolError::InvalidData("negative packet length"));
        }
        let mut data = vec![0; length as usize];
        self.stream.read_exact(&mut data)?;
        decode_frame(&data, self.compression)
    }
}

fn read_var_i32_from_stream(stream: &mut TcpStream) -> Result<i32> {
    let mut data = [0u8; 5];
    for index in 0..5 {
        stream.read_exact(&mut data[index..index + 1])?;
        if data[index] & 0x80 == 0 {
            return PacketReader::new(&data[..=index]).read_var_i32();
        }
    }
    Err(ProtocolError::VarIntTooLong)
}
