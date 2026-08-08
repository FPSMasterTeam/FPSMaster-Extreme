//! Vanilla 1.8.9 biome colouring: the per-biome climate table, the
//! `grass.png` / `foliage.png` colormap lookup, and the biomes that override the
//! result outright.
//!
//! Vanilla stores grass and foliage as greyscale textures and colours them at
//! render time from a 256×256 colormap indexed by the biome's temperature and
//! rainfall (`ColorizerGrass.getGrassColor`). A handful of biomes ignore the
//! colormap and return a fixed colour instead (mesa), or post-process it (roofed
//! forest), or use their own value (swampland).

/// Climate + colour overrides for one biome id.
#[derive(Debug, Clone, Copy)]
pub struct BiomeInfo {
    /// `BiomeGenBase.temperature`.
    pub temperature: f32,
    /// `BiomeGenBase.rainfall`.
    pub downfall: f32,
    /// Fixed grass colour, bypassing the colormap (mesa family).
    pub grass_override: Option<[u8; 3]>,
    /// Fixed foliage colour, bypassing the colormap (mesa, swampland).
    pub foliage_override: Option<[u8; 3]>,
    /// `BiomeGenBase.waterColorMultiplier` — white for every biome but swamp.
    pub water: [u8; 3],
    /// Roofed forest averages its colormap grass with a dark green
    /// (`(i & 0xFEFEFE) + 2634762 >> 1`).
    pub dark_forest_blend: bool,
}

const WHITE: [u8; 3] = [255, 255, 255];

/// Swampland's grass/foliage (`6975545`).
///
/// Vanilla's swamp GRASS is two-valued — `GRASS_COLOR_NOISE < -0.1 ? 0x4C763C :
/// 0x6A7039` — which mottles a swamp roughly half and half, since the threshold
/// sits just below the noise field's midpoint. Only the FOLIAGE is
/// unconditionally this value. Using one constant for both is a deliberate
/// approximation: it costs the mottling, not the hue.
const SWAMP_FOLIAGE: [u8; 3] = [0x6A, 0x70, 0x39];
/// `BiomeGenSwamp.waterColorMultiplier` = 14745518.
const SWAMP_WATER: [u8; 3] = [0xE0, 0xFF, 0xAE];
/// `BiomeGenMesa.getGrassColorAtPos` = 9470285.
const MESA_GRASS: [u8; 3] = [0x90, 0x81, 0x4D];
/// `BiomeGenMesa.getFoliageColorAtPos` = 10387789.
const MESA_FOLIAGE: [u8; 3] = [0x9E, 0x81, 0x4D];
/// Roofed forest's blend partner, `2634762`.
const DARK_FOREST_TINT: [u8; 3] = [0x28, 0x34, 0x0A];

const fn plain(temperature: f32, downfall: f32) -> BiomeInfo {
    BiomeInfo {
        temperature,
        downfall,
        grass_override: None,
        foliage_override: None,
        water: WHITE,
        dark_forest_blend: false,
    }
}

const fn mesa() -> BiomeInfo {
    BiomeInfo {
        temperature: 2.0,
        downfall: 0.0,
        grass_override: Some(MESA_GRASS),
        foliage_override: Some(MESA_FOLIAGE),
        water: WHITE,
        dark_forest_blend: false,
    }
}

const fn swamp() -> BiomeInfo {
    BiomeInfo {
        temperature: 0.8,
        downfall: 0.9,
        grass_override: Some(SWAMP_FOLIAGE),
        foliage_override: Some(SWAMP_FOLIAGE),
        water: SWAMP_WATER,
        dark_forest_blend: false,
    }
}

const fn roofed_forest() -> BiomeInfo {
    BiomeInfo {
        temperature: 0.7,
        downfall: 0.8,
        grass_override: None,
        foliage_override: None,
        water: WHITE,
        dark_forest_blend: true,
    }
}

/// Whether a biome gets weather at all.
///
/// Vanilla tracks this as `enableRain`, cleared by `setDisableRain()` on desert,
/// savanna, mesa, the Nether and the End. That is *almost* exactly the set whose
/// rainfall is 0, so deriving it from the climate table avoids maintaining a
/// second hand-written list — with one special case, below.
pub fn precipitates(id: u8) -> bool {
    // The End is the one biome where the rainfall derivation fails: `BiomeGenEnd`
    // never calls `setTemperatureRainfall`, so it keeps the 0.5/0.5 defaults, yet
    // it also calls `setDisableRain()`. Every other no-precipitation biome
    // (desert, savanna, mesa, the Nether) does have rainfall 0.
    if id == 9 {
        return false;
    }
    biome_info(id).downfall > 0.0
}

