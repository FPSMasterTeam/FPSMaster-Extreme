use std::collections::{HashMap, HashSet, VecDeque};

use crate::{BlockState, Chunk, ChunkPos, EntityId, EntityState, SectionPos};

#[derive(Debug, Default)]
pub struct World {
    chunks: HashMap<ChunkPos, Chunk>,
    entities: HashMap<EntityId, EntityState>,
}

impl World {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.chunks.values()
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos)
    }

    /// Whether the chunk containing the given world-space block column is
    /// loaded. Used to avoid running local physics (and falling) through
    /// terrain the server has not sent yet.
    pub fn is_block_column_loaded(&self, x: i32, z: i32) -> bool {
        self.chunks
            .contains_key(&ChunkPos::new(div_floor(x, 16), div_floor(z, 16)))
    }

    pub fn remove_chunk(&mut self, pos: ChunkPos) {
        self.chunks.remove(&pos);
    }

    pub fn chunk_mut_or_insert(&mut self, pos: ChunkPos) -> &mut Chunk {
        self.chunks.entry(pos).or_insert_with(|| Chunk::new(pos))
    }

    /// Load a fully-decoded chunk section in one shot. `section_y` is 0..16.
    /// `blocks` is 4096 raw 1.8 states (y*256 + z*16 + x order). `block_light`
    /// and `sky_light` are 2048 packed nibbles each. Creates the chunk/section
    /// if absent. No per-block hashmap work.
    pub fn load_section(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        section_y: i32,
        blocks: &[u16],
        block_light: &[u8],
        sky_light: &[u8],
    ) {
        let chunk = self.chunk_mut_or_insert(ChunkPos::new(chunk_x, chunk_z));
        let section = chunk.section_mut_or_insert(section_y);
        section.fill_blocks_raw(blocks);
        section.fill_light_nibbles(block_light, sky_light);
    }

    pub fn set_block(&mut self, x: i32, y: i32, z: i32, block: BlockState) {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        self.chunk_mut_or_insert(pos)
            .set_block(local_x, y, local_z, block);
    }

    pub fn set_block_if_chunk_loaded(&mut self, x: i32, y: i32, z: i32, block: BlockState) -> bool {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return false;
        };
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        chunk.set_block(local_x, y, local_z, block);
        true
    }

    pub fn block_at(&self, x: i32, y: i32, z: i32) -> BlockState {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let Some(chunk) = self.chunk(pos) else {
            return BlockState::AIR;
        };
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        chunk.get_block(local_x, y, local_z)
    }

    pub fn set_light(&mut self, x: i32, y: i32, z: i32, block_light: u8, sky_light: u8) {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        self.chunk_mut_or_insert(pos)
            .set_light(local_x, y, local_z, block_light, sky_light);
    }

    pub fn light_at(&self, x: i32, y: i32, z: i32) -> (u8, u8) {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let Some(chunk) = self.chunk(pos) else {
            return (0, 15);
        };
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        chunk.light_at(local_x, y, local_z)
    }

    /// Recompute sky-light for all loaded columns: per-column vertical cast
    /// followed by BFS horizontal/downward propagation so caves and overhangs
    /// get gradual sky light from nearby openings. Run once after generating a
    /// demo/offline world.
    pub fn recompute_vertical_skylight(&mut self) {
        for chunk in self.chunks.values_mut() {
            chunk.recompute_vertical_skylight();
        }
        self.propagate_sky_light();
    }

    /// BFS-propagate sky light from vertical-cast boundaries into covered
    /// areas. Decays by 1 per horizontal/upward step; passes straight down
    /// through transparent blocks with no decay (vanilla rule).
    fn propagate_sky_light(&mut self) {
        let mut lit_cells: Vec<(i32, i32, i32)> = Vec::new();
        for (cpos, chunk) in &self.chunks {
            let cx = cpos.x * 16;
            let cz = cpos.z * 16;
            for section in chunk.sections() {
                let sy = section.y();
                for yl in 0..16u8 {
                    for zl in 0..16u8 {
                        for xl in 0..16u8 {
                            if section.sky_light(xl, yl, zl) > 0 {
                                lit_cells.push((
                                    cx + xl as i32,
                                    sy * 16 + yl as i32,
                                    cz + zl as i32,
                                ));
                            }
                        }
                    }
                }
            }
        }

        let mut queue: VecDeque<(i32, i32, i32)> = VecDeque::new();
        for (wx, wy, wz) in lit_cells {
            let sky = self.light_at(wx, wy, wz).1;
            let is_seed = neighbors(wx, wy, wz).iter().any(|&(nx, ny, nz)| {
                if !(0..256).contains(&ny) || !self.is_block_column_loaded(nx, nz) {
                    return false;
                }
                if self.block_at(nx, ny, nz).is_opaque_cube() {
                    return false;
                }
                let new_level = if ny < wy { sky } else { sky.saturating_sub(1) };
                new_level > 0 && new_level > self.light_at(nx, ny, nz).1
            });
            if is_seed {
                queue.push_back((wx, wy, wz));
            }
        }

        while let Some((cx, cy, cz)) = queue.pop_front() {
            let level = self.light_at(cx, cy, cz).1;
            if level == 0 {
                continue;
            }
            for (nx, ny, nz) in neighbors(cx, cy, cz) {
                if !(0..256).contains(&ny) || !self.is_block_column_loaded(nx, nz) {
                    continue;
                }
                if self.block_at(nx, ny, nz).is_opaque_cube() {
                    continue;
                }
                let new_level = if ny < cy { level } else { level.saturating_sub(1) };
                if new_level > 0 && new_level > self.light_at(nx, ny, nz).1 {
                    let bl = self.light_at(nx, ny, nz).0;
                    self.set_light(nx, ny, nz, bl, new_level);
                    queue.push_back((nx, ny, nz));
                }
            }
        }
    }

    /// Flood-fill block-light around a just-changed block (offline/demo worlds,
    /// which have no server lightmap). `old` is the block previously at (x,y,z);
    /// the new block must already be set. Runs the classic Minecraft two-phase
    /// update (remove the old light, then re-propagate) and returns every section
    /// whose light changed so the caller can re-mesh them. Only spreads into
    /// already-loaded chunks. Sky-light is left untouched.
    pub fn update_block_light(&mut self, x: i32, y: i32, z: i32, old: BlockState) -> Vec<SectionPos> {
        let mut changed: HashSet<SectionPos> = HashSet::new();
        let old_level = old.luminance().max(self.light_at(x, y, z).0);

        // Phase 1: removal. Clear the source and any cell that was lit only by it,
        // collecting brighter borders as re-propagation seeds.
        let mut removal: VecDeque<(i32, i32, i32, u8)> = VecDeque::new();
        let mut additions: VecDeque<(i32, i32, i32)> = VecDeque::new();
        self.set_block_light_tracked(x, y, z, 0, &mut changed);
        removal.push_back((x, y, z, old_level));
        while let Some((cx, cy, cz, level)) = removal.pop_front() {
            for (nx, ny, nz) in neighbors(cx, cy, cz) {
                if !(0..256).contains(&ny) || !self.is_block_column_loaded(nx, nz) {
                    continue;
                }
                let nl = self.light_at(nx, ny, nz).0;
                if nl != 0 && nl < level {
                    self.set_block_light_tracked(nx, ny, nz, 0, &mut changed);
                    removal.push_back((nx, ny, nz, nl));
                } else if nl >= level {
                    additions.push_back((nx, ny, nz));
                }
            }
        }

        // Phase 2: addition. Seed the new emitter, then flood outward.
        let emit = self.block_at(x, y, z).luminance();
        if emit > 0 {
            self.set_block_light_tracked(x, y, z, emit, &mut changed);
            additions.push_back((x, y, z));
        }
        while let Some((cx, cy, cz)) = additions.pop_front() {
            let level = self.light_at(cx, cy, cz).0;
            if level <= 1 {
                continue;
            }
            for (nx, ny, nz) in neighbors(cx, cy, cz) {
                if !(0..256).contains(&ny) || !self.is_block_column_loaded(nx, nz) {
                    continue;
                }
                if self.block_at(nx, ny, nz).is_opaque_cube() {
                    continue;
                }
                let nl = self.light_at(nx, ny, nz).0;
                if level - 1 > nl {
                    self.set_block_light_tracked(nx, ny, nz, level - 1, &mut changed);
                    additions.push_back((nx, ny, nz));
                }
            }
        }

        changed.into_iter().collect()
    }

    /// Set only the block-light nibble (preserving sky-light) and record the
    /// affected section plus border neighbours.
    fn set_block_light_tracked(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        value: u8,
        changed: &mut HashSet<SectionPos>,
    ) {
        let sky = self.light_at(x, y, z).1;
        self.set_light(x, y, z, value, sky);
        insert_section_borders(x, y, z, changed);
    }

    /// Two-phase sky-light update after a block edit: vertical cast for the
    /// edited column, BFS removal of stale propagated light, then BFS
    /// re-propagation from remaining bright borders. Decays by 1 per
    /// horizontal/upward step; passes straight down through transparent blocks
    /// with no decay (vanilla rule). Returns sections whose sky-light changed.
    pub fn update_sky_light(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        old: BlockState,
    ) -> Vec<SectionPos> {
        let mut changed: HashSet<SectionPos> = HashSet::new();
        let mut removal: VecDeque<(i32, i32, i32, u8)> = VecDeque::new();
        let mut additions: VecDeque<(i32, i32, i32)> = VecDeque::new();

        // --- Vertical cast for the edited column ----------------------------
        let cpos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let section_ys: Vec<i32> = match self.chunks.get(&cpos) {
            Some(chunk) => chunk.sections().map(|s| s.y()).collect(),
            None => return Vec::new(),
        };
        if let Some(&top) = section_ys.iter().max() {
            let mut blocked = false;
            let mut vy = top * 16 + 15;
            while vy >= 0 {
                if section_ys.contains(&vy.div_euclid(16)) {
                    if !blocked && self.block_at(x, vy, z).is_opaque_cube() {
                        blocked = true;
                    }
                    let want = if blocked { 0 } else { 15 };
                    let (block_l, old_sky) = self.light_at(x, vy, z);
                    if old_sky != want {
                        self.set_light(x, vy, z, block_l, want);
                        insert_section_borders(x, vy, z, &mut changed);
                        if want < old_sky {
                            removal.push_back((x, vy, z, old_sky));
                        } else {
                            additions.push_back((x, vy, z));
                        }
                    }
                }
                vy -= 1;
            }
        }

        // --- Handle opacity change at the edited position -------------------
        if old.is_opaque_cube() && !self.block_at(x, y, z).is_opaque_cube() {
            for (nx, ny, nz) in neighbors(x, y, z) {
                if (0..256).contains(&ny)
                    && self.is_block_column_loaded(nx, nz)
                    && self.light_at(nx, ny, nz).1 > 0
                {
                    additions.push_back((nx, ny, nz));
                }
            }
        }

        // --- Phase 1: BFS removal of stale propagated sky light -------------
        while let Some((cx, cy, cz, level)) = removal.pop_front() {
            for (nx, ny, nz) in neighbors(cx, cy, cz) {
                if !(0..256).contains(&ny) || !self.is_block_column_loaded(nx, nz) {
                    continue;
                }
                let nl = self.light_at(nx, ny, nz).1;
                if nl == 0 {
                    continue;
                }
                // Downward: no decay, so the neighbour could be at `level`.
                let removes = if ny < cy { nl <= level } else { nl < level };
                if removes {
                    self.set_sky_light_tracked(nx, ny, nz, 0, &mut changed);
                    removal.push_back((nx, ny, nz, nl));
                } else {
                    additions.push_back((nx, ny, nz));
                }
            }
        }

        // --- Phase 2: BFS re-propagation ------------------------------------
        while let Some((cx, cy, cz)) = additions.pop_front() {
            let level = self.light_at(cx, cy, cz).1;
            if level == 0 {
                continue;
            }
            for (nx, ny, nz) in neighbors(cx, cy, cz) {
                if !(0..256).contains(&ny) || !self.is_block_column_loaded(nx, nz) {
                    continue;
                }
                if self.block_at(nx, ny, nz).is_opaque_cube() {
                    continue;
                }
                let new_level = if ny < cy { level } else { level.saturating_sub(1) };
                if new_level > 0 && new_level > self.light_at(nx, ny, nz).1 {
                    self.set_sky_light_tracked(nx, ny, nz, new_level, &mut changed);
                    additions.push_back((nx, ny, nz));
                }
            }
        }

        changed.into_iter().collect()
    }

    fn set_sky_light_tracked(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        value: u8,
        changed: &mut HashSet<SectionPos>,
    ) {
        let bl = self.light_at(x, y, z).0;
        self.set_light(x, y, z, bl, value);
        insert_section_borders(x, y, z, changed);
    }

    pub fn upsert_entity(&mut self, entity: EntityState) {
        self.entities.insert(entity.id, entity);
    }

    pub fn remove_entity(&mut self, id: EntityId) {
        self.entities.remove(&id);
    }

    pub fn entity(&self, id: EntityId) -> Option<&EntityState> {
        self.entities.get(&id)
    }

    pub fn entity_mut(&mut self, id: EntityId) -> Option<&mut EntityState> {
        self.entities.get_mut(&id)
    }

    pub fn entities_mut(&mut self) -> impl Iterator<Item = &mut EntityState> {
        self.entities.values_mut()
    }

    pub fn entities(&self) -> impl Iterator<Item = &EntityState> {
        self.entities.values()
    }
}

