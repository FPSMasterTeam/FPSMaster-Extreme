use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use image::{imageops::FilterType, DynamicImage, GenericImage, Rgba, RgbaImage};
use recraft_core::registry;
use zip::ZipArchive;

pub const TILE_SIZE: u32 = 16;
pub const ATLAS_COLUMNS: u32 = 16;

// Plains colormap point: temperature 0.8, downfall 0.4 (rainfall is multiplied
// by temperature before lookup). Used to sample grass.png / foliage.png.
const PLAINS_TEMPERATURE: f64 = 0.8;
const PLAINS_DOWNFALL: f64 = 0.4;
const DEFAULT_GRASS: [u8; 3] = [0x91, 0xBD, 0x59];
const DEFAULT_FOLIAGE: [u8; 3] = [0x77, 0xAB, 0x2F];

const GRASS_SIDE_OVERLAY: &str = "assets/minecraft/textures/blocks/grass_side_overlay.png";
const GRASS_COLORMAP: &str = "assets/minecraft/textures/colormap/grass.png";
const FOLIAGE_COLORMAP: &str = "assets/minecraft/textures/colormap/foliage.png";

/// Maps texture base-names to atlas tile indices and yields UVs. Index 0 is the
/// magenta "missing" tile, so unknown/missing textures are visually obvious.
#[derive(Debug, Clone, Default)]
pub struct AtlasUv {
    name_to_index: HashMap<String, u32>,
    rows: u32,
    /// Tile indices whose texture failed to load (magenta placeholders).
    missing: HashSet<u32>,
    /// The atlas RGBA pixels (Arc-shared so clones stay cheap), used to read a
    /// sprite tile's alpha mask for first-person item extrusion.
    pixels: Option<Arc<Vec<u8>>>,
}

impl AtlasUv {
    pub fn uv(&self, name: Option<&str>) -> [[f32; 2]; 4] {
        tile_uv(self.tile_index(name), self.rows)
    }

    /// Atlas tile index for a texture base-name (0 = the magenta "missing"
    /// tile). Used by the UI to blit a block's tile as an item thumbnail.
    pub fn tile_index(&self, name: Option<&str>) -> u32 {
        name.and_then(|name| self.name_to_index.get(name).copied())
            .unwrap_or(0)
    }

    /// Whether this name resolves to the magenta missing tile — either no
    /// name / a name absent from the atlas (index 0), or a mapped tile whose
    /// texture file failed to load.
    pub fn is_missing_tile(&self, name: Option<&str>) -> bool {
        let index = self.tile_index(name);
        index == 0 || self.missing.contains(&index)
    }

    /// Normalized `(u0, v0, width, height)` of a tile, for sub-tile UV
    /// mapping (partial-block faces crop the texture by their box extent).
    pub fn tile_rect(&self, name: Option<&str>) -> [f32; 4] {
        let index = self.tile_index(name);
        let atlas_w = (ATLAS_COLUMNS * TILE_SIZE) as f32;
        let atlas_h = (self.rows.max(1) * TILE_SIZE) as f32;
        [
            (index % ATLAS_COLUMNS * TILE_SIZE) as f32 / atlas_w,
            (index / ATLAS_COLUMNS * TILE_SIZE) as f32 / atlas_h,
            TILE_SIZE as f32 / atlas_w,
            TILE_SIZE as f32 / atlas_h,
        ]
    }

    /// The `TILE_SIZE`×`TILE_SIZE` opacity mask of a sprite tile in row-major
    /// order (`true` = opaque, alpha > 127, matching the cutout threshold).
    /// `None` when the name is unmapped (index 0) or the atlas pixels were not
    /// captured. Used by the first-person item renderer to extrude the sprite's
    /// silhouette edges.
    pub fn tile_alpha_mask(&self, name: Option<&str>) -> Option<Vec<bool>> {
        let pixels = self.pixels.as_ref()?;
        let index = self.tile_index(name);
        if index == 0 {
            return None;
        }
        let atlas_w = ATLAS_COLUMNS * TILE_SIZE;
        let ox = index % ATLAS_COLUMNS * TILE_SIZE;
        let oy = index / ATLAS_COLUMNS * TILE_SIZE;
        let mut mask = vec![false; (TILE_SIZE * TILE_SIZE) as usize];
        for ty in 0..TILE_SIZE {
            for tx in 0..TILE_SIZE {
                let p = (((oy + ty) * atlas_w + (ox + tx)) * 4) as usize;
                mask[(ty * TILE_SIZE + tx) as usize] = pixels.get(p + 3).copied().unwrap_or(0) > 127;
            }
        }
        Some(mask)
    }
}

/// The 16 vanilla dye color suffixes in meta order (stained glass/clay/wool).
pub const STAINED_COLORS: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "silver",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

#[derive(Debug, Clone)]
pub struct TextureAtlasImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub source: TextureAtlasSource,
    pub grass_color: [f32; 3],
    pub foliage_color: [f32; 3],
    name_to_index: HashMap<String, u32>,
    rows: u32,
    /// Tile indices left as magenta placeholders (texture file not found).
    missing_indices: Vec<u32>,
}

#[derive(Debug, Clone)]
pub enum TextureAtlasSource {
    Directory(PathBuf),
    Archive(PathBuf),
    Fallback,
}

impl TextureAtlasImage {
    /// UV lookup table for the mesher (cheap to clone — just the name map).
    pub fn uv_table(&self) -> AtlasUv {
        AtlasUv {
            name_to_index: self.name_to_index.clone(),
            rows: self.rows,
            // In pure-fallback mode every tile is a placeholder; per-block
            // missing-tile reports would only repeat the load warning.
            missing: if matches!(self.source, TextureAtlasSource::Fallback) {
                HashSet::new()
            } else {
                self.missing_indices.iter().copied().collect()
            },
            pixels: Some(Arc::new(self.pixels.clone())),
        }
    }

