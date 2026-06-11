use std::{
    env,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use image::{imageops::FilterType, DynamicImage, GenericImage, Rgba, RgbaImage};
use zip::ZipArchive;

pub const TILE_SIZE: u32 = 16;
pub const ATLAS_COLUMNS: u32 = 8;
pub const ATLAS_ROWS: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTile {
    Missing = 0,
    Stone = 1,
    GrassTop = 2,
    GrassSide = 3,
    Dirt = 4,
    Sand = 5,
    OakLog = 6,
    OakLeaves = 7,
}

impl BlockTile {
    pub const fn index(self) -> u32 {
        self as u32
    }
}

#[derive(Debug, Clone)]
pub struct TextureAtlasImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub source: TextureAtlasSource,
}

#[derive(Debug, Clone)]
pub enum TextureAtlasSource {
    Directory(PathBuf),
    Archive(PathBuf),
    Fallback,
}

impl TextureAtlasImage {
    pub fn load_default() -> Self {
        let candidates = candidate_asset_paths();
        for path in &candidates {
            match Self::from_asset_path(path.clone()) {
                Ok(atlas) => return atlas,
                Err(err) => log::warn!(
                    "failed to load Minecraft textures from {}: {err}",
                    path.display()
                ),
            }
        }
        if candidates.is_empty() {
            log::warn!(
                "no Minecraft assets found; run scripts/setup_minecraft_1_8_9_assets.py or pass --assets <resource-pack-root-or-zip>"
            );
        } else {
            log::warn!("tried Minecraft asset paths: {candidates:?}");
        }
        Self::fallback()
    }

    pub fn from_minecraft_jar(path: PathBuf) -> Result<Self, String> {
        Self::from_asset_zip(path)
    }

    pub fn from_asset_path(path: PathBuf) -> Result<Self, String> {
        if path.is_dir() {
            Self::from_asset_directory(path)
        } else {
            Self::from_asset_zip(path)
        }
    }

    pub fn from_asset_directory(path: PathBuf) -> Result<Self, String> {
        let mut atlas = fallback_atlas();

        let mut loaded = 0;
        loaded += copy_first_directory_tile(
            &path,
            &mut atlas,
            BlockTile::Stone,
            &["assets/minecraft/textures/blocks/stone.png"],
        );
        loaded += copy_first_directory_tile(
            &path,
            &mut atlas,
            BlockTile::GrassTop,
            &["assets/minecraft/textures/blocks/grass_top.png"],
        );
        loaded += copy_first_directory_tile(
            &path,
            &mut atlas,
            BlockTile::GrassSide,
            &["assets/minecraft/textures/blocks/grass_side.png"],
        );
        loaded += copy_first_directory_tile(
            &path,
            &mut atlas,
            BlockTile::Dirt,
            &["assets/minecraft/textures/blocks/dirt.png"],
        );
        loaded += copy_first_directory_tile(
            &path,
            &mut atlas,
            BlockTile::Sand,
            &["assets/minecraft/textures/blocks/sand.png"],
        );
        loaded += copy_first_directory_tile(
            &path,
            &mut atlas,
            BlockTile::OakLog,
            &[
                "assets/minecraft/textures/blocks/log_oak.png",
                "assets/minecraft/textures/blocks/log_oak_top.png",
            ],
        );
        loaded += copy_first_directory_tile(
            &path,
            &mut atlas,
            BlockTile::OakLeaves,
            &[
                "assets/minecraft/textures/blocks/leaves_oak.png",
                "assets/minecraft/textures/blocks/leaves_oak_opaque.png",
            ],
        );

        if loaded == 0 {
            return Err("directory did not contain 1.8-style block textures".to_owned());
        }
        log::info!("loaded {loaded} block atlas tiles from {}", path.display());

        Ok(Self {
            width: atlas.width(),
            height: atlas.height(),
            pixels: atlas.into_raw(),
            source: TextureAtlasSource::Directory(path),
        })
    }

    pub fn from_asset_zip(path: PathBuf) -> Result<Self, String> {
        let file = File::open(&path).map_err(|err| err.to_string())?;
        let mut zip = ZipArchive::new(file).map_err(|err| err.to_string())?;
        let mut atlas = fallback_atlas();

        let mut loaded = 0;
        loaded += copy_first_zip_tile(
            &mut zip,
            &mut atlas,
            BlockTile::Stone,
            &["assets/minecraft/textures/blocks/stone.png"],
        );
        loaded += copy_first_zip_tile(
            &mut zip,
            &mut atlas,
            BlockTile::GrassTop,
            &["assets/minecraft/textures/blocks/grass_top.png"],
        );
        loaded += copy_first_zip_tile(
            &mut zip,
            &mut atlas,
            BlockTile::GrassSide,
            &["assets/minecraft/textures/blocks/grass_side.png"],
        );
        loaded += copy_first_zip_tile(
            &mut zip,
            &mut atlas,
            BlockTile::Dirt,
            &["assets/minecraft/textures/blocks/dirt.png"],
        );
        loaded += copy_first_zip_tile(
            &mut zip,
            &mut atlas,
            BlockTile::Sand,
            &["assets/minecraft/textures/blocks/sand.png"],
        );
        loaded += copy_first_zip_tile(
            &mut zip,
            &mut atlas,
            BlockTile::OakLog,
            &[
                "assets/minecraft/textures/blocks/log_oak.png",
                "assets/minecraft/textures/blocks/log_oak_top.png",
            ],
        );
        loaded += copy_first_zip_tile(
            &mut zip,
            &mut atlas,
            BlockTile::OakLeaves,
            &[
                "assets/minecraft/textures/blocks/leaves_oak.png",
                "assets/minecraft/textures/blocks/leaves_oak_opaque.png",
            ],
        );

        if loaded == 0 {
            return Err("zip did not contain 1.8-style block textures".to_owned());
        }
        log::info!("loaded {loaded} block atlas tiles from {}", path.display());

        Ok(Self {
            width: atlas.width(),
            height: atlas.height(),
            pixels: atlas.into_raw(),
            source: TextureAtlasSource::Archive(path),
        })
    }