/// Record the section containing (x,y,z) plus any neighbour section across a
/// touched border, so cross-section smooth lighting re-meshes too.
fn insert_section_borders(x: i32, y: i32, z: i32, changed: &mut HashSet<SectionPos>) {
    let sx = div_floor(x, 16);
    let sz = div_floor(z, 16);
    let sy = y.div_euclid(16);
    changed.insert(SectionPos::new(sx, sy, sz));
    let (lx, ly, lz) = (mod_floor(x, 16), y.rem_euclid(16), mod_floor(z, 16));
    if lx == 0 {
        changed.insert(SectionPos::new(sx - 1, sy, sz));
    } else if lx == 15 {
        changed.insert(SectionPos::new(sx + 1, sy, sz));
    }
    if lz == 0 {
        changed.insert(SectionPos::new(sx, sy, sz - 1));
    } else if lz == 15 {
        changed.insert(SectionPos::new(sx, sy, sz + 1));
    }
    if ly == 0 && sy > 0 {
        changed.insert(SectionPos::new(sx, sy - 1, sz));
    } else if ly == 15 && sy < 15 {
        changed.insert(SectionPos::new(sx, sy + 1, sz));
    }
}

fn neighbors(x: i32, y: i32, z: i32) -> [(i32, i32, i32); 6] {
    [
        (x - 1, y, z),
        (x + 1, y, z),
        (x, y - 1, z),
        (x, y + 1, z),
        (x, y, z - 1),
        (x, y, z + 1),
    ]
}