    /// Build the atlas from the block registry's texture names, loading each
    /// `blocks/<name>.png` from the first available asset source.
    pub fn load_default() -> Self {
        let mut names = registry().all_texture_names();
        // The mining crack overlays (destroy_stage_0..9) live alongside the
        // block textures; give them atlas tiles so the renderer can draw
        // breaking progress over the targeted block.
        for stage in 0..10 {
            names.push(format!("destroy_stage_{stage}"));
        }
        // Glass pane edge textures, used by the mesher for pane top/bottom
        // faces (the data file's face textures cover the panel sides).
        names.push("glass_pane_top".to_owned());
        for color in STAINED_COLORS {
            names.push(format!("glass_pane_top_{color}"));
        }
        // The recessed front face of an extended piston body (the mesher draws it
        // directly; it is not a per-face entry in any block def).
        names.push("piston_inner".to_owned());
        // Item sprites (under textures/items/) join the atlas so the
        // first-person item renderer can draw real held items; their atlas
        // names keep the "items/" prefix.
        let mut seen = std::collections::HashSet::new();
        for id in (256..432).chain(2256..2268) {
            if let Some(name) = item_texture_name(id) {
                if seen.insert(name) {
                    names.push(format!("items/{name}"));
                }
            }
        }
        for path in candidate_asset_paths() {
            match Self::from_asset_path(path.clone(), &names) {
                Ok(atlas) => return atlas,
                Err(err) => {
                    log::warn!("failed to load textures from {}: {err}", path.display())
                }
            }
        }
        log::warn!(
            "no Minecraft 1.8.9 assets found; using fallback atlas (run the asset setup script or pass --assets)"
        );
        Self::fallback(&names)
    }

    fn from_asset_path(path: PathBuf, names: &[String]) -> Result<Self, String> {
        if path.is_dir() {
            Self::from_asset_directory(path, names)
        } else {
            Self::from_asset_zip(path, names)
        }
    }

    fn from_asset_directory(path: PathBuf, names: &[String]) -> Result<Self, String> {
        let mut read = |asset: &str| read_directory_image(&path, asset);
        let (built, loaded) = build_atlas(names, &mut read);
        if loaded == 0 && !names.is_empty() {
            return Err("directory has no block textures".to_owned());
        }
        Ok(Self::from_built(built, TextureAtlasSource::Directory(path)))
    }

    fn from_asset_zip(path: PathBuf, names: &[String]) -> Result<Self, String> {
        let file = File::open(&path).map_err(|err| err.to_string())?;
        let mut zip = ZipArchive::new(file).map_err(|err| err.to_string())?;
        let mut read = |asset: &str| read_zip_image(&mut zip, asset);
        let (built, loaded) = build_atlas(names, &mut read);
        if loaded == 0 && !names.is_empty() {
            return Err("archive has no block textures".to_owned());
        }
        Ok(Self::from_built(built, TextureAtlasSource::Archive(path)))
    }

    fn fallback(names: &[String]) -> Self {
        let mut read = |_: &str| None;
        let (built, _) = build_atlas(names, &mut read);
        Self::from_built(built, TextureAtlasSource::Fallback)
    }

    fn from_built(built: BuiltAtlas, source: TextureAtlasSource) -> Self {
        Self {
            width: built.image.width(),
            height: built.image.height(),
            pixels: built.image.into_raw(),
            source,
            grass_color: built.grass_color,
            foliage_color: built.foliage_color,
            name_to_index: built.name_to_index,
            rows: built.rows,
            missing_indices: built.missing_indices,
        }
    }
}

struct BuiltAtlas {
    image: RgbaImage,
    name_to_index: HashMap<String, u32>,
    rows: u32,
    grass_color: [f32; 3],
    foliage_color: [f32; 3],
    missing_indices: Vec<u32>,
}

/// Place the missing-tile at index 0 then every named texture at index 1.., one
/// per 16×16 cell, loading each via `read` (which resolves an `assets/...` path
/// to an image; None leaves the magenta placeholder).
fn build_atlas(
    names: &[String],
    read: &mut dyn FnMut(&str) -> Option<DynamicImage>,
) -> (BuiltAtlas, usize) {
    let tile_count = names.len() as u32 + 1; // +1 for the missing tile at 0
    let rows = tile_count.div_ceil(ATLAS_COLUMNS).max(1);
    let mut image = RgbaImage::new(ATLAS_COLUMNS * TILE_SIZE, rows * TILE_SIZE);
    fill_missing(&mut image);

    let mut name_to_index = HashMap::with_capacity(names.len());
    let mut loaded = 0;
    let mut missing = Vec::new();
    let mut missing_indices = Vec::new();
    for (offset, name) in names.iter().enumerate() {
        let index = offset as u32 + 1;
        name_to_index.insert(name.clone(), index);
        // Plain names live under textures/blocks/; names with a path (e.g.
        // "items/diamond_sword") resolve relative to textures/.
        let asset = if name.contains('/') {
            format!("assets/minecraft/textures/{name}.png")
        } else {
            format!("assets/minecraft/textures/blocks/{name}.png")
        };
        if let Some(source) = read(&asset) {
            copy_tile(&mut image, index, source);
            loaded += 1;
        } else {
            put_missing(&mut image, index);
            missing.push(name.as_str());
            missing_indices.push(index);
        }
    }

    if loaded > 0 {
        log::info!("loaded {loaded}/{} block atlas tiles", names.len());
        if !missing.is_empty() {
            log::warn!(
                "missing {} block atlas textures; using purple/black fallback for: {}",
                missing.len(),
                missing.join(", ")
            );
        }
    }

    let grass = read(GRASS_COLORMAP)
        .map(|img| sample_plains_color(&img))
        .unwrap_or(DEFAULT_GRASS);
    let foliage = read(FOLIAGE_COLORMAP)
        .map(|img| sample_plains_color(&img))
        .unwrap_or(DEFAULT_FOLIAGE);
    if let (Some(&index), Some(overlay)) =
        (name_to_index.get("grass_side"), read(GRASS_SIDE_OVERLAY))
    {
        composite_grass_side(&mut image, index, &overlay, grass);
    }

    (
        BuiltAtlas {
            image,
            name_to_index,
            rows,
            grass_color: u8_to_unit(grass),
            foliage_color: u8_to_unit(foliage),
            missing_indices,
        },
        loaded,
    )
}

/// Log once per (block, meta, context) when a texture resolves to the magenta
/// missing tile, so broken texture wiring is easy to spot in the log without
/// flooding it on every chunk rebuild.
pub(crate) fn warn_missing_tile(block_id: u16, meta: u8, context: &str, name: Option<&str>) {
    static SEEN: OnceLock<Mutex<HashSet<(u16, u8, String)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if !seen.lock().expect("missing-tile log set poisoned").insert((
        block_id,
        meta,
        context.to_owned(),
    )) {
        return;
    }
    match name {
        Some(name) => log::warn!(
            "block {block_id}:{meta} renders the missing (magenta) tile for {context}: \
             texture '{name}' is not in the atlas"
        ),
        None => log::warn!(
            "block {block_id}:{meta} renders the missing (magenta) tile for {context}: \
             no texture mapped in blocks.json"
        ),
    }
}