    fn fallback() -> Self {
        let atlas = fallback_atlas();
        Self {
            width: atlas.width(),
            height: atlas.height(),
            pixels: atlas.into_raw(),
            source: TextureAtlasSource::Fallback,
        }
    }
}

pub fn tile_uv(tile: BlockTile) -> [[f32; 2]; 4] {
    let index = tile.index();
    let x = index % ATLAS_COLUMNS;
    let y = index / ATLAS_COLUMNS;
    let atlas_w = (ATLAS_COLUMNS * TILE_SIZE) as f32;
    let atlas_h = (ATLAS_ROWS * TILE_SIZE) as f32;
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

fn empty_atlas() -> RgbaImage {
    let mut image = RgbaImage::new(ATLAS_COLUMNS * TILE_SIZE, ATLAS_ROWS * TILE_SIZE);
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let dark = ((x / 4) + (y / 4)) % 2 == 0;
            image.put_pixel(
                x,
                y,
                if dark {
                    Rgba([0, 0, 0, 255])
                } else {
                    Rgba([255, 0, 255, 255])
                },
            );
        }
    }
    image
}

fn fallback_atlas() -> RgbaImage {
    let mut atlas = empty_atlas();
    fill_tile(&mut atlas, BlockTile::Stone, [116, 116, 116, 255]);
    fill_tile(&mut atlas, BlockTile::GrassTop, [82, 158, 45, 255]);
    fill_tile(&mut atlas, BlockTile::GrassSide, [94, 132, 48, 255]);
    fill_tile(&mut atlas, BlockTile::Dirt, [115, 76, 39, 255]);
    fill_tile(&mut atlas, BlockTile::Sand, [194, 178, 128, 255]);
    fill_tile(&mut atlas, BlockTile::OakLog, [89, 55, 28, 255]);
    fill_tile(&mut atlas, BlockTile::OakLeaves, [46, 115, 40, 220]);
    atlas
}

fn copy_first_zip_tile<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    atlas: &mut RgbaImage,
    tile: BlockTile,
    paths: &[&str],
) -> usize {
    for path in paths {
        let Ok(mut file) = zip.by_name(path) else {
            continue;
        };
        let mut bytes = Vec::new();
        if let Err(err) = file.read_to_end(&mut bytes) {
            log::warn!("failed to read texture {path}: {err}");
            continue;
        }
        match image::load_from_memory(&bytes) {
            Ok(image) => {
                copy_tile(atlas, tile, image);
                return 1;
            }
            Err(err) => log::warn!("failed to decode texture {path}: {err}"),
        }
    }
    log::warn!("missing texture candidates for {tile:?}: {paths:?}");
    0
}

fn copy_first_directory_tile(
    root: &Path,
    atlas: &mut RgbaImage,
    tile: BlockTile,
    paths: &[&str],
) -> usize {
    for path in paths {
        let full_path = directory_texture_path(root, path);
        let Ok(bytes) = fs::read(&full_path) else {
            continue;
        };
        match image::load_from_memory(&bytes) {
            Ok(image) => {
                copy_tile(atlas, tile, image);
                return 1;
            }
            Err(err) => log::warn!("failed to decode texture {}: {err}", full_path.display()),
        }
    }
    log::warn!("missing texture candidates for {tile:?}: {paths:?}");
    0
}

fn directory_texture_path(root: &Path, asset_path: &str) -> PathBuf {
    let full_path = root.join(asset_path);
    if full_path.exists() {
        return full_path;
    }
    root.join(asset_path.strip_prefix("assets/").unwrap_or(asset_path))
}

fn copy_tile(atlas: &mut RgbaImage, tile: BlockTile, image: DynamicImage) {
    let tile_image = image
        .resize_exact(TILE_SIZE, TILE_SIZE, FilterType::Nearest)
        .to_rgba8();
    let x = tile.index() % ATLAS_COLUMNS * TILE_SIZE;
    let y = tile.index() / ATLAS_COLUMNS * TILE_SIZE;
    let _ = atlas.copy_from(&tile_image, x, y);
}

fn fill_tile(atlas: &mut RgbaImage, tile: BlockTile, rgba: [u8; 4]) {
    let x0 = tile.index() % ATLAS_COLUMNS * TILE_SIZE;
    let y0 = tile.index() / ATLAS_COLUMNS * TILE_SIZE;
    for y in y0..y0 + TILE_SIZE {
        for x in x0..x0 + TILE_SIZE {
            atlas.put_pixel(x, y, Rgba(rgba));
        }
    }
}
