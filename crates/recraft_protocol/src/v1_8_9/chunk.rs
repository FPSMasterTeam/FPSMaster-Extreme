use crate::io::{PacketReader, ProtocolError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedBlock {
    pub x: u8,
    pub y: u8,
    pub z: u8,
    pub id: u16,
    pub meta: u8,
    pub block_light: u8,
    pub sky_light: u8,
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

pub fn decode_chunk_data(
    data: &[u8],
    primary_bit_mask: u16,
    ground_up: bool,
    has_sky_light: bool,
) -> Result<DecodedChunkData> {
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
                        block_light: 0,
                        sky_light: if has_sky_light { 15 } else { 0 },
                    });
                }
            }
        }
        sections.push(DecodedSection {
            y: section_y,
            blocks,
        });
    }

    for section in &mut sections {
        let light = reader.read_bytes(2048)?;
        apply_nibble_light(&mut section.blocks, light, LightKind::Block);
    }

    if has_sky_light {
        for section in &mut sections {
            let light = reader.read_bytes(2048)?;
            apply_nibble_light(&mut section.blocks, light, LightKind::Sky);
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

    Ok(DecodedChunkData { sections, biomes })
}

#[derive(Debug, Clone, Copy)]
enum LightKind {
    Block,
    Sky,
}

fn apply_nibble_light(blocks: &mut [DecodedBlock], light: &[u8], kind: LightKind) {
    for (index, block) in blocks.iter_mut().enumerate() {
        let value = nibble_at(light, index);
        match kind {
            LightKind::Block => block.block_light = value,
            LightKind::Sky => block.sky_light = value,
        }
    }
}

fn nibble_at(bytes: &[u8], index: usize) -> u8 {
    let byte = bytes[index / 2];
    if index % 2 == 0 {
        byte & 0x0f
    } else {
        (byte >> 4) & 0x0f
    }
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
        assert_eq!(decoded.sections[0].blocks[0].block_light, 15);
        assert_eq!(decoded.sections[0].blocks[0].sky_light, 0);
        assert_eq!(decoded.biomes.as_ref().unwrap().len(), 256);
    }

    #[test]
    fn decodes_light_nibbles_in_xzy_order() {
        let mut data = Vec::new();
        for _ in 0..4096 {
            data.extend_from_slice(&(1u16 << 4).to_be_bytes());
        }

        let mut block_light = vec![0u8; 2048];
        block_light[0] = 0x21;
        block_light[1] = 0x43;
        data.extend_from_slice(&block_light);

        let mut sky_light = vec![0u8; 2048];
        sky_light[0] = 0xba;
        sky_light[1] = 0xdc;
        data.extend_from_slice(&sky_light);
        data.extend(std::iter::repeat(1).take(256));

        let decoded = decode_chunk_data(&data, 0x0001, true, true).unwrap();
        let blocks = &decoded.sections[0].blocks;
        assert_eq!(blocks[0].block_light, 1);
        assert_eq!(blocks[1].block_light, 2);
        assert_eq!(blocks[2].block_light, 3);
        assert_eq!(blocks[3].block_light, 4);
        assert_eq!(blocks[0].sky_light, 10);
        assert_eq!(blocks[1].sky_light, 11);
        assert_eq!(blocks[2].sky_light, 12);
        assert_eq!(blocks[3].sky_light, 13);
    }
}
