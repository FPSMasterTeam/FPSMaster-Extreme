use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("VarInt is too long")]
    VarIntTooLong,
    #[error("string length {0} exceeds maximum {1}")]
    StringTooLong(usize, usize),
    #[error("invalid UTF-8 string")]
    InvalidUtf8,
    #[error("invalid packet id 0x{0:02x} in {1}")]
    InvalidPacketId(i32, &'static str),
    #[error("invalid data: {0}")]
    InvalidData(&'static str),
    #[error("compression error: {0}")]
    Compression(String),
    #[error("io error: {0}")]
    Io(String),
}

impl From<std::io::Error> for ProtocolError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ProtocolError>;

#[derive(Debug, Clone, Copy)]
pub struct PacketReader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> PacketReader<'a> {
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    pub const fn remaining(&self) -> usize {
        self.data.len() - self.offset
    }

    pub const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        self.take(1).map(|bytes| bytes[0])
    }

    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_u8()? as i8)
    }

    pub fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read_u8()? != 0)
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    /// Little-endian u16. The 1.8 chunk block-state array is stored low-byte
    /// first, unlike the otherwise big-endian protocol.
    pub fn read_u16_le(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_i16(&mut self) -> Result<i16> {
        let bytes = self.take(2)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        let bytes = self.take(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_i64(&mut self) -> Result<i64> {
        let bytes = self.take(8)?;
        Ok(i64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.read_i32()? as u32))
    }

    pub fn read_f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.read_i64()? as u64))
    }

    pub fn read_var_i32(&mut self) -> Result<i32> {
        let mut num_read = 0;
        let mut result = 0i32;
        loop {
            let byte = self.read_u8()?;
            let value = (byte & 0x7f) as i32;
            result |= value << (7 * num_read);
            num_read += 1;
            if num_read > 5 {
                return Err(ProtocolError::VarIntTooLong);
            }
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
    }

    pub fn read_string(&mut self, max_chars: usize) -> Result<String> {
        let byte_len = self.read_var_i32()? as usize;
        if byte_len > max_chars * 4 {
            return Err(ProtocolError::StringTooLong(byte_len, max_chars * 4));
        }
        let bytes = self.take(byte_len)?;
        let value = std::str::from_utf8(bytes).map_err(|_| ProtocolError::InvalidUtf8)?;
        if value.chars().count() > max_chars {
            return Err(ProtocolError::StringTooLong(
                value.chars().count(),
                max_chars,
            ));
        }
        Ok(value.to_owned())
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        self.take(len)
    }

    pub fn read_remaining(&mut self) -> &'a [u8] {
        let bytes = &self.data[self.offset..];
        self.offset = self.data.len();
        bytes
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.remaining() < len {
            return Err(ProtocolError::UnexpectedEof);
        }
        let start = self.offset;
        self.offset += len;
        Ok(&self.data[start..self.offset])
    }
}

#[derive(Debug, Default, Clone)]
pub struct PacketWriter {
    data: Vec<u8>,
}

impl PacketWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.data
    }

    pub fn write_u8(&mut self, value: u8) {
        self.data.push(value);
    }

    pub fn write_i8(&mut self, value: i8) {
        self.write_u8(value as u8);
    }

    pub fn write_bool(&mut self, value: bool) {
        self.write_u8(u8::from(value));
    }

    pub fn write_u16(&mut self, value: u16) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_i16(&mut self, value: i16) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_i64(&mut self, value: i64) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_f32(&mut self, value: f32) {
        self.data.extend_from_slice(&value.to_bits().to_be_bytes());
    }

    pub fn write_f64(&mut self, value: f64) {
        self.data.extend_from_slice(&value.to_bits().to_be_bytes());
    }

    pub fn write_var_i32(&mut self, mut value: i32) {
        loop {
            if (value & !0x7f) == 0 {
                self.write_u8(value as u8);
                return;
            }
            self.write_u8(((value & 0x7f) | 0x80) as u8);
            value = ((value as u32) >> 7) as i32;
        }
    }

    pub fn write_string(&mut self, value: &str) {
        self.write_var_i32(value.len() as i32);
        self.data.extend_from_slice(value.as_bytes());
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
    }
}

pub fn encode_var_i32(value: i32) -> Vec<u8> {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(value);
    writer.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn var_int_round_trip_known_values() {
        let values = [
            0,
            1,
            2,
            127,
            128,
            255,
            2_097_151,
            2_147_483_647,
            -1,
            -2_147_483_648,
        ];
        for value in values {
            let encoded = encode_var_i32(value);
            let mut reader = PacketReader::new(&encoded);
            assert_eq!(reader.read_var_i32().unwrap(), value);
            assert!(reader.is_empty());
        }
    }

    #[test]
    fn var_int_matches_minecraft_examples() {
        assert_eq!(encode_var_i32(0), &[0x00]);
        assert_eq!(encode_var_i32(127), &[0x7f]);
        assert_eq!(encode_var_i32(128), &[0x80, 0x01]);
        assert_eq!(encode_var_i32(255), &[0xff, 0x01]);
        assert_eq!(
            encode_var_i32(2_147_483_647),
            &[0xff, 0xff, 0xff, 0xff, 0x07]
        );
        assert_eq!(encode_var_i32(-1), &[0xff, 0xff, 0xff, 0xff, 0x0f]);
    }
}