fn tile_uv(index: u32, rows: u32) -> [[f32; 2]; 4] {
    let x = index % ATLAS_COLUMNS;
    let y = index / ATLAS_COLUMNS;
    let atlas_w = (ATLAS_COLUMNS * TILE_SIZE) as f32;
    let atlas_h = (rows * TILE_SIZE) as f32;
    let min_u = (x * TILE_SIZE) as f32 / atlas_w;
    let max_u = ((x + 1) * TILE_SIZE) as f32 / atlas_w;
    let min_v = (y * TILE_SIZE) as f32 / atlas_h;
    let max_v = ((y + 1) * TILE_SIZE) as f32 / atlas_h;
    [
        [min_u, max_v],
        [min_u, min_v],
        [max_u, min_v],
        [max_u, max_v],
    ]
}

fn tile_origin(index: u32) -> (u32, u32) {
    (
        index % ATLAS_COLUMNS * TILE_SIZE,
        index / ATLAS_COLUMNS * TILE_SIZE,
    )
}

fn copy_tile(atlas: &mut RgbaImage, index: u32, image: DynamicImage) {
    let tile = first_animation_frame(image)
        .resize_exact(TILE_SIZE, TILE_SIZE, FilterType::Nearest)
        .to_rgba8();
    let (x, y) = tile_origin(index);
    let _ = atlas.copy_from(&tile, x, y);
}

/// Animated textures ship as a vertical strip of square frames; use frame 0.
fn first_animation_frame(image: DynamicImage) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    if width > 0 && height > width && height % width == 0 {
        image.crop_imm(0, 0, width, width)
    } else {
        image
    }
}

fn fill_missing(atlas: &mut RgbaImage) {
    put_missing(atlas, 0);
}

fn put_missing(atlas: &mut RgbaImage, index: u32) {
    let (x0, y0) = tile_origin(index);
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let dark = ((x / 4) + (y / 4)) % 2 == 0;
            let color = if dark {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 0, 255, 255])
            };
            atlas.put_pixel(x0 + x, y0 + y, color);
        }
    }
}

fn u8_to_unit(rgb: [u8; 3]) -> [f32; 3] {
    [
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    ]
}

fn sample_plains_color(colormap: &DynamicImage) -> [u8; 3] {
    let image = colormap.to_rgba8();
    let (width, height) = image.dimensions();
    let rain = PLAINS_DOWNFALL * PLAINS_TEMPERATURE;
    let x = (((1.0 - PLAINS_TEMPERATURE) * 255.0) as u32).min(width.saturating_sub(1));
    let y = (((1.0 - rain) * 255.0) as u32).min(height.saturating_sub(1));
    let pixel = image.get_pixel(x, y);
    [pixel[0], pixel[1], pixel[2]]
}

