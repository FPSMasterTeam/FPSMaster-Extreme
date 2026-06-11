use std::{
    env,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use image::{imageops::FilterType, DynamicImage, GenericImage, Rgba, RgbaImage};
use zip::ZipArchive;

pub const TILE_SIZE: u32 = 16;
pub const ATLAS_COLUMNS: u32 = 16;
pub const ATLAS_ROWS: u32 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTile {
    Missing = 0,
    Stone = 1,
    GrassTop = 2,
    GrassSide = 3,
    Dirt = 4,
    CoarseDirt = 5,
    PodzolTop = 6,
    PodzolSide = 7,
    Cobblestone = 8,
    Bedrock = 9,
    Gravel = 10,
    Sand = 11,
    RedSand = 12,
    Granite = 13,
    PolishedGranite = 14,
    Diorite = 15,
    PolishedDiorite = 16,
    Andesite = 17,
    PolishedAndesite = 18,
    CoalOre = 19,
    IronOre = 20,
    GoldOre = 21,
    LapisOre = 22,
    RedstoneOre = 23,
    DiamondOre = 24,
    EmeraldOre = 25,
    PlanksOak = 26,
    PlanksSpruce = 27,
    PlanksBirch = 28,
    PlanksJungle = 29,
    PlanksAcacia = 30,
    PlanksDarkOak = 31,
    OakLogSide = 32,
    OakLogTop = 33,
    SpruceLogSide = 34,
    SpruceLogTop = 35,
    BirchLogSide = 36,
    BirchLogTop = 37,
    JungleLogSide = 38,
    JungleLogTop = 39,
    AcaciaLogSide = 40,
    AcaciaLogTop = 41,
    DarkOakLogSide = 42,
    DarkOakLogTop = 43,
    OakLeaves = 44,
    SpruceLeaves = 45,
    BirchLeaves = 46,
    JungleLeaves = 47,
    AcaciaLeaves = 48,
    DarkOakLeaves = 49,
    SandstoneSide = 50,
    SandstoneTop = 51,
    SandstoneBottom = 52,
    SandstoneCarved = 53,
    SandstoneSmooth = 54,
    RedSandstoneSide = 55,
    RedSandstoneTop = 56,
    RedSandstoneBottom = 57,
    RedSandstoneCarved = 58,
    RedSandstoneSmooth = 59,
    WoolWhite = 60,
    WoolOrange = 61,
    WoolMagenta = 62,
    WoolLightBlue = 63,
    WoolYellow = 64,
    WoolLime = 65,
    WoolPink = 66,
    WoolGray = 67,
    WoolSilver = 68,
    WoolCyan = 69,
    WoolPurple = 70,
    WoolBlue = 71,
    WoolBrown = 72,
    WoolGreen = 73,
    WoolRed = 74,
    WoolBlack = 75,
    GoldBlock = 76,
    IronBlock = 77,
    LapisBlock = 78,
    DiamondBlock = 79,
    EmeraldBlock = 80,
    RedstoneBlock = 81,
    CoalBlock = 82,
    Brick = 83,
    MossyCobblestone = 84,
    Obsidian = 85,
    Snow = 86,
    Ice = 87,
    PackedIce = 88,
    Clay = 89,
    HardenedClay = 90,
    StainedClayWhite = 91,
    StainedClayOrange = 92,
    StainedClayMagenta = 93,
    StainedClayLightBlue = 94,
    StainedClayYellow = 95,
    StainedClayLime = 96,
    StainedClayPink = 97,
    StainedClayGray = 98,
    StainedClaySilver = 99,
    StainedClayCyan = 100,
    StainedClayPurple = 101,
    StainedClayBlue = 102,
    StainedClayBrown = 103,
    StainedClayGreen = 104,
    StainedClayRed = 105,
    StainedClayBlack = 106,
    PumpkinSide = 107,
    PumpkinTop = 108,
    PumpkinFace = 109,
    MelonSide = 110,
    MelonTop = 111,
    Netherrack = 112,
    SoulSand = 113,
    Glowstone = 114,
    StoneBrick = 115,
    StoneBrickMossy = 116,
    StoneBrickCracked = 117,
    StoneBrickCarved = 118,
    MyceliumTop = 119,
    MyceliumSide = 120,
    NetherBrick = 121,
    EndStone = 122,
    QuartzSide = 123,
    QuartzTop = 124,
    QuartzBottom = 125,
    QuartzChiseled = 126,
    QuartzChiseledTop = 127,
    QuartzPillarSide = 128,
    QuartzPillarTop = 129,
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
        let loaded = load_directory_tiles(&path, &mut atlas);

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
        let loaded = load_zip_tiles(&mut zip, &mut atlas);

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
    for y in 0..image.height() {
        for x in 0..image.width() {
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
    fill_tile(&mut atlas, BlockTile::Cobblestone, [98, 98, 98, 255]);
    fill_tile(&mut atlas, BlockTile::GrassTop, [82, 158, 45, 255]);
    fill_tile(&mut atlas, BlockTile::GrassSide, [94, 132, 48, 255]);
    fill_tile(&mut atlas, BlockTile::Dirt, [115, 76, 39, 255]);
    fill_tile(&mut atlas, BlockTile::Sand, [194, 178, 128, 255]);
    fill_tile(&mut atlas, BlockTile::OakLogSide, [89, 55, 28, 255]);
    fill_tile(&mut atlas, BlockTile::OakLogTop, [151, 119, 75, 255]);
    fill_tile(&mut atlas, BlockTile::PlanksOak, [157, 128, 79, 255]);
    fill_tile(&mut atlas, BlockTile::OakLeaves, [46, 115, 40, 220]);
    fill_tile(&mut atlas, BlockTile::Snow, [235, 245, 245, 255]);
    fill_tile(&mut atlas, BlockTile::Netherrack, [111, 54, 53, 255]);
    fill_tile(&mut atlas, BlockTile::Glowstone, [171, 135, 84, 255]);
    atlas
}

struct TextureSpec {
    tile: BlockTile,
    paths: &'static [&'static str],
}

const TEXTURE_SPECS: &[TextureSpec] = &[
    TextureSpec {
        tile: BlockTile::Stone,
        paths: &["assets/minecraft/textures/blocks/stone.png"],
    },
    TextureSpec {
        tile: BlockTile::GrassTop,
        paths: &["assets/minecraft/textures/blocks/grass_top.png"],
    },
    TextureSpec {
        tile: BlockTile::GrassSide,
        paths: &["assets/minecraft/textures/blocks/grass_side.png"],
    },
    TextureSpec {
        tile: BlockTile::Dirt,
        paths: &["assets/minecraft/textures/blocks/dirt.png"],
    },
    TextureSpec {
        tile: BlockTile::CoarseDirt,
        paths: &["assets/minecraft/textures/blocks/coarse_dirt.png"],
    },
    TextureSpec {
        tile: BlockTile::PodzolTop,
        paths: &["assets/minecraft/textures/blocks/dirt_podzol_top.png"],
    },
    TextureSpec {
        tile: BlockTile::PodzolSide,
        paths: &["assets/minecraft/textures/blocks/dirt_podzol_side.png"],
    },
    TextureSpec {
        tile: BlockTile::Cobblestone,
        paths: &["assets/minecraft/textures/blocks/cobblestone.png"],
    },
    TextureSpec {
        tile: BlockTile::Bedrock,
        paths: &["assets/minecraft/textures/blocks/bedrock.png"],
    },
    TextureSpec {
        tile: BlockTile::Gravel,
        paths: &["assets/minecraft/textures/blocks/gravel.png"],
    },
    TextureSpec {
        tile: BlockTile::Sand,
        paths: &["assets/minecraft/textures/blocks/sand.png"],
    },
    TextureSpec {
        tile: BlockTile::RedSand,
        paths: &["assets/minecraft/textures/blocks/red_sand.png"],
    },
    TextureSpec {
        tile: BlockTile::Granite,
        paths: &["assets/minecraft/textures/blocks/stone_granite.png"],
    },
    TextureSpec {
        tile: BlockTile::PolishedGranite,
        paths: &["assets/minecraft/textures/blocks/stone_granite_smooth.png"],
    },
    TextureSpec {
        tile: BlockTile::Diorite,
        paths: &["assets/minecraft/textures/blocks/stone_diorite.png"],
    },
    TextureSpec {
        tile: BlockTile::PolishedDiorite,
        paths: &["assets/minecraft/textures/blocks/stone_diorite_smooth.png"],
    },
    TextureSpec {
        tile: BlockTile::Andesite,
        paths: &["assets/minecraft/textures/blocks/stone_andesite.png"],
    },
    TextureSpec {
        tile: BlockTile::PolishedAndesite,
        paths: &["assets/minecraft/textures/blocks/stone_andesite_smooth.png"],
    },
    TextureSpec {
        tile: BlockTile::CoalOre,
        paths: &["assets/minecraft/textures/blocks/coal_ore.png"],
    },
    TextureSpec {
        tile: BlockTile::IronOre,
        paths: &["assets/minecraft/textures/blocks/iron_ore.png"],
    },
    TextureSpec {
        tile: BlockTile::GoldOre,
        paths: &["assets/minecraft/textures/blocks/gold_ore.png"],
    },
    TextureSpec {
        tile: BlockTile::LapisOre,
        paths: &["assets/minecraft/textures/blocks/lapis_ore.png"],
    },
    TextureSpec {
        tile: BlockTile::RedstoneOre,
        paths: &["assets/minecraft/textures/blocks/redstone_ore.png"],
    },
    TextureSpec {
        tile: BlockTile::DiamondOre,
        paths: &["assets/minecraft/textures/blocks/diamond_ore.png"],
    },
    TextureSpec {
        tile: BlockTile::EmeraldOre,
        paths: &["assets/minecraft/textures/blocks/emerald_ore.png"],
    },
    TextureSpec {
        tile: BlockTile::PlanksOak,
        paths: &["assets/minecraft/textures/blocks/planks_oak.png"],
    },
    TextureSpec {
        tile: BlockTile::PlanksSpruce,
        paths: &["assets/minecraft/textures/blocks/planks_spruce.png"],
    },
    TextureSpec {
        tile: BlockTile::PlanksBirch,
        paths: &["assets/minecraft/textures/blocks/planks_birch.png"],
    },
    TextureSpec {
        tile: BlockTile::PlanksJungle,
        paths: &["assets/minecraft/textures/blocks/planks_jungle.png"],
    },
    TextureSpec {
        tile: BlockTile::PlanksAcacia,
        paths: &["assets/minecraft/textures/blocks/planks_acacia.png"],
    },
    TextureSpec {
        tile: BlockTile::PlanksDarkOak,
        paths: &["assets/minecraft/textures/blocks/planks_big_oak.png"],
    },
    TextureSpec {
        tile: BlockTile::OakLogSide,
        paths: &["assets/minecraft/textures/blocks/log_oak.png"],
    },
    TextureSpec {
        tile: BlockTile::OakLogTop,
        paths: &["assets/minecraft/textures/blocks/log_oak_top.png"],
    },
    TextureSpec {
        tile: BlockTile::SpruceLogSide,
        paths: &["assets/minecraft/textures/blocks/log_spruce.png"],
    },
    TextureSpec {
        tile: BlockTile::SpruceLogTop,
        paths: &["assets/minecraft/textures/blocks/log_spruce_top.png"],
    },
    TextureSpec {
        tile: BlockTile::BirchLogSide,
        paths: &["assets/minecraft/textures/blocks/log_birch.png"],
    },
    TextureSpec {
        tile: BlockTile::BirchLogTop,
        paths: &["assets/minecraft/textures/blocks/log_birch_top.png"],
    },
    TextureSpec {
        tile: BlockTile::JungleLogSide,
        paths: &["assets/minecraft/textures/blocks/log_jungle.png"],
    },
    TextureSpec {
        tile: BlockTile::JungleLogTop,
        paths: &["assets/minecraft/textures/blocks/log_jungle_top.png"],
    },
    TextureSpec {
        tile: BlockTile::AcaciaLogSide,
        paths: &["assets/minecraft/textures/blocks/log_acacia.png"],
    },
    TextureSpec {
        tile: BlockTile::AcaciaLogTop,
        paths: &["assets/minecraft/textures/blocks/log_acacia_top.png"],
    },
    TextureSpec {
        tile: BlockTile::DarkOakLogSide,
        paths: &["assets/minecraft/textures/blocks/log_big_oak.png"],
    },
    TextureSpec {
        tile: BlockTile::DarkOakLogTop,
        paths: &["assets/minecraft/textures/blocks/log_big_oak_top.png"],
    },
    TextureSpec {
        tile: BlockTile::OakLeaves,
        paths: &["assets/minecraft/textures/blocks/leaves_oak.png"],
    },
    TextureSpec {
        tile: BlockTile::SpruceLeaves,
        paths: &["assets/minecraft/textures/blocks/leaves_spruce.png"],
    },
    TextureSpec {
        tile: BlockTile::BirchLeaves,
        paths: &["assets/minecraft/textures/blocks/leaves_birch.png"],
    },
    TextureSpec {
        tile: BlockTile::JungleLeaves,
        paths: &["assets/minecraft/textures/blocks/leaves_jungle.png"],
    },
    TextureSpec {
        tile: BlockTile::AcaciaLeaves,
        paths: &["assets/minecraft/textures/blocks/leaves_acacia.png"],
    },
    TextureSpec {
        tile: BlockTile::DarkOakLeaves,
        paths: &["assets/minecraft/textures/blocks/leaves_big_oak.png"],
    },
    TextureSpec {
        tile: BlockTile::SandstoneSide,
        paths: &["assets/minecraft/textures/blocks/sandstone_normal.png"],
    },
    TextureSpec {
        tile: BlockTile::SandstoneTop,
        paths: &["assets/minecraft/textures/blocks/sandstone_top.png"],
    },
    TextureSpec {
        tile: BlockTile::SandstoneBottom,
        paths: &["assets/minecraft/textures/blocks/sandstone_bottom.png"],
    },
    TextureSpec {
        tile: BlockTile::SandstoneCarved,
        paths: &["assets/minecraft/textures/blocks/sandstone_carved.png"],
    },
    TextureSpec {
        tile: BlockTile::SandstoneSmooth,
        paths: &["assets/minecraft/textures/blocks/sandstone_smooth.png"],
    },
    TextureSpec {
        tile: BlockTile::RedSandstoneSide,
        paths: &["assets/minecraft/textures/blocks/red_sandstone_normal.png"],
    },
    TextureSpec {
        tile: BlockTile::RedSandstoneTop,
        paths: &["assets/minecraft/textures/blocks/red_sandstone_top.png"],
    },
    TextureSpec {
        tile: BlockTile::RedSandstoneBottom,
        paths: &["assets/minecraft/textures/blocks/red_sandstone_bottom.png"],
    },
    TextureSpec {
        tile: BlockTile::RedSandstoneCarved,
        paths: &["assets/minecraft/textures/blocks/red_sandstone_carved.png"],
    },
    TextureSpec {
        tile: BlockTile::RedSandstoneSmooth,
        paths: &["assets/minecraft/textures/blocks/red_sandstone_smooth.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolWhite,
        paths: &["assets/minecraft/textures/blocks/wool_colored_white.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolOrange,
        paths: &["assets/minecraft/textures/blocks/wool_colored_orange.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolMagenta,
        paths: &["assets/minecraft/textures/blocks/wool_colored_magenta.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolLightBlue,
        paths: &["assets/minecraft/textures/blocks/wool_colored_light_blue.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolYellow,
        paths: &["assets/minecraft/textures/blocks/wool_colored_yellow.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolLime,
        paths: &["assets/minecraft/textures/blocks/wool_colored_lime.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolPink,
        paths: &["assets/minecraft/textures/blocks/wool_colored_pink.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolGray,
        paths: &["assets/minecraft/textures/blocks/wool_colored_gray.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolSilver,
        paths: &["assets/minecraft/textures/blocks/wool_colored_silver.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolCyan,
        paths: &["assets/minecraft/textures/blocks/wool_colored_cyan.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolPurple,
        paths: &["assets/minecraft/textures/blocks/wool_colored_purple.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolBlue,
        paths: &["assets/minecraft/textures/blocks/wool_colored_blue.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolBrown,
        paths: &["assets/minecraft/textures/blocks/wool_colored_brown.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolGreen,
        paths: &["assets/minecraft/textures/blocks/wool_colored_green.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolRed,
        paths: &["assets/minecraft/textures/blocks/wool_colored_red.png"],
    },
    TextureSpec {
        tile: BlockTile::WoolBlack,
        paths: &["assets/minecraft/textures/blocks/wool_colored_black.png"],
    },
    TextureSpec {
        tile: BlockTile::GoldBlock,
        paths: &["assets/minecraft/textures/blocks/gold_block.png"],
    },
    TextureSpec {
        tile: BlockTile::IronBlock,
        paths: &["assets/minecraft/textures/blocks/iron_block.png"],
    },
    TextureSpec {
        tile: BlockTile::LapisBlock,
        paths: &["assets/minecraft/textures/blocks/lapis_block.png"],
    },
    TextureSpec {
        tile: BlockTile::DiamondBlock,
        paths: &["assets/minecraft/textures/blocks/diamond_block.png"],
    },
    TextureSpec {
        tile: BlockTile::EmeraldBlock,
        paths: &["assets/minecraft/textures/blocks/emerald_block.png"],
    },
    TextureSpec {
        tile: BlockTile::RedstoneBlock,
        paths: &["assets/minecraft/textures/blocks/redstone_block.png"],
    },
    TextureSpec {
        tile: BlockTile::CoalBlock,
        paths: &["assets/minecraft/textures/blocks/coal_block.png"],
    },
    TextureSpec {
        tile: BlockTile::Brick,
        paths: &["assets/minecraft/textures/blocks/brick.png"],
    },
    TextureSpec {
        tile: BlockTile::MossyCobblestone,
        paths: &["assets/minecraft/textures/blocks/cobblestone_mossy.png"],
    },
    TextureSpec {
        tile: BlockTile::Obsidian,
        paths: &["assets/minecraft/textures/blocks/obsidian.png"],
    },
    TextureSpec {
        tile: BlockTile::Snow,
        paths: &["assets/minecraft/textures/blocks/snow.png"],
    },
    TextureSpec {
        tile: BlockTile::Ice,
        paths: &["assets/minecraft/textures/blocks/ice.png"],
    },
    TextureSpec {
        tile: BlockTile::PackedIce,
        paths: &["assets/minecraft/textures/blocks/ice_packed.png"],
    },
    TextureSpec {
        tile: BlockTile::Clay,
        paths: &["assets/minecraft/textures/blocks/clay.png"],
    },
    TextureSpec {
        tile: BlockTile::HardenedClay,
        paths: &["assets/minecraft/textures/blocks/hardened_clay.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayWhite,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_white.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayOrange,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_orange.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayMagenta,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_magenta.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayLightBlue,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_light_blue.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayYellow,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_yellow.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayLime,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_lime.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayPink,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_pink.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayGray,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_gray.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClaySilver,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_silver.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayCyan,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_cyan.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayPurple,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_purple.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayBlue,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_blue.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayBrown,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_brown.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayGreen,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_green.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayRed,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_red.png"],
    },
    TextureSpec {
        tile: BlockTile::StainedClayBlack,
        paths: &["assets/minecraft/textures/blocks/hardened_clay_stained_black.png"],
    },
    TextureSpec {
        tile: BlockTile::PumpkinSide,
        paths: &["assets/minecraft/textures/blocks/pumpkin_side.png"],
    },
    TextureSpec {
        tile: BlockTile::PumpkinTop,
        paths: &["assets/minecraft/textures/blocks/pumpkin_top.png"],
    },
    TextureSpec {
        tile: BlockTile::PumpkinFace,
        paths: &["assets/minecraft/textures/blocks/pumpkin_face_off.png"],
    },
    TextureSpec {
        tile: BlockTile::MelonSide,
        paths: &["assets/minecraft/textures/blocks/melon_side.png"],
    },
    TextureSpec {
        tile: BlockTile::MelonTop,
        paths: &["assets/minecraft/textures/blocks/melon_top.png"],
    },
    TextureSpec {
        tile: BlockTile::Netherrack,
        paths: &["assets/minecraft/textures/blocks/netherrack.png"],
    },
    TextureSpec {
        tile: BlockTile::SoulSand,
        paths: &["assets/minecraft/textures/blocks/soul_sand.png"],
    },
    TextureSpec {
        tile: BlockTile::Glowstone,
        paths: &["assets/minecraft/textures/blocks/glowstone.png"],
    },
    TextureSpec {
        tile: BlockTile::StoneBrick,
        paths: &["assets/minecraft/textures/blocks/stonebrick.png"],
    },
    TextureSpec {
        tile: BlockTile::StoneBrickMossy,
        paths: &["assets/minecraft/textures/blocks/stonebrick_mossy.png"],
    },
    TextureSpec {
        tile: BlockTile::StoneBrickCracked,
        paths: &["assets/minecraft/textures/blocks/stonebrick_cracked.png"],
    },
    TextureSpec {
        tile: BlockTile::StoneBrickCarved,
        paths: &["assets/minecraft/textures/blocks/stonebrick_carved.png"],
    },
    TextureSpec {
        tile: BlockTile::MyceliumTop,
        paths: &["assets/minecraft/textures/blocks/mycelium_top.png"],
    },
    TextureSpec {
        tile: BlockTile::MyceliumSide,
        paths: &["assets/minecraft/textures/blocks/mycelium_side.png"],
    },
    TextureSpec {
        tile: BlockTile::NetherBrick,
        paths: &["assets/minecraft/textures/blocks/nether_brick.png"],
    },
    TextureSpec {
        tile: BlockTile::EndStone,
        paths: &["assets/minecraft/textures/blocks/end_stone.png"],
    },
    TextureSpec {
        tile: BlockTile::QuartzSide,
        paths: &["assets/minecraft/textures/blocks/quartz_block_side.png"],
    },
    TextureSpec {
        tile: BlockTile::QuartzTop,
        paths: &["assets/minecraft/textures/blocks/quartz_block_top.png"],
    },
    TextureSpec {
        tile: BlockTile::QuartzBottom,
        paths: &["assets/minecraft/textures/blocks/quartz_block_bottom.png"],
    },
    TextureSpec {
        tile: BlockTile::QuartzChiseled,
        paths: &["assets/minecraft/textures/blocks/quartz_block_chiseled.png"],
    },
    TextureSpec {
        tile: BlockTile::QuartzChiseledTop,
        paths: &["assets/minecraft/textures/blocks/quartz_block_chiseled_top.png"],
    },
    TextureSpec {
        tile: BlockTile::QuartzPillarSide,
        paths: &["assets/minecraft/textures/blocks/quartz_block_lines.png"],
    },
    TextureSpec {
        tile: BlockTile::QuartzPillarTop,
        paths: &["assets/minecraft/textures/blocks/quartz_block_lines_top.png"],
    },
];

fn load_directory_tiles(root: &Path, atlas: &mut RgbaImage) -> usize {
    TEXTURE_SPECS
        .iter()
        .map(|spec| copy_first_directory_tile(root, atlas, spec.tile, spec.paths))
        .sum()
}

fn load_zip_tiles<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    atlas: &mut RgbaImage,
) -> usize {
    TEXTURE_SPECS
        .iter()
        .map(|spec| copy_first_zip_tile(zip, atlas, spec.tile, spec.paths))
        .sum()
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