/// Vanilla `BiomeGenBase.getFloatTemperature`: above y=64 the temperature falls
/// off with altitude, which is what puts snow on mountain tops inside an
/// otherwise rainy biome.
///
/// Vanilla adds a noise term to blur the snow line; this is the deterministic
/// part only, so the transition is a clean contour rather than a ragged one.
pub fn temperature_at(id: u8, y: i32) -> f32 {
    let base = biome_info(id).temperature;
    if y > 64 {
        base - (y - 64) as f32 * 0.05 / 30.0
    } else {
        base
    }
}

/// Below this temperature a column gets snow instead of rain
/// (`renderRainSnow`'s `>= 0.15F` test).
pub const SNOW_TEMPERATURE: f32 = 0.15;

/// Climate for a 1.8.9 biome id. Ids the client does not know fall back to
/// plains, matching how vanilla resolves an unregistered biome.
pub fn biome_info(id: u8) -> BiomeInfo {
    match id {
        0 => plain(0.5, 0.5),          // Ocean
        1 => plain(0.8, 0.4),          // Plains
        2 => plain(2.0, 0.0),          // Desert
        3 => plain(0.2, 0.3),          // Extreme Hills
        4 => plain(0.7, 0.8),          // Forest
        5 => plain(0.25, 0.8),         // Taiga
        6 => swamp(),                  // Swampland
        7 => plain(0.5, 0.5),          // River
        8 => plain(2.0, 0.0),          // Hell
        9 => plain(0.5, 0.5),          // The End
        10 => plain(0.0, 0.5),         // Frozen Ocean
        11 => plain(0.0, 0.5),         // Frozen River
        12 => plain(0.0, 0.5),         // Ice Plains
        13 => plain(0.0, 0.5),         // Ice Mountains
        14 => plain(0.9, 1.0),         // Mushroom Island
        15 => plain(0.9, 1.0),         // Mushroom Island Shore
        16 => plain(0.8, 0.4),         // Beach
        17 => plain(2.0, 0.0),         // Desert Hills
        18 => plain(0.7, 0.8),         // Forest Hills
        19 => plain(0.25, 0.8),        // Taiga Hills
        20 => plain(0.2, 0.3),         // Extreme Hills Edge
        21 => plain(0.95, 0.9),        // Jungle
        22 => plain(0.95, 0.9),        // Jungle Hills
        23 => plain(0.95, 0.8),        // Jungle Edge
        24 => plain(0.5, 0.5),         // Deep Ocean
        25 => plain(0.2, 0.3),         // Stone Beach
        26 => plain(0.05, 0.3),        // Cold Beach
        27 => plain(0.6, 0.6),         // Birch Forest
        28 => plain(0.6, 0.6),         // Birch Forest Hills
        29 => roofed_forest(),         // Roofed Forest
        30 => plain(-0.5, 0.4),        // Cold Taiga
        31 => plain(-0.5, 0.4),        // Cold Taiga Hills
        32 => plain(0.3, 0.8),         // Mega Taiga
        33 => plain(0.3, 0.8),         // Mega Taiga Hills
        34 => plain(0.2, 0.3),         // Extreme Hills+
        35 => plain(1.2, 0.0),         // Savanna
        36 => plain(1.0, 0.0),         // Savanna Plateau
        37..=39 => mesa(),             // Mesa / Mesa Plateau F / Mesa Plateau
        // Mutated variants (base id + 128) reuse their base climate.
        129 => plain(0.8, 0.4),        // Sunflower Plains
        130 => plain(2.0, 0.0),        // Desert M
        131 => plain(0.2, 0.3),        // Extreme Hills M
        132 => plain(0.7, 0.8),        // Flower Forest
        133 => plain(0.25, 0.8),       // Taiga M
        134 => swamp(),                // Swampland M
        140 => plain(0.0, 0.5),        // Ice Plains Spikes
        149 => plain(0.95, 0.9),       // Jungle M
        151 => plain(0.95, 0.8),       // Jungle Edge M
        155 => plain(0.6, 0.6),        // Birch Forest M
        156 => plain(0.6, 0.6),        // Birch Forest Hills M
        157 => roofed_forest(),        // Roofed Forest M
        158 => plain(-0.5, 0.4),       // Cold Taiga M
        160 => plain(0.25, 0.8),       // Mega Spruce Taiga
        161 => plain(0.25, 0.8),       // Redwood Taiga Hills M
        162 => plain(0.2, 0.3),        // Extreme Hills+ M
        // Savanna M is NOT a plain copy: `BiomeGenSavanna.createMutatedBiome`
        // sets `temperature = (base + 1.0) * 0.5`, so 1.2 becomes 1.1. (Savanna
        // Plateau M keeps 1.0 only because 1.0 is that formula's fixed point.)
        163 => plain(1.1, 0.0),        // Savanna M
        164 => plain(1.0, 0.0),        // Savanna Plateau M
        165..=167 => mesa(),           // Mesa Bryce / Plateau F M / Plateau M
        _ => plain(0.8, 0.4),          // unknown -> plains
    }
}