fn composite_grass_side(atlas: &mut RgbaImage, index: u32, overlay: &DynamicImage, grass: [u8; 3]) {
    let overlay = overlay
        .resize_exact(TILE_SIZE, TILE_SIZE, FilterType::Nearest)
        .to_rgba8();
    let (x0, y0) = tile_origin(index);
    for ty in 0..TILE_SIZE {
        for tx in 0..TILE_SIZE {
            let over = overlay.get_pixel(tx, ty);
            let alpha = over[3] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            let base = atlas.get_pixel(x0 + tx, y0 + ty).0;
            let mut out = [0u8; 4];
            for c in 0..3 {
                let tinted = grass[c] as f32 * (over[c] as f32 / 255.0);
                out[c] = (base[c] as f32 * (1.0 - alpha) + tinted * alpha)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            out[3] = 255;
            atlas.put_pixel(x0 + tx, y0 + ty, Rgba(out));
        }
    }
}

/// Load a single GUI texture (e.g. "widgets", "icons") from any asset source.
pub fn load_gui_image(name: &str) -> Option<RgbaImage> {
    load_asset_image(&format!("assets/minecraft/textures/gui/{name}.png"))
}

/// Load the 6 panorama faces (panorama_0..5) for the title screen cubemap.
/// Returns `None` if any face is missing.
pub fn load_panorama_faces() -> Option<[RgbaImage; 6]> {
    let faces: Vec<RgbaImage> = (0..6)
        .map(|i| {
            load_asset_image(&format!(
                "assets/minecraft/textures/gui/title/background/panorama_{i}.png"
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(std::array::from_fn(|i| faces[i].clone()))
}

/// 1.8 numeric item id → texture base-name under `textures/items/`. Metadata
/// variants (dye/fish/record subtypes) collapse to the base item. None means we
/// don't have a thumbnail for it (the UI then draws a tint swatch).
pub fn item_texture_name(id: i16) -> Option<&'static str> {
    let name = match id {
        256 => "iron_shovel",
        257 => "iron_pickaxe",
        258 => "iron_axe",
        259 => "flint_and_steel",
        260 => "apple",
        261 => "bow_standby",
        262 => "arrow",
        263 => "coal",
        264 => "diamond",
        265 => "iron_ingot",
        266 => "gold_ingot",
        267 => "iron_sword",
        268 => "wood_sword",
        269 => "wood_shovel",
        270 => "wood_pickaxe",
        271 => "wood_axe",
        272 => "stone_sword",
        273 => "stone_shovel",
        274 => "stone_pickaxe",
        275 => "stone_axe",
        276 => "diamond_sword",
        277 => "diamond_shovel",
        278 => "diamond_pickaxe",
        279 => "diamond_axe",
        280 => "stick",
        281 => "bowl",
        282 => "mushroom_stew",
        283 => "gold_sword",
        284 => "gold_shovel",
        285 => "gold_pickaxe",
        286 => "gold_axe",
        287 => "string",
        288 => "feather",
        289 => "gunpowder",
        290 => "wood_hoe",
        291 => "stone_hoe",
        292 => "iron_hoe",
        293 => "diamond_hoe",
        294 => "gold_hoe",
        295 => "seeds_wheat",
        296 => "wheat",
        297 => "bread",
        298 => "leather_helmet",
        299 => "leather_chestplate",
        300 => "leather_leggings",
        301 => "leather_boots",
        302 => "chainmail_helmet",
        303 => "chainmail_chestplate",
        304 => "chainmail_leggings",
        305 => "chainmail_boots",
        306 => "iron_helmet",
        307 => "iron_chestplate",
        308 => "iron_leggings",
        309 => "iron_boots",
        310 => "diamond_helmet",
        311 => "diamond_chestplate",
        312 => "diamond_leggings",
        313 => "diamond_boots",
        314 => "gold_helmet",
        315 => "gold_chestplate",
        316 => "gold_leggings",
        317 => "gold_boots",
        318 => "flint",
        319 => "porkchop_raw",
        320 => "porkchop_cooked",
        321 => "painting",
        322 => "apple_golden",
        323 => "sign",
        324 => "door_wood",
        325 => "bucket_empty",
        326 => "bucket_water",
        327 => "bucket_lava",
        328 => "minecart_normal",
        329 => "saddle",
        330 => "door_iron",
        331 => "redstone_dust",
        332 => "snowball",
        333 => "boat",
        334 => "leather",
        335 => "bucket_milk",
        336 => "brick",
        337 => "clay_ball",
        338 => "reeds",
        339 => "paper",
        340 => "book_normal",
        341 => "slimeball",
        342 => "minecart_chest",
        343 => "minecart_furnace",
        344 => "egg",
        345 => "compass",
        346 => "fishing_rod_uncast",
        347 => "clock",
        348 => "glowstone_dust",
        349 => "fish_cod_raw",
        350 => "fish_cod_cooked",
        351 => "dye_powder_black",
        352 => "bone",
        353 => "sugar",
        354 => "cake",
        355 => "bed",
        356 => "repeater",
        357 => "cookie",
        358 => "map_filled",
        359 => "shears",
        360 => "melon",
        361 => "seeds_pumpkin",
        362 => "seeds_melon",
        363 => "beef_raw",
        364 => "beef_cooked",
        365 => "chicken_raw",
        366 => "chicken_cooked",
        367 => "rotten_flesh",
        368 => "ender_pearl",
        369 => "blaze_rod",
        370 => "ghast_tear",
        371 => "gold_nugget",
        372 => "nether_wart",
        373 => "potion_bottle_drinkable",
        374 => "potion_bottle_empty",
        375 => "spider_eye",
        376 => "spider_eye_fermented",
        377 => "blaze_powder",
        378 => "magma_cream",
        379 => "brewing_stand",
        380 => "cauldron",
        381 => "ender_eye",
        382 => "melon_speckled",
        383 => "spawn_egg",
        384 => "experience_bottle",
        385 => "fireball",
        386 => "book_writable",
        387 => "book_written",
        388 => "emerald",
        389 => "item_frame",
        390 => "flower_pot",
        391 => "carrot",
        392 => "potato",
        393 => "potato_baked",
        394 => "potato_poisonous",
        395 => "map_empty",
        396 => "carrot_golden",
        398 => "carrot_on_a_stick",
        399 => "nether_star",
        400 => "pumpkin_pie",
        401 => "fireworks",
        402 => "fireworks_charge",
        403 => "book_enchanted",
        404 => "comparator",
        405 => "netherbrick",
        406 => "quartz",
        407 => "minecart_tnt",
        408 => "minecart_hopper",
        409 => "prismarine_shard",
        410 => "prismarine_crystals",
        411 => "rabbit_raw",
        412 => "rabbit_cooked",
        413 => "rabbit_stew",
        414 => "rabbit_foot",
        415 => "rabbit_hide",
        416 => "wooden_armorstand",
        417 => "iron_horse_armor",
        418 => "gold_horse_armor",
        419 => "diamond_horse_armor",
        420 => "lead",
        421 => "name_tag",
        422 => "minecart_command_block",
        423 => "mutton_raw",
        424 => "mutton_cooked",
        425 => "banner_base",
        427 => "door_spruce",
        428 => "door_birch",
        429 => "door_jungle",
        430 => "door_acacia",
        431 => "door_dark_oak",
        2257 => "record_cat",
        2258 => "record_blocks",
        2259 => "record_chirp",
        2260 => "record_far",
        2261 => "record_mall",
        2262 => "record_mellohi",
        2263 => "record_stal",
        2264 => "record_strad",
        2265 => "record_ward",
        2267 => "record_wait",
        _ => return None,
    };
    Some(name)
}

/// An atlas of 16×16 item thumbnails packed into a 16-wide tile grid, built from
/// the `textures/items/` assets the [`item_texture_name`] table references. Only
/// items whose texture actually loaded get a tile; the rest fall back to swatches.
#[derive(Debug, Clone, Default)]
pub struct ItemAtlasImage {
    image: Option<RgbaImage>,
    name_to_index: HashMap<&'static str, u32>,
}

impl ItemAtlasImage {
    /// Build the item atlas by loading every referenced item texture. Always
    /// succeeds (an empty atlas when no item assets are present).
    pub fn load_default() -> Self {
        // Collect the unique texture names referenced by the id table.
        let mut names: Vec<&'static str> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for id in (256..432).chain(2256..2268) {
            if let Some(name) = item_texture_name(id) {
                if seen.insert(name) {
                    names.push(name);
                }
            }
        }
        // Load each, keeping only the ones that exist.
        let loaded: Vec<(&'static str, RgbaImage)> = names
            .into_iter()
            .filter_map(|name| {
                load_asset_image(&format!("assets/minecraft/textures/items/{name}.png"))
                    .map(|image| (name, to_item_tile(image)))
            })
            .collect();
        if loaded.is_empty() {
            log::warn!("no item textures found; item thumbnails will use color swatches");
            return Self::default();
        }
        let count = loaded.len() as u32;
        let rows = count.div_ceil(ATLAS_COLUMNS).max(1);
        let mut image = RgbaImage::new(ATLAS_COLUMNS * TILE_SIZE, rows * TILE_SIZE);
        let mut name_to_index = HashMap::with_capacity(loaded.len());
        for (index, (name, tile)) in loaded.into_iter().enumerate() {
            let index = index as u32;
            let x = index % ATLAS_COLUMNS * TILE_SIZE;
            let y = index / ATLAS_COLUMNS * TILE_SIZE;
            let _ = image.copy_from(&tile, x, y);
            name_to_index.insert(name, index);
        }
        log::info!("loaded {count} item textures");
        Self {
            image: Some(image),
            name_to_index,
        }
    }

    pub fn image(&self) -> Option<&RgbaImage> {
        self.image.as_ref()
    }

    /// Source pixel rect of an item id's thumbnail in the atlas, or None if there
    /// is no loaded texture for it.
    pub fn tile_for_id(&self, id: i16) -> Option<(u32, u32)> {
        let name = item_texture_name(id)?;
        let index = *self.name_to_index.get(name)?;
        Some((
            index % ATLAS_COLUMNS * TILE_SIZE,
            index / ATLAS_COLUMNS * TILE_SIZE,
        ))
    }
}

/// Crop an item texture to its first animation frame (compass/clock are vertical
/// strips) and scale it to a 16×16 tile.
fn to_item_tile(image: RgbaImage) -> RgbaImage {
    let (w, h) = image.dimensions();
    let frame = if w > 0 && h > w && h % w == 0 {
        image::imageops::crop_imm(&image, 0, 0, w, w).to_image()
    } else {
        image
    };
    if frame.width() == TILE_SIZE && frame.height() == TILE_SIZE {
        frame
    } else {
        image::imageops::resize(&frame, TILE_SIZE, TILE_SIZE, FilterType::Nearest)
    }
}

/// Load a single entity texture (e.g. "steve", "zombie/zombie") from any
/// asset source.
pub fn load_entity_image(name: &str) -> Option<RgbaImage> {
    load_asset_image(&format!("assets/minecraft/textures/entity/{name}.png"))
}

/// Side length of the square sky atlas (sun + moon phases + a white star texel).
pub const SKY_ATLAS_PX: u32 = 128;

/// Sky-atlas UV rect `[u0, v0, u1, v1]` of the sun sprite.
pub fn sky_sun_rect() -> [f32; 4] {
    let s = SKY_ATLAS_PX as f32;
    [0.0, 64.0 / s, 32.0 / s, 96.0 / s]
}

/// Sky-atlas UV rect `[u0, v0, u1, v1]` of moon phase `0..7` (the 4×2 grid of
/// `moon_phases.png`, stored in the atlas's top 128×64 band).
pub fn sky_moon_rect(phase: u32) -> [f32; 4] {
    let s = SKY_ATLAS_PX as f32;
    let col = (phase % 4) as f32;
    let row = ((phase / 4) % 2) as f32;
    [
        col * 32.0 / s,
        row * 32.0 / s,
        (col + 1.0) * 32.0 / s,
        (row + 1.0) * 32.0 / s,
    ]
}

/// Sky-atlas UV of the opaque-white texel sampled by the (untextured) stars.
pub fn sky_white_uv() -> [f32; 2] {
    let s = SKY_ATLAS_PX as f32;
    [98.0 / s, 66.0 / s]
}

/// RGBA pixels for the sky atlas: `moon_phases.png` (128×64) in the top band,
/// `sun.png` (32×32) below it, and a small opaque-white block for star quads.
/// Missing assets fall back to procedural discs, so it always builds.
#[derive(Debug, Clone)]
pub struct SkyAtlasImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl SkyAtlasImage {
    pub fn load_default() -> Self {
        let mut atlas = RgbaImage::new(SKY_ATLAS_PX, SKY_ATLAS_PX);

        let moon = load_asset_image("assets/minecraft/textures/environment/moon_phases.png")
            .map(|img| resize_exact(img, 128, 64))
            .unwrap_or_else(procedural_moon_phases);
        let _ = atlas.copy_from(&moon, 0, 0);

        let sun = load_asset_image("assets/minecraft/textures/environment/sun.png")
            .map(|img| resize_exact(img, 32, 32))
            .unwrap_or_else(|| procedural_disc(32, [255, 245, 160]));
        let _ = atlas.copy_from(&sun, 0, 64);

        // Opaque-white block the star quads sample (see `sky_white_uv`).
        for y in 64..72 {
            for x in 96..104 {
                atlas.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }

        if load_asset_image("assets/minecraft/textures/environment/sun.png").is_some() {
            log::info!("loaded sky textures (sun/moon)");
        } else {
            log::info!("no sky textures found; using procedural sun/moon");
        }

        Self {
            width: SKY_ATLAS_PX,
            height: SKY_ATLAS_PX,
            pixels: atlas.into_raw(),
        }
    }
}

fn resize_exact(image: RgbaImage, w: u32, h: u32) -> RgbaImage {
    if image.dimensions() == (w, h) {
        image
    } else {
        DynamicImage::ImageRgba8(image)
            .resize_exact(w, h, FilterType::Nearest)
            .to_rgba8()
    }
}

/// A soft filled disc on a transparent tile, for a missing sun/moon texture.
fn procedural_disc(size: u32, color: [u8; 3]) -> RgbaImage {
    let r = size as f32 * 0.45;
    let c = size as f32 / 2.0;
    RgbaImage::from_fn(size, size, |x, y| {
        let dx = x as f32 + 0.5 - c;
        let dy = y as f32 + 0.5 - c;
        if dx * dx + dy * dy <= r * r {
            Rgba([color[0], color[1], color[2], 255])
        } else {
            Rgba([0, 0, 0, 0])
        }
    })
}

/// Eight gray moon discs across the 4×2 phase grid (procedural fallback).
fn procedural_moon_phases() -> RgbaImage {
    let mut image = RgbaImage::new(128, 64);
    let disc = procedural_disc(32, [200, 200, 210]);
    for phase in 0..8u32 {
        let x = (phase % 4) * 32;
        let y = (phase / 4) * 32;
        let _ = image.copy_from(&disc, x, y);
    }
    image
}

pub(crate) fn load_asset_image(asset: &str) -> Option<RgbaImage> {
    for path in candidate_asset_paths() {
        let image = if path.is_dir() {
            read_directory_image(&path, asset)
        } else {
            File::open(&path)
                .ok()
                .and_then(|file| ZipArchive::new(file).ok())
                .and_then(|mut zip| read_zip_image(&mut zip, asset))
        };
        if let Some(image) = image {
            return Some(image.to_rgba8());
        }
    }
    None
}

/// Load a raw (non-image) asset file, e.g. `font/glyph_sizes.bin`.
pub(crate) fn load_asset_bytes(asset: &str) -> Option<Vec<u8>> {
    for path in candidate_asset_paths() {
        if path.is_dir() {
            if let Ok(bytes) = fs::read(directory_texture_path(&path, asset)) {
                return Some(bytes);
            }
        } else if let Some(bytes) = File::open(&path)
            .ok()
            .and_then(|file| ZipArchive::new(file).ok())
            .and_then(|mut zip| {
                let mut file = zip.by_name(asset).ok()?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).ok()?;
                Some(bytes)
            })
        {
            return Some(bytes);
        }
    }
    None
}

/// Side length in pixels of one slot in the entity texture atlas grid.
pub const ENTITY_SLOT_PX: u32 = 64;

/// Number of fixed slots stacked at the top of the entity atlas (one per
/// [`EntitySlot`], including the trailing guaranteed-white slot).
pub const ENTITY_SLOT_COUNT: u32 = 25;

/// Extra 64x64 rows reserved below the fixed slots for per-player downloaded
/// skins, allocated at runtime by the skin loader.
pub const PLAYER_SKIN_SLOTS: u32 = 64;

/// First atlas row of the per-player skin region.
pub const PLAYER_SKIN_BASE_ROW: u32 = ENTITY_SLOT_COUNT;

/// Dimensions of the entity texture atlas: a single column of fixed
/// [`EntitySlot`] rows followed by [`PLAYER_SKIN_SLOTS`] per-player skin rows,
/// each `ENTITY_SLOT_PX` square.
pub const ENTITY_ATLAS_WIDTH: u32 = ENTITY_SLOT_PX;
pub const ENTITY_ATLAS_HEIGHT: u32 = (ENTITY_SLOT_COUNT + PLAYER_SKIN_SLOTS) * ENTITY_SLOT_PX;

/// Pixel origin (top-left) of per-player skin row `index` (0-based).
pub fn player_skin_row_origin(index: u32) -> (u32, u32) {
    (0, (PLAYER_SKIN_BASE_ROW + index) * ENTITY_SLOT_PX)
}

/// Decode a downloaded skin PNG and normalize it to the modern 64x64 layout,
/// returning tightly-packed RGBA (64×64×4 bytes) for upload into a skin row.
pub fn normalize_skin_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    Some(normalize_skin(image).into_raw())
}

/// One 64x64 slot of the entity texture atlas. The discriminant is the slot's
/// row index from the top; `entity_slot_origin` yields its pixel origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntitySlot {
    /// Player skin (steve.png), normalized to the modern 64x64 layout.
    Player = 0,
    Zombie = 1,
    /// 64x32 source texture; occupies the top half of the slot.
    Skeleton = 2,
    Creeper = 3,
    Pig = 4,
    Cow = 5,
    Sheep = 6,
    Chicken = 7,
    Villager = 8,
    ZombiePigman = 9,
    Mooshroom = 10,
    /// Sheep wool overlay (sheep_fur.png), drawn as an inflated second layer.
    SheepFur = 11,
    Wolf = 12,
    Ocelot = 13,
    Spider = 14,
    Enderman = 15,
    Slime = 16,
    MagmaCube = 17,
    Squid = 18,
    Snowman = 19,
    Bat = 20,
    Blaze = 21,
    Ghast = 22,
    Silverfish = 23,
    /// Guaranteed opaque-white slot sampled by solid-color geometry.
    White = 24,
}

/// Pixel origin (top-left corner) of an entity atlas slot.
pub fn entity_slot_origin(slot: EntitySlot) -> (u32, u32) {
    (0, slot as u32 * ENTITY_SLOT_PX)
}

/// UV of a texel inside the guaranteed-opaque-white region of the entity
/// atlas (the center of the `EntitySlot::White` slot); solid-color model
/// geometry samples here so its vertex tint passes through unchanged.
pub const ENTITY_WHITE_UV: [f32; 2] = [
    0.5,
    ((EntitySlot::White as u32 * ENTITY_SLOT_PX + ENTITY_SLOT_PX / 2) as f32)
        / ENTITY_ATLAS_HEIGHT as f32,
];

/// The 1.8 entity textures loaded into each mob slot, plus the procedural
/// fallback tint used when the asset is missing. The player slot is handled
/// separately (normalize_skin / procedural_skin).
const MOB_SLOT_ASSETS: [(EntitySlot, &str, [u8; 3]); 23] = [
    (EntitySlot::Zombie, "zombie/zombie", [88, 124, 80]),
    (EntitySlot::Skeleton, "skeleton/skeleton", [192, 192, 192]),
    (EntitySlot::Creeper, "creeper/creeper", [86, 170, 70]),
    (EntitySlot::Pig, "pig/pig", [238, 158, 158]),
    (EntitySlot::Cow, "cow/cow", [108, 80, 58]),
    (EntitySlot::Sheep, "sheep/sheep", [228, 228, 228]),
    (EntitySlot::Chicken, "chicken", [238, 238, 216]),
    (EntitySlot::Villager, "villager/villager", [136, 104, 70]),
    (EntitySlot::ZombiePigman, "zombie_pigman", [228, 150, 150]),
    (EntitySlot::Mooshroom, "cow/mooshroom", [160, 40, 40]),
    (EntitySlot::SheepFur, "sheep/sheep_fur", [228, 228, 228]),
    (EntitySlot::Wolf, "wolf/wolf", [206, 200, 192]),
    (EntitySlot::Ocelot, "cat/ocelot", [200, 168, 104]),
    (EntitySlot::Spider, "spider/spider", [62, 48, 42]),
    (EntitySlot::Enderman, "enderman/enderman", [22, 22, 30]),
    (EntitySlot::Slime, "slime/slime", [112, 200, 92]),
    (EntitySlot::MagmaCube, "slime/magmacube", [180, 70, 30]),
    (EntitySlot::Squid, "squid", [80, 96, 152]),
    (EntitySlot::Snowman, "snowman", [228, 236, 240]),
    (EntitySlot::Bat, "bat", [84, 72, 62]),
    (EntitySlot::Blaze, "blaze", [232, 170, 40]),
    (EntitySlot::Ghast, "ghast/ghast", [220, 220, 220]),
    (EntitySlot::Silverfish, "silverfish", [120, 120, 130]),
];

/// RGBA pixels for the entity atlas sampled by the model pass: a vertical
/// grid of 64x64 slots, one per entity texture (player skin plus the common
/// 1.8 mobs), with a guaranteed solid-white slot at the bottom for
/// solid-color geometry. Every slot has a procedural fallback, so building
/// the atlas always succeeds even with no assets installed.
#[derive(Debug, Clone)]
pub struct EntityAtlasImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub player_skin_loaded: bool,
    /// How many of the mob slots were filled from real assets (the rest use
    /// per-type procedural textures).
    pub mob_textures_loaded: usize,
}

impl EntityAtlasImage {
    /// Build the atlas, falling back to procedural textures per slot when an
    /// asset is missing. Always succeeds.
    pub fn load_default() -> Self {
        // Start fully white so the White slot (and any texel a normalized
        // texture does not cover, e.g. the bottom half of a 64x32 slot) is a
        // safe opaque-white sample.
        let mut atlas = RgbaImage::from_pixel(
            ENTITY_ATLAS_WIDTH,
            ENTITY_ATLAS_HEIGHT,
            Rgba([255, 255, 255, 255]),
        );

        let skin = load_entity_image("steve");
        let player_skin_loaded = skin.is_some();
        let skin = skin.map(normalize_skin).unwrap_or_else(procedural_skin);
        blit_slot(&mut atlas, EntitySlot::Player, &skin);

        let mut mob_textures_loaded = 0;
        for (slot, name, tint) in MOB_SLOT_ASSETS {
            let image = match load_entity_image(name) {
                Some(image) => {
                    mob_textures_loaded += 1;
                    normalize_entity_texture(image)
                }
                None => procedural_entity_texture(tint),
            };
            blit_slot(&mut atlas, slot, &image);
        }
        log::info!(
            "loaded {mob_textures_loaded}/{} mob entity textures",
            MOB_SLOT_ASSETS.len()
        );

        Self {
            width: ENTITY_ATLAS_WIDTH,
            height: ENTITY_ATLAS_HEIGHT,
            pixels: atlas.into_raw(),
            player_skin_loaded,
            mob_textures_loaded,
        }
    }
}

/// Copy a (normalized, at most 64x64) image into its slot, anchored top-left.
fn blit_slot(atlas: &mut RgbaImage, slot: EntitySlot, image: &RgbaImage) {
    let (x, y) = entity_slot_origin(slot);
    let _ = atlas.copy_from(image, x, y);
}

/// Fit a mob texture into a 64-wide slot keeping its aspect ratio: 64x64
/// textures fill the slot, 64x32 textures (and other 2:1 sources) end up in
/// the slot's top half, which matches their models' UV layout. Anything else
/// is scaled to 64 wide with the height clamped to the slot.
fn normalize_entity_texture(image: RgbaImage) -> RgbaImage {
    let (w, h) = image.dimensions();
    if w == ENTITY_SLOT_PX && h <= ENTITY_SLOT_PX {
        return image;
    }
    let target_h = (h * ENTITY_SLOT_PX / w.max(1)).clamp(1, ENTITY_SLOT_PX);
    DynamicImage::ImageRgba8(image)
        .resize_exact(ENTITY_SLOT_PX, target_h, FilterType::Nearest)
        .to_rgba8()
}

/// Procedural stand-in for a missing mob texture: a two-tone checker of the
/// type's tint, plus dark eyes in the head-front region (x 8..16, y 8..16 for
/// the 8x8x8 head most of these models use) so mobs stay readable.
fn procedural_entity_texture(tint: [u8; 3]) -> RgbaImage {
    let shade = |c: u8, mul: f32| (c as f32 * mul).round().clamp(0.0, 255.0) as u8;
    let base = Rgba([tint[0], tint[1], tint[2], 255]);
    let dark = Rgba([
        shade(tint[0], 0.82),
        shade(tint[1], 0.82),
        shade(tint[2], 0.82),
        255,
    ]);
    let eye = Rgba([
        shade(tint[0], 0.25),
        shade(tint[1], 0.25),
        shade(tint[2], 0.25),
        255,
    ]);
    let mut image = RgbaImage::from_fn(ENTITY_SLOT_PX, ENTITY_SLOT_PX, |x, y| {
        if ((x / 4) + (y / 4)) % 2 == 0 {
            base
        } else {
            dark
        }
    });
    for (x, y) in [(9, 11), (10, 11), (13, 11), (14, 11)] {
        image.put_pixel(x, y, eye);
        image.put_pixel(x, y + 1, eye);
    }
    image
}

/// Bring a player skin to the modern 64x64 layout: HD skins are downscaled
/// and legacy 64x32 skins get their right arm/leg copied into the 1.8-format
/// left arm/leg slots so the geometry builder can hardcode one layout.
fn normalize_skin(image: RgbaImage) -> RgbaImage {
    // Legacy skins are 2:1 (64x32 and HD multiples); everything else is
    // treated as the square modern layout.
    let target_height = if image.height() * 2 == image.width() {
        32
    } else {
        64
    };
    let image = if image.width() != 64 || image.height() != target_height {
        DynamicImage::ImageRgba8(image)
            .resize_exact(64, target_height, FilterType::Nearest)
            .to_rgba8()
    } else {
        image
    };
    if image.height() == 64 {
        return image;
    }
    let mut full = RgbaImage::new(64, 64);
    let _ = full.copy_from(&image, 0, 0);
    copy_region(&mut full, 0, 16, 16, 48, 16, 16); // right leg -> left leg slot
    copy_region(&mut full, 40, 16, 32, 48, 16, 16); // right arm -> left arm slot
    full
}

fn copy_region(image: &mut RgbaImage, sx: u32, sy: u32, dx: u32, dy: u32, w: u32, h: u32) {
    for y in 0..h {
        for x in 0..w {
            let pixel = *image.get_pixel(sx + x, sy + y);
            image.put_pixel(dx + x, dy + y, pixel);
        }
    }
}

/// A simple Steve-like 64x64 skin so players stay readable when no asset
/// skin is available: skin-tone head/arms, hair, eyes, teal shirt, indigo
/// pants and gray shoes painted into the standard skin regions.
fn procedural_skin() -> RgbaImage {
    const SKIN: Rgba<u8> = Rgba([198, 152, 110, 255]);
    const HAIR: Rgba<u8> = Rgba([66, 48, 30, 255]);
    const SHIRT: Rgba<u8> = Rgba([0, 134, 143, 255]);
    const PANTS: Rgba<u8> = Rgba([64, 64, 140, 255]);
    const SHOES: Rgba<u8> = Rgba([90, 90, 90, 255]);
    const EYE_WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);
    const EYE_IRIS: Rgba<u8> = Rgba([60, 50, 130, 255]);

    let mut skin = RgbaImage::from_pixel(64, 64, SKIN);
    let mut fill = |x0: u32, y0: u32, x1: u32, y1: u32, color: Rgba<u8>| {
        for y in y0..y1 {
            for x in x0..x1 {
                skin.put_pixel(x, y, color);
            }
        }
    };
    fill(16, 16, 40, 32, SHIRT); // torso block
    fill(0, 16, 16, 32, PANTS); // right leg block
    fill(16, 48, 32, 64, PANTS); // left leg block (1.8 layout)
    fill(0, 30, 16, 32, SHOES); // bottom rows of the leg side strips
    fill(16, 62, 32, 64, SHOES);
    fill(8, 0, 16, 8, HAIR); // head top
    fill(0, 8, 32, 11, HAIR); // hair band across all four head sides
                              // Symmetric eyes on the head-front region (x 8..16, y 8..16).
    skin.put_pixel(9, 12, EYE_WHITE);
    skin.put_pixel(10, 12, EYE_IRIS);
    skin.put_pixel(13, 12, EYE_IRIS);
    skin.put_pixel(14, 12, EYE_WHITE);
    skin
}

