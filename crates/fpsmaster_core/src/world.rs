use std::collections::{HashMap, HashSet, VecDeque};

use crate::{BlockState, Chunk, ChunkPos, EntityId, EntityState, SectionPos};

#[derive(Debug, Default)]
pub struct World {
    chunks: HashMap<ChunkPos, Chunk>,
    entities: HashMap<EntityId, EntityState>,
    /// Index of block-entity positions (signs, chests, enchanting tables, end
    /// portals) → their current state, kept in sync on every block mutation.
    /// Lets the renderer enumerate the handful of block-entities directly each
    /// frame instead of brute-force scanning every loaded section's full 16³.
    block_entities: HashMap<[i32; 3], BlockState>,
}

/// Block ids that carry a client-rendered block-entity (vanilla TileEntity):
/// chest/trapped/ender (54/146/130), standing/wall sign (63/68), enchanting
/// table (116) and end portal (119).
pub const fn is_block_entity(id: u16) -> bool {
    matches!(id, 54 | 63 | 68 | 116 | 119 | 130 | 146)
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
        let (x0, z0) = (pos.x * 16, pos.z * 16);
        self.block_entities
            .retain(|&[x, _, z], _| !(x >= x0 && x < x0 + 16 && z >= z0 && z < z0 + 16));
    }

    /// Block-entity positions and states currently loaded, for the renderer's
    /// per-frame block-entity pass. See [`is_block_entity`].
    pub fn block_entities(&self) -> impl Iterator<Item = (&[i32; 3], &BlockState)> {
        self.block_entities.iter()
    }

    /// Keep the block-entity index in step with a single-cell change.
    fn index_block(&mut self, x: i32, y: i32, z: i32, block: BlockState) {
        if is_block_entity(block.id) {
            self.block_entities.insert([x, y, z], block);
        } else {
            self.block_entities.remove(&[x, y, z]);
        }
    }

    /// Rebuild the block-entity index for one freshly loaded section (a bulk
    /// load replaces all 4096 cells, so drop the section's old entries first).
    fn reindex_section(&mut self, chunk_x: i32, chunk_z: i32, section_y: i32) {
        let (x0, z0, y0) = (chunk_x * 16, chunk_z * 16, section_y * 16);
        self.block_entities.retain(|&[x, y, z], _| {
            !(x >= x0 && x < x0 + 16 && z >= z0 && z < z0 + 16 && y >= y0 && y < y0 + 16)
        });
        let Some(section) = self
            .chunks
            .get(&ChunkPos::new(chunk_x, chunk_z))
            .and_then(|c| c.sections().find(|s| s.y() == section_y))
        else {
            return;
        };
        let mut found = Vec::new();
        for ly in 0..16u8 {
            for lz in 0..16u8 {
                for lx in 0..16u8 {
                    let b = section.get(lx, ly, lz);
                    if is_block_entity(b.id) {
                        found.push(([x0 + lx as i32, y0 + ly as i32, z0 + lz as i32], b));
                    }
                }
            }
        }
        for (p, b) in found {
            self.block_entities.insert(p, b);
        }
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
        self.reindex_section(chunk_x, chunk_z, section_y);
    }

    pub fn set_block(&mut self, x: i32, y: i32, z: i32, block: BlockState) {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        self.chunk_mut_or_insert(pos)
            .set_block(local_x, y, local_z, block);
        self.index_block(x, y, z, block);
    }

    pub fn set_block_if_chunk_loaded(&mut self, x: i32, y: i32, z: i32, block: BlockState) -> bool {
        let pos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return false;
        };
        let local_x = mod_floor(x, 16) as u8;
        let local_z = mod_floor(z, 16) as u8;
        chunk.set_block(local_x, y, local_z, block);
        self.index_block(x, y, z, block);
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

    /// Vanilla propagation cost of light entering the cell at (x,y,z):
    /// `max(1, lightOpacity)` (air 1, water/ice 3, leaves 1, ...), or None when
    /// the block is fully light-blocking (opacity 15) and carries no light.
    fn light_cost(&self, x: i32, y: i32, z: i32) -> Option<u8> {
        let opacity = self.block_at(x, y, z).light_opacity();
        if opacity >= 15 {
            None
        } else {
            Some(opacity.max(1))
        }
    }

    /// BFS-propagate sky light from vertical-cast boundaries into covered
    /// areas, decaying by the vanilla per-block cost `max(1, lightOpacity)`
    /// per step (so water columns attenuate 3/block, matching the values
    /// vanilla servers send).
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
                let Some(cost) = self.light_cost(nx, ny, nz) else {
                    return false;
                };
                let new_level = sky.saturating_sub(cost);
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
                let Some(cost) = self.light_cost(nx, ny, nz) else {
                    continue;
                };
                let new_level = level.saturating_sub(cost);
                if new_level > 0 && new_level > self.light_at(nx, ny, nz).1 {
                    let bl = self.light_at(nx, ny, nz).0;
                    self.set_light(nx, ny, nz, bl, new_level);
                    queue.push_back((nx, ny, nz));
                }
            }
        }
    }

    /// Flood-fill block-light around a just-changed block. `old` is the block
    /// previously at (x,y,z);
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
                let Some(cost) = self.light_cost(nx, ny, nz) else {
                    continue;
                };
                let new_level = level.saturating_sub(cost);
                let nl = self.light_at(nx, ny, nz).0;
                if new_level > nl {
                    self.set_block_light_tracked(nx, ny, nz, new_level, &mut changed);
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

    /// Incremental sky-light update after a block edit, vanilla-style and
    /// LOCAL: only the cells whose direct-sky status flipped (the heightmap
    /// range that opened/closed) plus the edited cell are invalidated; the
    /// removal/re-add BFS then relaxes just the affected neighbourhood with the
    /// vanilla `max(1, lightOpacity)` per-block decay. Server-sent light
    /// outside the touched region is never rewritten, so a relight is
    /// idempotent over vanilla values and costs O(light radius), not O(column
    /// flood). Returns sections whose sky-light changed.
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

        let cpos = ChunkPos::new(div_floor(x, 16), div_floor(z, 16));
        let section_ys: Vec<i32> = match self.chunks.get(&cpos) {
            Some(chunk) => chunk.sections().map(|s| s.y()).collect(),
            None => return Vec::new(),
        };
        let Some(&top) = section_ys.iter().max() else {
            return Vec::new();
        };
        let top_y = top * 16 + 15;

        // Heightmap (vanilla: the topmost block with any light opacity) after
        // the edit, and reconstructed as it was before by substituting `old`
        // at the edited height. -1 means a fully open column.
        let heightmap = |world: &Self, replace: Option<(i32, BlockState)>| -> i32 {
            let mut vy = top_y;
            while vy >= 0 {
                if section_ys.contains(&vy.div_euclid(16)) {
                    let block = match replace {
                        Some((ry, rb)) if ry == vy => rb,
                        _ => world.block_at(x, vy, z),
                    };
                    if block.light_opacity() > 0 {
                        return vy;
                    }
                }
                vy -= 1;
            }
            -1
        };
        let h_new = heightmap(self, None);
        let h_old = heightmap(self, Some((y, old)));

        // --- Cells whose direct-sky status flipped ---------------------------
        if h_new < h_old {
            // Column opened: (h_new, h_old] gains direct sky.
            for vy in (h_new + 1)..=h_old {
                if !section_ys.contains(&vy.div_euclid(16)) {
                    continue;
                }
                let (block_l, sky) = self.light_at(x, vy, z);
                if sky != 15 {
                    self.set_light(x, vy, z, block_l, 15);
                    insert_section_borders(x, vy, z, &mut changed);
                    additions.push_back((x, vy, z));
                }
            }
        } else if h_new > h_old {
            // Column covered: (h_old, h_new] loses direct sky; the BFS re-lights
            // it from whatever still reaches it.
            for vy in (h_old + 1)..=h_new {
                if !section_ys.contains(&vy.div_euclid(16)) {
                    continue;
                }
                let sky = self.light_at(x, vy, z).1;
                if sky > 0 {
                    self.set_sky_light_tracked(x, vy, z, 0, &mut changed);
                    removal.push_back((x, vy, z, sky));
                }
            }
        }

        // --- The edited cell itself ------------------------------------------
        // Below the heightmap its stored light may be stale for the new opacity
        // (e.g. water placed or removed under the surface): invalidate it and
        // let the removal frontier + re-add BFS recompute it from neighbours.
        if y <= h_new && old.light_opacity() != self.block_at(x, y, z).light_opacity() {
            let sky = self.light_at(x, y, z).1;
            if sky > 0 {
                self.set_sky_light_tracked(x, y, z, 0, &mut changed);
                removal.push_back((x, y, z, sky));
            }
            // A transparency increase can only be re-lit from lit neighbours —
            // seed them explicitly (the removal frontier may be empty).
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
                // Every propagation step costs >= 1, so anything we lit is
                // strictly dimmer; equal-or-brighter borders re-seed instead.
                if nl < level {
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
                let Some(cost) = self.light_cost(nx, ny, nz) else {
                    continue;
                };
                let new_level = level.saturating_sub(cost);
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
    fn sky_light_decays_downward_below_heightmap() {
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
        // Below the heightmap every step costs max(1, opacity) — including
        // straight down (vanilla getRawLight has no free downward pass; only
        // the direct-sky column above the heightmap stays 15).
        assert_eq!(world.light_at(1, 68, 0).1, 13, "downward decays by 1");
        assert_eq!(world.light_at(1, 67, 0).1, 12);
        assert_eq!(world.light_at(1, 66, 0).1, 11);
    }

    #[test]
    fn water_attenuates_skylight_like_vanilla() {
        let mut world = World::new();
        let water = BlockState::new(9, 0);
        // A walled 1×1 pond: sand floor at y=59, water y=60..=63, stone walls
        // around so the only light path is straight down from the sky.
        world.set_block(0, 59, 0, BlockState::new(12, 0));
        for wy in 60..=63 {
            world.set_block(0, wy, 0, water);
            for (wx, wz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                world.set_block(wx, wy, wz, BlockState::STONE);
            }
        }
        // Allocate the section above the surface (worlds always have their
        // above-surface sections allocated; the BFS seeds only scan allocated
        // sections).
        world.set_block(0, 64, 0, BlockState::AIR);
        world.recompute_vertical_skylight();

        assert_eq!(world.light_at(0, 64, 0).1, 15, "air above the surface");
        assert_eq!(world.light_at(0, 63, 0).1, 12, "each water block costs 3");
        assert_eq!(world.light_at(0, 62, 0).1, 9);
        assert_eq!(world.light_at(0, 61, 0).1, 6);
        assert_eq!(world.light_at(0, 60, 0).1, 3);
        assert_eq!(world.light_at(0, 59, 0).1, 0, "floor absorbs the rest");
    }

    #[test]
    fn local_relight_reproduces_vanilla_water_values() {
        // The bug this guards: server-decoded vanilla light was correct, but a
        // block edit re-ran the (then water-transparent) local relight and
        // visibly brightened the water column. The relight must reproduce the
        // exact vanilla attenuation so a "refresh" changes nothing.
        let mut world = World::new();
        let water = BlockState::new(9, 0);
        world.set_block(0, 59, 0, BlockState::new(12, 0));
        for wy in 60..=63 {
            world.set_block(0, wy, 0, water);
            for (wx, wz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                world.set_block(wx, wy, wz, BlockState::STONE);
            }
        }
        world.set_block(0, 64, 0, BlockState::AIR); // allocate the section above
        world.recompute_vertical_skylight();

        // Roof the pond column, then remove the roof again — both edits re-run
        // the incremental relight over the whole column.
        world.set_block(0, 70, 0, BlockState::STONE);
        world.update_sky_light(0, 70, 0, BlockState::AIR);
        world.set_block(0, 70, 0, BlockState::AIR);
        world.update_sky_light(0, 70, 0, BlockState::STONE);

        assert_eq!(world.light_at(0, 64, 0).1, 15);
        assert_eq!(world.light_at(0, 63, 0).1, 12, "unchanged after relight");
        assert_eq!(world.light_at(0, 62, 0).1, 9);
        assert_eq!(world.light_at(0, 61, 0).1, 6);
        assert_eq!(world.light_at(0, 60, 0).1, 3);
        assert_eq!(world.light_at(0, 59, 0).1, 0);
    }

    #[test]
    fn digging_the_pond_floor_stays_local_and_vanilla() {
        // Digging a block under water must (a) light the new cell with the
        // vanilla attenuated value and (b) leave the rest of the water column
        // untouched — no whole-column rewrite.
        let mut world = World::new();
        let water = BlockState::new(9, 0);
        let sand = BlockState::new(12, 0);
        world.set_block(0, 58, 0, BlockState::STONE);
        world.set_block(0, 59, 0, sand);
        for wy in 60..=63 {
            world.set_block(0, wy, 0, water);
            for (wx, wz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                world.set_block(wx, 59, wz, BlockState::STONE);
                world.set_block(wx, wy, wz, BlockState::STONE);
            }
        }
        world.set_block(0, 64, 0, BlockState::AIR); // allocate the section above
        world.recompute_vertical_skylight();
        assert_eq!(world.light_at(0, 60, 0).1, 3, "deepest water before dig");

        // Dig the sand floor: the new cavity is lit only through the water.
        world.set_block(0, 59, 0, BlockState::AIR);
        world.update_sky_light(0, 59, 0, sand);

        assert_eq!(world.light_at(0, 59, 0).1, 2, "cavity: 3 - max(1, air)");
        assert_eq!(world.light_at(0, 63, 0).1, 12, "water column unchanged");
        assert_eq!(world.light_at(0, 62, 0).1, 9);
        assert_eq!(world.light_at(0, 61, 0).1, 6);
        assert_eq!(world.light_at(0, 60, 0).1, 3);
    }

    #[test]
    fn torch_light_attenuates_through_water() {
        let mut world = World::new();
        // Emitter at one end of a walled water corridor: block light should
        // fall off by 3 per water block (max(1, opacity)), not 1.
        let water = BlockState::new(9, 0);
        world.set_block(8, 8, 8, BlockState::AIR); // load the chunk
        for wx in 9..=12 {
            world.set_block(wx, 8, 8, water);
        }
        let glowstone = BlockState::new(89, 0);
        world.set_block(8, 8, 8, glowstone);
        world.update_block_light(8, 8, 8, BlockState::AIR);

        assert_eq!(world.light_at(8, 8, 8).0, 15, "emitter");
        assert_eq!(world.light_at(9, 8, 8).0, 12, "first water block costs 3");
        assert_eq!(world.light_at(10, 8, 8).0, 9);
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
    fn block_entity_index_tracks_mutations_loads_and_unloads() {
        let mut world = World::new();
        let collect = |w: &World| {
            let mut v: Vec<[i32; 3]> = w.block_entities().map(|(p, _)| *p).collect();
            v.sort();
            v
        };

        // set_block on a block-entity id indexes it; a non-entity id does not.
        world.set_block(3, 64, 5, BlockState::new(63, 4)); // standing sign
        world.set_block(3, 65, 5, BlockState::STONE);
        assert_eq!(collect(&world), vec![[3, 64, 5]]);
        assert_eq!(
            world.block_entities().next().map(|(_, b)| *b),
            Some(BlockState::new(63, 4))
        );

        // Overwriting the sign with a non-entity block drops it from the index.
        world.set_block(3, 64, 5, BlockState::AIR);
        assert!(world.block_entities().next().is_none());

        // A bulk section load scans for block-entities (chest at local 5,6,7).
        let mut blocks = vec![0u16; 4096];
        blocks[6 * 256 + 7 * 16 + 5] = (54u16 << 4) | 2; // chest, meta 2
        world.load_section(2, 3, 0, &blocks, &[], &[]);
        let chest = [2 * 16 + 5, 6, 3 * 16 + 7];
        assert_eq!(collect(&world), vec![chest]);

        // Unloading the chunk clears its block-entities.
        world.remove_chunk(ChunkPos::new(2, 3));
        assert!(world.block_entities().next().is_none());
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
