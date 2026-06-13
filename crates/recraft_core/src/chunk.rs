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

/// One 16×16×16 section of a chunk column: the chunk grid `(x, z)` plus the
/// vertical section index `y` (0..16). This is the unit at which terrain is
/// meshed, uploaded and frustum-culled, so editing one block only rebuilds its
/// own section instead of the whole 16×256×16 column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SectionPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl SectionPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub const fn chunk(&self) -> ChunkPos {
        ChunkPos::new(self.x, self.z)
    }
}

#[derive(Debug, Clone)]
pub struct ChunkSection {
    y: i32,
    blocks: Box<[BlockState; SECTION_VOLUME]>,
    block_light: Box<[u8; SECTION_VOLUME]>,
    sky_light: Box<[u8; SECTION_VOLUME]>,
}

impl ChunkSection {
    pub fn new(y: i32) -> Self {
        Self {
            y,
            blocks: Box::new([BlockState::AIR; SECTION_VOLUME]),
            block_light: Box::new([0; SECTION_VOLUME]),
            sky_light: Box::new([15; SECTION_VOLUME]),
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

    pub fn block_light(&self, x: u8, y: u8, z: u8) -> u8 {
        self.block_light[Self::index(x, y, z)]
    }

    pub fn set_block_light(&mut self, x: u8, y: u8, z: u8, value: u8) {
        self.block_light[Self::index(x, y, z)] = value.min(15);
    }

    pub fn sky_light(&self, x: u8, y: u8, z: u8) -> u8 {
        self.sky_light[Self::index(x, y, z)]
    }

    pub fn set_sky_light(&mut self, x: u8, y: u8, z: u8, value: u8) {
        self.sky_light[Self::index(x, y, z)] = value.min(15);
    }

    const fn index(x: u8, y: u8, z: u8) -> usize {
        (y as usize * CHUNK_SIZE * CHUNK_SIZE) + (z as usize * CHUNK_SIZE) + x as usize
    }

    /// Fill all 4096 block cells from raw 1.8 block-state values in
    /// y*256 + z*16 + x order. Each raw value is (block_id << 4) | meta.
    pub fn fill_blocks_raw(&mut self, raw: &[u16]) {
        let count = SECTION_VOLUME.min(raw.len());
        for (cell, &value) in self.blocks[..count].iter_mut().zip(&raw[..count]) {
            *cell = BlockState::new(value >> 4, (value & 0x0f) as u8);
        }
    }

    /// Fill block+sky light from 2048-byte packed-nibble arrays (1.8 wire
    /// order: nibble index == block index; low nibble = even index, high
    /// nibble = odd). Missing bytes are treated as 0.
    pub fn fill_light_nibbles(&mut self, block_light: &[u8], sky_light: &[u8]) {
        fn nibble(arr: &[u8], i: usize) -> u8 {
            let byte = arr.get(i / 2).copied().unwrap_or(0);
            if i % 2 == 0 {
                byte & 0x0f
            } else {
                (byte >> 4) & 0x0f
            }
        }
        for i in 0..SECTION_VOLUME {
            self.block_light[i] = nibble(block_light, i);
            self.sky_light[i] = nibble(sky_light, i);
        }
    }
}

/// Number of 16-block-tall sections in a 0..256 column.
pub const SECTION_COUNT: usize = 16;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub position: ChunkPos,
    // Direct-indexed by section_y (0..16). Indexing beats the old linear Vec
    // scan, which was a hot path: meshing does ~6 neighbour lookups per block.
    sections: [Option<Box<ChunkSection>>; SECTION_COUNT],
}

impl Chunk {
    pub fn new(position: ChunkPos) -> Self {
        Self {
            position,
            sections: std::array::from_fn(|_| None),
        }
    }

    pub fn sections(&self) -> impl Iterator<Item = &ChunkSection> {
        self.sections
            .iter()
            .filter_map(|section| section.as_deref())
    }