fn candidate_asset_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("RECRAFT_ASSET_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("RECRAFT_ASSET_ZIP") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("RECRAFT_MINECRAFT_JAR") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from("local_assets/minecraft-1.8.9"));
    candidates.push(PathBuf::from("local_assets/default"));
    candidates.push(PathBuf::from("resourcepacks/default"));
    candidates.push(PathBuf::from("."));
    candidates.push(PathBuf::from("1.8.9.jar"));
    candidates.push(PathBuf::from("assets/1.8.9.jar"));
    if let Some(appdata) = env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join(".minecraft/versions/1.8.9/1.8.9.jar"));
    }
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates
            .push(home.join("Library/Application Support/minecraft/versions/1.8.9/1.8.9.jar"));
        candidates.push(home.join(".minecraft/versions/1.8.9/1.8.9.jar"));
    }
    candidates
        .into_iter()
        .filter(|path| asset_path_exists(path))
        .collect()
}

fn asset_path_exists(path: &Path) -> bool {
    if path.is_file() {
        return true;
    }
    path.join("assets/minecraft/textures/blocks").is_dir()
        || path.join("minecraft/textures/blocks").is_dir()
}

fn directory_texture_path(root: &Path, asset_path: &str) -> PathBuf {
    let full_path = root.join(asset_path);
    if full_path.exists() {
        return full_path;
    }
    root.join(asset_path.strip_prefix("assets/").unwrap_or(asset_path))
}