/// A decoded 256×256 vanilla colormap (`grass.png` / `foliage.png`).
#[derive(Debug, Clone)]
pub struct Colormap {
    /// Row-major RGB, 256×256.
    pixels: Vec<[u8; 3]>,
    width: u32,
    height: u32,
}

impl Colormap {
    pub fn new(pixels: Vec<[u8; 3]>, width: u32, height: u32) -> Self {
        Self {
            pixels,
            width,
            height,
        }
    }

    /// Vanilla `ColorizerGrass.getGrassColor`: rainfall is scaled by temperature,
    /// then both are inverted into a 0..255 index. Out-of-range climates clamp,
    /// matching `BiomeGenBase.getGrassColorAtPos`, which runs both through
    /// `MathHelper.clamp_float(_, 0, 1)` before the lookup. (There is no index
    /// masking in vanilla — `ColorizerFoliage` has no bounds guard at all.)
    pub fn sample(&self, temperature: f32, downfall: f32) -> [u8; 3] {
        if self.pixels.is_empty() {
            return [255, 255, 255];
        }
        // Vanilla widens the biome's f32 climate to double before indexing, and
        // the truncation is load-bearing: plains' 0.8f widens to 0.800000011…,
        // so `(1 - t) * 255` lands a hair under 51 and floors to 50. Doing this
        // in f32 happens to agree here, but f64 keeps it exact everywhere.
        let temperature = temperature.clamp(0.0, 1.0) as f64;
        let downfall = downfall.clamp(0.0, 1.0) as f64 * temperature;
        let x = (((1.0 - temperature) * 255.0) as u32).min(self.width.saturating_sub(1));
        let y = (((1.0 - downfall) * 255.0) as u32).min(self.height.saturating_sub(1));
        self.pixels[(y * self.width + x) as usize]
    }
}

/// The pair of colormaps plus the resolved per-biome lookups the mesher needs.
#[derive(Debug, Clone)]
pub struct BiomeColorTable {
    grass: Vec<[u8; 3]>,
    foliage: Vec<[u8; 3]>,
    water: Vec<[u8; 3]>,
}

impl BiomeColorTable {
    /// Resolve every biome id up front — 256 colormap lookups once, instead of
    /// one per block face during meshing.
    pub fn build(grass_map: &Colormap, foliage_map: &Colormap) -> Self {
        let mut grass = Vec::with_capacity(256);
        let mut foliage = Vec::with_capacity(256);
        let mut water = Vec::with_capacity(256);
        for id in 0..=255u8 {
            let info = biome_info(id);
            let mut g = info
                .grass_override
                .unwrap_or_else(|| grass_map.sample(info.temperature, info.downfall));
            if info.dark_forest_blend {
                // Vanilla: `(i & 0xFEFEFE) + 2634762 >> 1`, i.e. average with the
                // dark-forest tint after dropping each channel's low bit.
                g = [
                    (((g[0] & 0xFE) as u16 + DARK_FOREST_TINT[0] as u16) >> 1) as u8,
                    (((g[1] & 0xFE) as u16 + DARK_FOREST_TINT[1] as u16) >> 1) as u8,
                    (((g[2] & 0xFE) as u16 + DARK_FOREST_TINT[2] as u16) >> 1) as u8,
                ];
            }
            let f = info
                .foliage_override
                .unwrap_or_else(|| foliage_map.sample(info.temperature, info.downfall));
            grass.push(g);
            foliage.push(f);
            water.push(info.water);
        }
        Self {
            grass,
            foliage,
            water,
        }
    }

    /// A table with no colormap available — every biome renders the fallback
    /// plains colour, matching the old global-constant behaviour.
    pub fn fallback(grass: [u8; 3], foliage: [u8; 3]) -> Self {
        Self {
            grass: vec![grass; 256],
            foliage: vec![foliage; 256],
            water: (0..=255u8).map(|id| biome_info(id).water).collect(),
        }
    }

