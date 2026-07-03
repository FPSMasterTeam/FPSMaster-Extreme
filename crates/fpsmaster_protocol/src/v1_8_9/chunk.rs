use crate::io::{PacketReader, ProtocolError, Result};

/// One 16x16x16 chunk section in compact wire-shaped form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSectionData {
    /// Section index 0..16 (the bit position in the primary bit mask).
    pub y: u8,
    /// 4096 raw block states in y*256 + z*16 + x order; value = (block_id << 4) | meta.
    pub blocks: Vec<u16>,
    /// 2048 packed nibbles (block light), wire order. Always len 2048.
    pub block_light: Vec<u8>,
    /// 2048 packed nibbles (sky light); all zeros when has_sky_light is false.
    pub sky_light: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkColumnData {
    pub sections: Vec<ChunkSectionData>,
    pub biomes: Option<Vec<u8>>,
}

/// Extract a 4-bit value from a packed nibble array (low nibble first).
pub fn nibble_at(bytes: &[u8], index: usize) -> u8 {
    let byte = bytes[index / 2];
    if index % 2 == 0 {
        byte & 0x0f
    } else {
        (byte >> 4) & 0x0f
    }
}

pub fn decode_chunk_column(
    data: &[u8],
    primary_bit_mask: u16,
    ground_up: bool,
    has_sky_light: bool,
) -> Result<ChunkColumnData> {
    let mut reader = PacketReader::new(data);
    let section_count = primary_bit_mask.count_ones() as usize;
    let mut sections = Vec::with_capacity(section_count);

    for section_y in 0..16u8 {
        if primary_bit_mask & (1 << section_y) == 0 {
            continue;
        }

        // 1.8 stores each block state as a little-endian u16; keep the raw
        // (block_id << 4) | meta value and let the consumer split it.
        let mut blocks = Vec::with_capacity(4096);
        for _ in 0..4096 {
            blocks.push(reader.read_u16_le()?);
        }
        sections.push(ChunkSectionData {
            y: section_y,
            blocks,
            block_light: Vec::new(),
            sky_light: Vec::new(),
        });
    }

    for section in &mut sections {
        section.block_light = reader.read_bytes(2048)?.to_vec();
    }

    if has_sky_light {
        for section in &mut sections {
            section.sky_light = reader.read_bytes(2048)?.to_vec();
        }
    } else {
        for section in &mut sections {
            section.sky_light = vec![0u8; 2048];
        }
    }

    let biomes = if ground_up {
        Some(reader.read_bytes(256)?.to_vec())
    } else {
        None
    };

    if !reader.is_empty() {
        return Err(ProtocolError::InvalidData(
            "chunk packet has trailing bytes",
        ));
    }

    Ok(ChunkColumnData { sections, biomes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_single_section_stone_chunk() {
        let mut data = Vec::new();
        for _ in 0..4096 {
            data.extend_from_slice(&(1u16 << 4).to_le_bytes());
        }
        data.extend(std::iter::repeat_n(0xff, 2048));
        data.extend(std::iter::repeat_n(0x00, 2048));
        data.extend(std::iter::repeat_n(1, 256));

        let decoded = decode_chunk_column(&data, 0x0001, true, true).unwrap();
        assert_eq!(decoded.sections.len(), 1);
        let section = &decoded.sections[0];
        assert_eq!(section.blocks.len(), 4096);
        assert_eq!(section.blocks[0], 1u16 << 4);
        assert_eq!(section.blocks[0] >> 4, 1); // id
        assert_eq!(section.blocks[0] & 0x0f, 0); // meta
        assert_eq!(section.block_light.len(), 2048);
        assert_eq!(nibble_at(&section.block_light, 0), 15);
        assert_eq!(section.sky_light.len(), 2048);
        assert_eq!(nibble_at(&section.sky_light, 0), 0);
        assert_eq!(decoded.biomes.as_ref().unwrap().len(), 256);
    }

    #[test]
    fn decodes_block_states_as_little_endian() {
        // Block state for id 17 (log), meta 0 == 0x0110, whose high byte is set;
        // a big-endian read would mangle it into id 4096.
        let mut data = Vec::new();
        data.extend_from_slice(&(17u16 << 4).to_le_bytes());
        for _ in 1..4096 {
            data.extend_from_slice(&0u16.to_le_bytes());
        }
        data.extend(std::iter::repeat_n(0x00, 2048));
        data.extend(std::iter::repeat_n(0x00, 2048));
        data.extend(std::iter::repeat_n(1, 256));

        let decoded = decode_chunk_column(&data, 0x0001, true, true).unwrap();
        assert_eq!(decoded.sections[0].blocks[0], 17u16 << 4);
        assert_eq!(decoded.sections[0].blocks[0] >> 4, 17);
        assert_eq!(decoded.sections[0].blocks[0] & 0x0f, 0);
    }

    #[test]
    fn section_bit_maps_to_world_height() {
        // Section present only at bit 4 → world y 64..79.
        let mut data = Vec::new();
        for _ in 0..4096 {
            data.extend_from_slice(&(1u16 << 4).to_le_bytes());
        }
        data.extend(std::iter::repeat_n(0u8, 2048));
        data.extend(std::iter::repeat_n(0u8, 2048));
        data.extend(std::iter::repeat_n(1u8, 256));
        let decoded = decode_chunk_column(&data, 0x0010, true, true).unwrap();
        assert_eq!(decoded.sections.len(), 1);
        assert_eq!(decoded.sections[0].y, 4);
    }

    #[test]
    fn decodes_light_nibbles_in_xzy_order() {
        let mut data = Vec::new();
        for _ in 0..4096 {
            data.extend_from_slice(&(1u16 << 4).to_le_bytes());
        }

        let mut block_light = vec![0u8; 2048];
        block_light[0] = 0x21;
        block_light[1] = 0x43;
        data.extend_from_slice(&block_light);

        let mut sky_light = vec![0u8; 2048];
        sky_light[0] = 0xba;
        sky_light[1] = 0xdc;
        data.extend_from_slice(&sky_light);
        data.extend(std::iter::repeat_n(1, 256));

        let decoded = decode_chunk_column(&data, 0x0001, true, true).unwrap();
        let section = &decoded.sections[0];
        assert_eq!(section.block_light[..2], [0x21, 0x43]);
        assert_eq!(nibble_at(&section.block_light, 0), 1);
        assert_eq!(nibble_at(&section.block_light, 1), 2);
        assert_eq!(nibble_at(&section.block_light, 2), 3);
        assert_eq!(nibble_at(&section.block_light, 3), 4);
        assert_eq!(section.sky_light[..2], [0xba, 0xdc]);
        assert_eq!(nibble_at(&section.sky_light, 0), 10);
        assert_eq!(nibble_at(&section.sky_light, 1), 11);
        assert_eq!(nibble_at(&section.sky_light, 2), 12);
        assert_eq!(nibble_at(&section.sky_light, 3), 13);
    }

    #[test]
    fn missing_sky_light_yields_zeroed_nibbles() {
        // Nether-style column: no sky light section on the wire, no biomes.
        let mut data = Vec::new();
        for _ in 0..4096 {
            data.extend_from_slice(&(1u16 << 4).to_le_bytes());
        }
        data.extend(std::iter::repeat_n(0x7fu8, 2048));

        let decoded = decode_chunk_column(&data, 0x0001, false, false).unwrap();
        let section = &decoded.sections[0];
        assert_eq!(section.sky_light.len(), 2048);
        assert!(section.sky_light.iter().all(|&b| b == 0));
        assert_eq!(decoded.biomes, None);
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut data = Vec::new();
        for _ in 0..4096 {
            data.extend_from_slice(&0u16.to_le_bytes());
        }
        data.extend(std::iter::repeat_n(0u8, 2048));
        data.extend(std::iter::repeat_n(0u8, 2048));
        data.extend(std::iter::repeat_n(1u8, 256));
        data.push(0xab); // junk

        let err = decode_chunk_column(&data, 0x0001, true, true).unwrap_err();
        assert_eq!(
            err,
            ProtocolError::InvalidData("chunk packet has trailing bytes")
        );
    }
}