fn read_directory_image(root: &Path, asset_path: &str) -> Option<DynamicImage> {
    let full_path = directory_texture_path(root, asset_path);
    let bytes = fs::read(full_path).ok()?;
    image::load_from_memory(&bytes).ok()
}

fn read_zip_image<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    asset_path: &str,
) -> Option<DynamicImage> {
    let mut file = zip.by_name(asset_path).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    image::load_from_memory(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_atlas_has_expected_dimensions_and_white_slot() {
        let atlas = EntityAtlasImage::load_default();
        assert_eq!(atlas.width, ENTITY_ATLAS_WIDTH);
        assert_eq!(atlas.height, ENTITY_ATLAS_HEIGHT);
        assert_eq!(
            atlas.pixels.len(),
            (ENTITY_ATLAS_WIDTH * ENTITY_ATLAS_HEIGHT * 4) as usize
        );
        // The texel under ENTITY_WHITE_UV must be opaque white regardless of
        // which assets were found.
        let x = (ENTITY_WHITE_UV[0] * ENTITY_ATLAS_WIDTH as f32) as u32;
        let y = (ENTITY_WHITE_UV[1] * ENTITY_ATLAS_HEIGHT as f32) as u32;
        let i = ((y * ENTITY_ATLAS_WIDTH + x) * 4) as usize;
        assert_eq!(&atlas.pixels[i..i + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn missing_tile_detection_flags_unmapped_names() {
        let atlas = TextureAtlasImage::load_default().uv_table();
        // No name / unknown name resolve to the magenta tile.
        assert!(atlas.is_missing_tile(None));
        assert!(atlas.is_missing_tile(Some("definitely_not_a_texture")));
        // A registry texture is mapped (real tile with assets; in fallback
        // mode the per-tile missing set is intentionally empty).
        assert!(!atlas.is_missing_tile(Some("stone")));
    }

    #[test]
    fn item_names_cover_common_ids() {
        assert_eq!(item_texture_name(256), Some("iron_shovel"));
        assert_eq!(item_texture_name(264), Some("diamond"));
        assert_eq!(item_texture_name(331), Some("redstone_dust"));
        assert_eq!(item_texture_name(2257), Some("record_cat"));
        assert_eq!(item_texture_name(0), None); // air
        assert_eq!(item_texture_name(255), None); // still a block id
    }

    #[test]
    fn item_atlas_builds_and_resolves() {
        let atlas = ItemAtlasImage::load_default();
        // Air / block ids never resolve to an item tile.
        assert!(atlas.tile_for_id(0).is_none());
        assert!(atlas.tile_for_id(1).is_none());
        // With item assets present, common items get a tile; with none the atlas
        // is empty — both are valid and must never panic.
        if atlas.image().is_some() {
            assert!(
                atlas.tile_for_id(264).is_some() || atlas.tile_for_id(256).is_some(),
                "expected a common item to resolve when item assets exist"
            );
        }
    }

    #[test]
    fn slot_origins_are_distinct_and_inside_the_atlas() {
        let slots = [
            EntitySlot::Player,
            EntitySlot::Zombie,
            EntitySlot::Skeleton,
            EntitySlot::Creeper,
            EntitySlot::Pig,
            EntitySlot::Cow,
            EntitySlot::Sheep,
            EntitySlot::Chicken,
            EntitySlot::Villager,
            EntitySlot::ZombiePigman,
            EntitySlot::Mooshroom,
            EntitySlot::SheepFur,
            EntitySlot::Wolf,
            EntitySlot::Ocelot,
            EntitySlot::Spider,
            EntitySlot::Enderman,
            EntitySlot::Slime,
            EntitySlot::MagmaCube,
            EntitySlot::Squid,
            EntitySlot::Snowman,
            EntitySlot::Bat,
            EntitySlot::Blaze,
            EntitySlot::Ghast,
            EntitySlot::Silverfish,
            EntitySlot::White,
        ];
        let mut seen = std::collections::HashSet::new();
        for slot in slots {
            let (x, y) = entity_slot_origin(slot);
            assert!(x + ENTITY_SLOT_PX <= ENTITY_ATLAS_WIDTH);
            assert!(y + ENTITY_SLOT_PX <= ENTITY_ATLAS_HEIGHT);
            assert!(seen.insert((x, y)), "duplicate slot origin {x},{y}");
        }
        assert_eq!(seen.len() as u32, ENTITY_SLOT_COUNT);
    }

    #[test]
    fn normalize_entity_texture_keeps_two_to_one_sources_in_the_top_half() {
        let legacy = RgbaImage::from_pixel(128, 64, Rgba([1, 2, 3, 255]));
        let normalized = normalize_entity_texture(legacy);
        assert_eq!(normalized.dimensions(), (64, 32));
        let square = RgbaImage::from_pixel(128, 128, Rgba([1, 2, 3, 255]));
        assert_eq!(normalize_entity_texture(square).dimensions(), (64, 64));
    }
}