    pub fn grass(&self, biome: u8) -> [u8; 3] {
        self.grass[biome as usize]
    }

    pub fn foliage(&self, biome: u8) -> [u8; 3] {
        self.foliage[biome as usize]
    }

    pub fn water(&self, biome: u8) -> [u8; 3] {
        self.water[biome as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> Colormap {
        // A colormap whose red channel encodes x and green encodes y, so a sample
        // reveals exactly which texel the climate mapped to.
        let mut pixels = Vec::with_capacity(256 * 256);
        for y in 0..256u32 {
            for x in 0..256u32 {
                pixels.push([x as u8, y as u8, 0]);
            }
        }
        Colormap::new(pixels, 256, 256)
    }

    #[test]
    fn colormap_lookup_matches_vanilla_indexing() {
        let map = ramp();
        // Plains: temperature 0.8, downfall 0.4 -> rainfall 0.32. x floors to 50,
        // not 51, because 0.8f widens to 0.800000011… and `(1 - t) * 255` lands
        // just under 51 — vanilla truncates the same way.
        assert_eq!(map.sample(0.8, 0.4), [50, 173, 0]);
        // Desert: temperature clamps to 1.0, rainfall 0 -> the hot/dry corner.
        assert_eq!(map.sample(2.0, 0.0), [0, 255, 0]);
        // Cold taiga: negative temperature clamps to 0 -> the cold corner.
        assert_eq!(map.sample(-0.5, 0.4), [255, 255, 0]);
    }

    #[test]
    fn biomes_that_ignore_the_colormap_keep_their_fixed_colour() {
        let table = BiomeColorTable::build(&ramp(), &ramp());
        // Mesa is a flat colour regardless of the colormap.
        assert_eq!(table.grass(37), MESA_GRASS);
        assert_eq!(table.foliage(165), MESA_FOLIAGE);
        // Swamp overrides both and is the only biome with tinted water.
        assert_eq!(table.grass(6), SWAMP_FOLIAGE);
        assert_eq!(table.water(6), SWAMP_WATER);
        assert_eq!(table.water(1), WHITE, "every other biome leaves water alone");
        // Roofed forest darkens whatever the colormap returned.
        let plain_forest = table.grass(4);
        let roofed = table.grass(29);
        assert!(
            roofed[1] < plain_forest[1],
            "roofed forest blends toward the dark tint: {roofed:?} vs {plain_forest:?}"
        );
    }

    #[test]
    fn distinct_climates_give_distinct_colours() {
        let table = BiomeColorTable::build(&ramp(), &ramp());
        let plains = table.grass(1);
        let taiga = table.grass(5);
        let desert = table.grass(2);
        let jungle = table.grass(21);
        assert_ne!(plains, taiga);
        assert_ne!(plains, desert);
        assert_ne!(plains, jungle);
    }

    /// Both of these were wrong until the table was checked against decompiled
    /// 1.8.9 source — and both looked entirely plausible, which is the point.
    #[test]
    fn the_two_cases_the_climate_table_got_wrong() {
        // `BiomeGenSavanna.createMutatedBiome` halves the temperature toward 1.0
        // rather than copying it, so Savanna M is 1.1, not the base 1.2.
        assert_eq!(biome_info(163).temperature, 1.1);
        assert_eq!(biome_info(35).temperature, 1.2, "the base is unchanged");
        // Savanna Plateau M lands on 1.0 either way — 1.0 is the formula's fixed
        // point — so it is right by luck, not by the rule.
        assert_eq!(biome_info(164).temperature, 1.0);

        // The End keeps the 0.5/0.5 climate defaults yet still calls
        // setDisableRain(), so it is the one biome `downfall > 0` gets wrong.
        assert_eq!(biome_info(9).downfall, 0.5);
        assert!(!precipitates(9), "The End has no weather");
        // Every other dry biome really does have rainfall 0.
        for id in [2u8, 8, 17, 35, 36, 37, 38, 39, 130, 163, 164, 165, 166, 167] {
            assert!(!precipitates(id), "biome {id} should be dry");
        }
        // And the wet ones still are.
        for id in [0u8, 1, 4, 5, 6, 12, 21, 29] {
            assert!(precipitates(id), "biome {id} should get weather");
        }
    }

    #[test]
    fn unknown_ids_fall_back_to_plains() {
        assert_eq!(biome_info(200).temperature, biome_info(1).temperature);
        assert_eq!(biome_info(200).downfall, biome_info(1).downfall);
    }
}