fn div_floor(value: i32, divisor: i32) -> i32 {
    let quotient = value / divisor;
    let remainder = value % divisor;
    if remainder != 0 && ((remainder < 0) != (divisor < 0)) {
        quotient - 1
    } else {
        quotient
    }
}

fn mod_floor(value: i32, divisor: i32) -> i32 {
    let remainder = value % divisor;
    if remainder < 0 {
        remainder + divisor.abs()
    } else {
        remainder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_light_floods_and_clears() {
        let mut world = World::new();
        // A loaded air column (creates chunk 0,0 so propagation is allowed).
        world.set_block(8, 8, 8, BlockState::AIR);
        let glowstone = BlockState::new(89, 0);

        world.set_block(8, 8, 8, glowstone);
        world.update_block_light(8, 8, 8, BlockState::AIR);
        assert_eq!(world.light_at(8, 8, 8).0, 15, "emitter is full bright");
        assert_eq!(world.light_at(9, 8, 8).0, 14, "neighbour falls off by 1");
        assert_eq!(world.light_at(13, 8, 8).0, 10, "5 blocks away = 15-5");
        assert_eq!(world.light_at(8, 8, 8).1, 15, "sky-light untouched");

        // Removing it clears the whole flood (no other sources).
        world.set_block(8, 8, 8, BlockState::AIR);
        world.update_block_light(8, 8, 8, glowstone);
        assert_eq!(world.light_at(8, 8, 8).0, 0);
        assert_eq!(world.light_at(9, 8, 8).0, 0);
        assert_eq!(world.light_at(13, 8, 8).0, 0);
    }

    #[test]
    fn roofing_a_column_darkens_skylight_below() {
        let mut world = World::new();
        world.set_block(2, 64, 2, BlockState::STONE);
        world.recompute_vertical_skylight();
        assert_eq!(world.light_at(2, 70, 2).1, 15);

        world.set_block(2, 72, 2, BlockState::STONE);
        world.update_sky_light(2, 72, 2, BlockState::AIR);
        // Adjacent columns are open sky → horizontal propagation provides 14.
        assert_eq!(world.light_at(2, 70, 2).1, 14);

        world.set_block(2, 72, 2, BlockState::AIR);
        world.update_sky_light(2, 72, 2, BlockState::STONE);
        assert_eq!(world.light_at(2, 70, 2).1, 15);
    }

    #[test]
    fn sky_light_propagates_under_ceiling() {
        let mut world = World::new();
        // 3×3 ceiling with a 1×1 opening at the centre.
        for x in -1..=1 {
            for z in -1..=1 {
                world.set_block(x, 68, z, BlockState::STONE);
                world.set_block(x, 64, z, BlockState::STONE);
            }
        }
        world.set_block(0, 68, 0, BlockState::AIR); // skylight opening

        world.recompute_vertical_skylight();

        assert_eq!(world.light_at(0, 67, 0).1, 15, "directly under opening");
        assert_eq!(world.light_at(1, 67, 0).1, 14, "one block from opening");
        assert_eq!(world.light_at(-1, 67, 0).1, 14);
        assert_eq!(world.light_at(0, 67, 1).1, 14);
        assert_eq!(world.light_at(0, 67, -1).1, 14);
    }

    #[test]
    fn sky_light_descends_without_decay() {
        let mut world = World::new();
        // Open column at x=0, y=69 is the lowest open-air block.
        // Wall at x=0 from y=68 down blocks horizontal light to x=1.
        world.set_block(0, 64, 0, BlockState::STONE);
        for wy in 65..=68 {
            world.set_block(0, wy, 0, BlockState::STONE);
        }
        // Covered shaft at x=1: ceiling at y=70, open inside.
        world.set_block(1, 70, 0, BlockState::STONE);
        world.set_block(1, 64, 0, BlockState::STONE);
        // Seal z-sides and far x-side of the shaft.
        for wy in 65..70 {
            world.set_block(1, wy, -1, BlockState::STONE);
            world.set_block(1, wy, 1, BlockState::STONE);
            world.set_block(2, wy, 0, BlockState::STONE);
        }

        world.recompute_vertical_skylight();

        assert_eq!(world.light_at(0, 69, 0).1, 15, "open air");
        assert_eq!(world.light_at(1, 69, 0).1, 14, "horizontal from open column");
        // Below: the only light path is downward from (1,69) — no horizontal
        // source because of the wall at x=0. Downward has no decay.
        assert_eq!(world.light_at(1, 68, 0).1, 14, "down without decay");
        assert_eq!(world.light_at(1, 67, 0).1, 14, "still 14 further down");
        assert_eq!(world.light_at(1, 66, 0).1, 14, "still 14 at bottom");
    }

    #[test]
    fn incremental_sky_light_removes_and_restores() {
        let mut world = World::new();
        // Two columns: x=0 open, x=1 will be covered.
        world.set_block(0, 64, 0, BlockState::STONE);
        world.set_block(1, 64, 0, BlockState::STONE);
        world.recompute_vertical_skylight();
        assert_eq!(world.light_at(1, 67, 0).1, 15);

        // Roof x=1 → sky drops, but gets propagated 14 from x=0.
        world.set_block(1, 70, 0, BlockState::STONE);
        world.update_sky_light(1, 70, 0, BlockState::AIR);
        assert_eq!(world.light_at(1, 69, 0).1, 14);

        // Remove roof → full sky-light restored.
        world.set_block(1, 70, 0, BlockState::AIR);
        world.update_sky_light(1, 70, 0, BlockState::STONE);
        assert_eq!(world.light_at(1, 69, 0).1, 15);
    }

    #[test]
    fn block_light_does_not_enter_opaque_blocks() {
        let mut world = World::new();
        world.set_block(8, 8, 8, BlockState::AIR);
        // Wall of stone directly beside where the torch will go.
        world.set_block(9, 8, 8, BlockState::STONE);
        world.set_block(8, 8, 8, BlockState::new(50, 0)); // torch (lum 14)
        world.update_block_light(8, 8, 8, BlockState::AIR);
        assert_eq!(world.light_at(8, 8, 8).0, 14);
        assert_eq!(world.light_at(9, 8, 8).0, 0, "opaque stone receives no light");
    }

    #[test]
    fn world_coordinates_handle_negative_chunks() {
        let mut world = World::new();
        world.set_block(-1, 64, -1, BlockState::DIRT);
        assert_eq!(world.block_at(-1, 64, -1), BlockState::DIRT);
        assert_eq!(
            world
                .chunk(ChunkPos::new(-1, -1))
                .unwrap()
                .get_block(15, 64, 15),
            BlockState::DIRT
        );
    }

    #[test]
    fn load_section_bulk_loads_blocks_and_light() {
        let mut world = World::new();
        let mut blocks = vec![0u16; crate::chunk::SECTION_VOLUME];
        // STONE at local (x=5, y=6, z=7) within section 4 of chunk (-1, -1).
        blocks[6 * 256 + 7 * 16 + 5] = 1 << 4;
        let mut block_light = vec![0u8; 2048];
        let mut sky_light = vec![0u8; 2048];
        let light_index = 6 * 256 + 7 * 16 + 5; // 1653, odd -> high nibble
        block_light[light_index / 2] = 0x90; // high nibble = 9
        sky_light[light_index / 2] = 0xd0; // high nibble = 13
        world.load_section(-1, -1, 4, &blocks, &block_light, &sky_light);

        let (world_x, world_y, world_z) = (-16 + 5, 4 * 16 + 6, -16 + 7);
        assert_eq!(world.block_at(world_x, world_y, world_z), BlockState::STONE);
        assert_eq!(
            world.block_at(world_x, world_y + 1, world_z),
            BlockState::AIR
        );
        assert_eq!(world.light_at(world_x, world_y, world_z), (9, 13));

        // Also exercise a non-negative chunk.
        world.load_section(2, 3, 0, &blocks, &block_light, &sky_light);
        assert_eq!(world.block_at(2 * 16 + 5, 6, 3 * 16 + 7), BlockState::STONE);
    }

    #[test]
    fn world_light_handles_negative_chunks() {
        let mut world = World::new();
        world.set_light(-1, 64, -1, 3, 14);
        assert_eq!(world.light_at(-1, 64, -1), (3, 14));
        assert_eq!(
            world
                .chunk(ChunkPos::new(-1, -1))
                .unwrap()
                .light_at(15, 64, 15),
            (3, 14)
        );
    }
}
