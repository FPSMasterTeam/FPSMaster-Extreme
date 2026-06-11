use crate::BlockState;

pub const CHUNK_SIZE: usize = 16;
pub const SECTION_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

impl ChunkPos {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }
}

#[derive(Debug, Clone)]
pub struct ChunkSection {
    y: i32,
    blocks: Box<[BlockState; SECTION_VOLUME]>,
}

impl ChunkSection {
    pub fn new(y: i32) -> Self {
        Self {
            y,
            blocks: Box::new([BlockState::AIR; SECTION_VOLUME]),
        }
    }

    pub const fn y(&self) -> i32 {
        self.y
    }

    pub fn get(&self, x: u8, y: u8, z: u8) -> BlockState {
        self.blocks[Self::index(x, y, z)]
    }

    pub fn set(&mut self, x: u8, y: u8, z: u8, block: BlockState) {
        self.blocks[Self::index(x, y, z)] = block;
    }

    const fn index(x: u8, y: u8, z: u8) -> usize {
        (y as usize * CHUNK_SIZE * CHUNK_SIZE) + (z as usize * CHUNK_SIZE) + x as usize
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub position: ChunkPos,
    sections: Vec<ChunkSection>,
}

impl Chunk {
    pub fn new(position: ChunkPos) -> Self {
        Self {
            position,
            sections: Vec::new(),
        }
    }

    pub fn sections(&self) -> &[ChunkSection] {
        &self.sections
    }

    pub fn section_mut_or_insert(&mut self, section_y: i32) -> &mut ChunkSection {
        if let Some(index) = self
            .sections
            .iter()
            .position(|section| section.y() == section_y)
        {
            return &mut self.sections[index];
        }

        self.sections.push(ChunkSection::new(section_y));
        self.sections.sort_by_key(ChunkSection::y);
        let index = self
            .sections
            .iter()
            .position(|section| section.y() == section_y)
            .expect("inserted section must exist");
        &mut self.sections[index]
    }

    pub fn section(&self, section_y: i32) -> Option<&ChunkSection> {
        self.sections
            .iter()
            .find(|section| section.y() == section_y)
    }

    pub fn get_block(&self, x: u8, y: i32, z: u8) -> BlockState {
        if !(0..256).contains(&y) {
            return BlockState::AIR;
        }
        let section_y = y >> 4;
        let local_y = (y & 15) as u8;
        self.section(section_y)
            .map(|section| section.get(x, local_y, z))
            .unwrap_or(BlockState::AIR)
    }

    pub fn set_block(&mut self, x: u8, y: i32, z: u8, block: BlockState) {
        if !(0..256).contains(&y) {
            return;
        }
        let section_y = y >> 4;
        let local_y = (y & 15) as u8;
        self.section_mut_or_insert(section_y)
            .set(x, local_y, z, block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_blocks_round_trip() {
        let mut chunk = Chunk::new(ChunkPos::new(0, 0));
        chunk.set_block(1, 64, 2, BlockState::STONE);
        assert_eq!(chunk.get_block(1, 64, 2), BlockState::STONE);
        assert_eq!(chunk.get_block(1, 65, 2), BlockState::AIR);
    }
}
