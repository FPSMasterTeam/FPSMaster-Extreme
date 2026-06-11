use crate::io::{PacketReader, ProtocolError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedBlock {
    pub x: u8,
    pub y: u8,
    pub z: u8,
    pub id: u16,
    pub meta: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSection {
    pub y: u8,
    pub blocks: Vec<DecodedBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedChunkData {
    pub sections: Vec<DecodedSection>,
    pub biomes: Option<Vec<u8>>,
}

pub fn decode_chunk_data(data: &[u8], primary_bit_mask: u16, ground_up: bool, has_sky_light: bool) -> Result<DecodedChunkData> {
    let mut reader = PacketReader::new(data);
    let section_count = primary_bit_mask.count_ones() as usize;
    let mut sections = Vec::with_capacity(section_count);

    for section_y in 0..16u8 {
        if primary_bit_mask & (1 << section_y) == 0 {
            continue;
        }

        let mut blocks = Vec::with_capacity(4096);
        for y in 0..16u8 {
            for z in 0..16u8 {
                for x in 0..16u8 {
                    let raw = reader.read_u16()?;
                    blocks.push(DecodedBlock {
                        x,
                        y,
                        z,
                        id: raw >> 4,
                        meta: (raw & 0x0f) as u8,
                    });
                }
            }
        }
        sections.push(DecodedSection { y: section_y, blocks });
    }

    for _ in 0..section_count {
        reader.read_bytes(2048)?; // block light nibble array
    }

    if has_sky_light {
        for _ in 0..section_count {
            reader.read_bytes(2048)?;
        }
    }

    let biomes = if ground_up {
        Some(reader.read_bytes(256)?.to_vec())
    } else {
        None
    };

    if !reader.is_empty() {
        return Err(ProtocolError::InvalidData("chunk packet has trailing bytes"));
    }

    Ok(DecodedChunkData { sections, biomes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_single_section_stone_chunk() {
        let mut data = Vec::new();
        for _ in 0..4096 {
            data.extend_from_slice(&(1u16 << 4).to_be_bytes());
        }
        data.extend(std::iter::repeat(0xff).take(2048));
        data.extend(std::iter::repeat(0x00).take(2048));
        data.extend(std::iter::repeat(1).take(256));

        let decoded = decode_chunk_data(&data, 0x0001, true, true).unwrap();
        assert_eq!(decoded.sections.len(), 1);
        assert_eq!(decoded.sections[0].blocks[0].id, 1);
        assert_eq!(decoded.sections[0].blocks[0].meta, 0);
        assert_eq!(decoded.biomes.as_ref().unwrap().len(), 256);
    }
}