    fn section_index(section_y: i32) -> Option<usize> {
        usize::try_from(section_y)
            .ok()
            .filter(|&index| index < SECTION_COUNT)
    }

    pub fn section_mut_or_insert(&mut self, section_y: i32) -> &mut ChunkSection {
        let index = Self::section_index(section_y).expect("section_y in 0..16");
        self.sections[index].get_or_insert_with(|| Box::new(ChunkSection::new(section_y)))
    }

    pub fn section(&self, section_y: i32) -> Option<&ChunkSection> {
        Self::section_index(section_y).and_then(|index| self.sections[index].as_deref())
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

    pub fn light_at(&self, x: u8, y: i32, z: u8) -> (u8, u8) {
        if !(0..256).contains(&y) {
            return (0, 15);
        }
        let section_y = y >> 4;
        let local_y = (y & 15) as u8;
        self.section(section_y)
            .map(|section| {
                (
                    section.block_light(x, local_y, z),
                    section.sky_light(x, local_y, z),
                )
            })
            .unwrap_or((0, 15))
    }

    pub fn set_light(&mut self, x: u8, y: i32, z: u8, block_light: u8, sky_light: u8) {
        if !(0..256).contains(&y) {
            return;
        }
        let section_y = y >> 4;
        let local_y = (y & 15) as u8;
        let section = self.section_mut_or_insert(section_y);
        section.set_block_light(x, local_y, z, block_light);
        section.set_sky_light(x, local_y, z, sky_light);
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

    #[test]
    fn chunk_light_round_trip() {
        let mut chunk = Chunk::new(ChunkPos::new(0, 0));
        chunk.set_light(1, 64, 2, 7, 12);
        assert_eq!(chunk.light_at(1, 64, 2), (7, 12));
    }

    #[test]
    fn fill_blocks_raw_matches_index_layout() {
        let mut section = ChunkSection::new(0);
        let mut raw = vec![0u16; SECTION_VOLUME];
        // STONE (id 1, meta 0) at (x=1, y=2, z=3): index = y*256 + z*16 + x.
        raw[2 * 256 + 3 * 16 + 1] = 1 << 4;
        // id 35, meta 5 at (x=15, y=15, z=15).
        raw[15 * 256 + 15 * 16 + 15] = (35 << 4) | 5;
        section.fill_blocks_raw(&raw);
        assert_eq!(section.get(1, 2, 3), BlockState::STONE);
        assert_eq!(section.get(15, 15, 15), BlockState::new(35, 5));
        assert_eq!(section.get(0, 0, 0), BlockState::AIR);
    }

    #[test]
    fn fill_light_nibbles_round_trips() {
        let mut section = ChunkSection::new(0);
        let mut block_light = [0u8; SECTION_VOLUME / 2];
        // Block index 0 -> low nibble of byte 0, index 1 -> high nibble.
        block_light[0] = 0x21;
        let mut sky_light = [0u8; SECTION_VOLUME / 2];
        sky_light[0] = 0x43;
        section.fill_light_nibbles(&block_light, &sky_light);
        assert_eq!(section.block_light(0, 0, 0), 1);
        assert_eq!(section.block_light(1, 0, 0), 2);
        assert_eq!(section.sky_light(0, 0, 0), 3);
        assert_eq!(section.sky_light(1, 0, 0), 4);
        // Unset bytes are zero.
        assert_eq!(section.block_light(2, 0, 0), 0);
    }

    #[test]
    fn fill_light_nibbles_tolerates_short_arrays() {
        let mut section = ChunkSection::new(0);
        section.fill_light_nibbles(&[0x21], &[]);
        assert_eq!(section.block_light(0, 0, 0), 1);
        assert_eq!(section.block_light(1, 0, 0), 2);
        assert_eq!(section.sky_light(0, 0, 0), 0);
        assert_eq!(section.sky_light(15, 15, 15), 0);
    }
}
