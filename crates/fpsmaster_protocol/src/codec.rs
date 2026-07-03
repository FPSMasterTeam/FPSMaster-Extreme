use std::io::{Read, Write};

use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression as ZlibCompression};

use crate::io::{PacketReader, PacketWriter, ProtocolError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Disabled,
    Enabled { threshold: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketFrame {
    pub id: i32,
    pub body: Vec<u8>,
}

impl PacketFrame {
    pub fn new(id: i32, body: Vec<u8>) -> Self {
        Self { id, body }
    }

    pub fn encode_payload(&self) -> Vec<u8> {
        let mut writer = PacketWriter::new();
        writer.write_var_i32(self.id);
        writer.write_bytes(&self.body);
        writer.into_inner()
    }

    pub fn decode_payload(payload: &[u8]) -> Result<Self> {
        let mut reader = PacketReader::new(payload);
        let id = reader.read_var_i32()?;
        Ok(Self::new(id, reader.read_remaining().to_vec()))
    }
}

pub fn encode_frame(frame: &PacketFrame, compression: Compression) -> Result<Vec<u8>> {
    let payload = frame.encode_payload();
    let packet_data = match compression {
        Compression::Disabled => payload,
        Compression::Enabled { threshold } if payload.len() < threshold as usize => {
            let mut writer = PacketWriter::new();
            writer.write_var_i32(0);
            writer.write_bytes(&payload);
            writer.into_inner()
        }
        Compression::Enabled { .. } => {
            let mut encoder = ZlibEncoder::new(Vec::new(), ZlibCompression::default());
            encoder.write_all(&payload)?;
            let compressed = encoder
                .finish()
                .map_err(|err| ProtocolError::Compression(err.to_string()))?;
            let mut writer = PacketWriter::new();
            writer.write_var_i32(payload.len() as i32);
            writer.write_bytes(&compressed);
            writer.into_inner()
        }
    };

    let mut writer = PacketWriter::new();
    writer.write_var_i32(packet_data.len() as i32);
    writer.write_bytes(&packet_data);
    Ok(writer.into_inner())
}

pub fn decode_frame(packet_data: &[u8], compression: Compression) -> Result<PacketFrame> {
    match compression {
        Compression::Disabled => PacketFrame::decode_payload(packet_data),
        Compression::Enabled { .. } => {
            let mut reader = PacketReader::new(packet_data);
            let uncompressed_len = reader.read_var_i32()?;
            let remaining = reader.read_remaining();
            if uncompressed_len == 0 {
                PacketFrame::decode_payload(remaining)
            } else {
                let mut decoder = ZlibDecoder::new(remaining);
                let mut payload = Vec::with_capacity(uncompressed_len as usize);
                decoder.read_to_end(&mut payload)?;
                if payload.len() != uncompressed_len as usize {
                    return Err(ProtocolError::InvalidData("zlib length mismatch"));
                }
                PacketFrame::decode_payload(&payload)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncompressed_payload_round_trips() {
        let frame = PacketFrame::new(0x04, vec![1, 2, 3]);
        let encoded = encode_frame(&frame, Compression::Disabled).unwrap();
        let mut reader = PacketReader::new(&encoded);
        let len = reader.read_var_i32().unwrap() as usize;
        let decoded = decode_frame(reader.read_bytes(len).unwrap(), Compression::Disabled).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn compressed_payload_round_trips() {
        let frame = PacketFrame::new(0x21, vec![7; 512]);
        let encoded = encode_frame(&frame, Compression::Enabled { threshold: 32 }).unwrap();
        let mut reader = PacketReader::new(&encoded);
        let len = reader.read_var_i32().unwrap() as usize;
        let decoded = decode_frame(
            reader.read_bytes(len).unwrap(),
            Compression::Enabled { threshold: 32 },
        )
        .unwrap();
        assert_eq!(decoded, frame);
    }
}
