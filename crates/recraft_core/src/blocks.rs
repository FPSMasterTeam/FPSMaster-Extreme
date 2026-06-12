//! Data-driven block definitions. The per-block render/collision/texture data
//! lives in `blocks.json`; this module parses it into a registry that the rest
//! of the engine queries. Only the *assignments* are data — the geometry math
//! for each shape lives in `block.rs`.

use std::{collections::HashMap, fs, path::PathBuf, sync::OnceLock};

use serde::Deserialize;

/// Default `blocks.json`, embedded so the client always has data even without
/// an external file. An on-disk `blocks.json` (see [`candidate_paths`]) takes
/// precedence so the data can be edited without recompiling.
const EMBEDDED_BLOCKS: &str = include_str!("blocks.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockFace {
    Top,
    Bottom,
    Side,
}

/// How a block is rendered/collided, assigned per-block in the data file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Cube,
    Cross,
    Rail,
    Ladder,
    Slab,
    Stairs,
    Layer,
    Fence,
    Pane,
    Cactus,
    Farmland,
    Lily,
    /// Entity-rendered blocks with no block geometry (signs, banners, skulls).
    None,
    Trapdoor,
    Chest,
    /// Pressure plates.
    Plate,
    Button,
    Cake,
    /// Flower pot.
    Pot,
    Anvil,
    Cauldron,
    Hopper,
    /// Nether portal panel (axis from meta).
    Portal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionKind {
    Auto,
    None,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderLayer {
    Solid,
    Cutout,
    Translucent,
}

/// Biome/constant tint applied to a face's texture.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Tint {
    #[default]
    None,
    Grass,
    Foliage,
    Water,
    Rgb([f32; 3]),
}

#[derive(Debug, Clone, Default)]
struct Faces {
    all: Option<String>,
    top: Option<String>,
    bottom: Option<String>,
    side: Option<String>,
}

impl Faces {
    fn get(&self, face: BlockFace) -> Option<&str> {
        match face {
            BlockFace::Top => self.top.as_deref().or(self.all.as_deref()),
            // Pillar-style blocks usually share top/bottom; fall back to top.
            BlockFace::Bottom => self
                .bottom
                .as_deref()
                .or(self.all.as_deref())
                .or(self.top.as_deref()),
            BlockFace::Side => self.side.as_deref().or(self.all.as_deref()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlockDef {
    pub shape: Shape,
    pub collision: CollisionKind,
    pub layer: RenderLayer,
    pub opaque: bool,
    pub alpha: f32,
    tint: TintFaces,
    tex: Faces,
    mask: u8,
    by_meta: Vec<Option<Faces>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct TintFaces {
    all: Tint,
    top: Tint,
    bottom: Tint,
    side: Tint,
}

impl TintFaces {
    fn get(&self, face: BlockFace) -> Tint {
        let specific = match face {
            BlockFace::Top => self.top,
            BlockFace::Bottom => self.bottom,
            BlockFace::Side => self.side,
        };
        match specific {
            Tint::None => self.all,
            other => other,
        }
    }
}

impl BlockDef {
    /// Resolve the texture base-name for a face at a given meta.
    pub fn texture_name(&self, meta: u8, face: BlockFace) -> Option<&str> {
        let index = (meta & self.mask) as usize;
        if let Some(Some(variant)) = self.by_meta.get(index) {
            if let Some(name) = variant.get(face) {
                return Some(name);
            }
        }
        self.tex.get(face)
    }

    pub fn tint(&self, face: BlockFace) -> Tint {
        self.tint.get(face)
    }
}

pub struct BlockRegistry {
    defs: HashMap<u16, BlockDef>,
}

impl BlockRegistry {
    pub fn def(&self, id: u16) -> Option<&BlockDef> {
        self.defs.get(&id)
    }

    /// Every unique texture base-name referenced by any block, for atlas build.
    pub fn all_texture_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut push = |name: &Option<String>| {
            if let Some(name) = name {
                if !names.iter().any(|existing| existing == name) {
                    names.push(name.clone());
                }
            }
        };
        for def in self.defs.values() {
            for faces in std::iter::once(&def.tex).chain(def.by_meta.iter().flatten()) {
                push(&faces.all);
                push(&faces.top);
                push(&faces.bottom);
                push(&faces.side);
            }
        }
        names.sort();
        names
    }

    fn load() -> Self {
        let json = read_external().unwrap_or_else(|| EMBEDDED_BLOCKS.to_owned());
        match parse(&json) {
            Ok(registry) => registry,
            Err(err) => {
                log::error!("failed to parse blocks.json ({err}); falling back to embedded data");
                parse(EMBEDDED_BLOCKS).expect("embedded blocks.json must parse")
            }
        }
    }
}

static REGISTRY: OnceLock<BlockRegistry> = OnceLock::new();

pub fn registry() -> &'static BlockRegistry {
    REGISTRY.get_or_init(BlockRegistry::load)
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("blocks.json"),
        PathBuf::from("data/blocks.json"),
    ];
    if let Some(dir) = std::env::var_os("RECRAFT_BLOCKS") {
        paths.insert(0, PathBuf::from(dir));
    }
    paths
}

fn read_external() -> Option<String> {
    for path in candidate_paths() {
        if let Ok(text) = fs::read_to_string(&path) {
            log::info!("loaded block data from {}", path.display());
            return Some(text);
        }
    }
    None
}

// --- JSON schema (raw, before resolution) ---

#[derive(Deserialize)]
struct RawData {
    blocks: Vec<RawBlock>,
}

#[derive(Deserialize)]
struct RawBlock {
    id: u16,
    #[serde(default = "default_shape")]
    shape: String,
    #[serde(default = "default_collision")]
    collision: String,
    #[serde(default = "default_layer")]
    layer: String,
    opaque: Option<bool>,
    alpha: Option<f32>,
    tex: Option<TexSpec>,
    tint: Option<TexSpec>,
    #[serde(default = "default_mask")]
    mask: u8,
    #[serde(default)]
    by_meta: Vec<Option<TexSpec>>,
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum TexSpec {
    All(String),
    Faces(FacesSpec),
}

#[derive(Deserialize, Clone, Default)]
struct FacesSpec {
    all: Option<String>,
    top: Option<String>,
    bottom: Option<String>,
    side: Option<String>,
}

fn default_shape() -> String {
    "cube".to_owned()
}
fn default_collision() -> String {
    "auto".to_owned()
}
fn default_layer() -> String {
    "solid".to_owned()
}
fn default_mask() -> u8 {
    15
}

fn parse(json: &str) -> Result<BlockRegistry, serde_json::Error> {
    let raw: RawData = serde_json::from_str(json)?;
    let mut defs = HashMap::new();
    for block in raw.blocks {
        defs.insert(block.id, resolve(block));
    }
    Ok(BlockRegistry { defs })
}

fn resolve(raw: RawBlock) -> BlockDef {
    let shape = parse_shape(&raw.shape);
    let layer = parse_layer(&raw.layer);
    let collision = parse_collision(&raw.collision);
    let opaque = raw
        .opaque
        .unwrap_or(shape == Shape::Cube && layer == RenderLayer::Solid);
    BlockDef {
        shape,
        collision,
        layer,
        opaque,
        alpha: raw.alpha.unwrap_or(1.0),
        tint: raw.tint.map(tint_faces).unwrap_or_default(),
        tex: raw.tex.map(faces_from_spec).unwrap_or_default(),
        mask: raw.mask,
        by_meta: raw
            .by_meta
            .into_iter()
            .map(|entry| entry.map(faces_from_spec))
            .collect(),
    }
}

fn faces_from_spec(spec: TexSpec) -> Faces {
    match spec {
        TexSpec::All(name) => Faces {
            all: Some(name),
            ..Faces::default()
        },
        TexSpec::Faces(f) => Faces {
            all: f.all,
            top: f.top,
            bottom: f.bottom,
            side: f.side,
        },
    }
}

fn tint_faces(spec: TexSpec) -> TintFaces {
    match spec {
        TexSpec::All(name) => TintFaces {
            all: parse_tint(&name),
            ..TintFaces::default()
        },
        TexSpec::Faces(f) => TintFaces {
            all: f.all.as_deref().map(parse_tint).unwrap_or(Tint::None),
            top: f.top.as_deref().map(parse_tint).unwrap_or(Tint::None),
            bottom: f.bottom.as_deref().map(parse_tint).unwrap_or(Tint::None),
            side: f.side.as_deref().map(parse_tint).unwrap_or(Tint::None),
        },
    }
}

fn parse_tint(value: &str) -> Tint {
    match value {
        "grass" => Tint::Grass,
        "foliage" => Tint::Foliage,
        "water" => Tint::Water,
        hex if hex.starts_with('#') && hex.len() == 7 => {
            let channel = |range| u8::from_str_radix(&hex[range], 16).unwrap_or(255) as f32 / 255.0;
            Tint::Rgb([channel(1..3), channel(3..5), channel(5..7)])
        }
        _ => Tint::None,
    }
}

fn parse_shape(value: &str) -> Shape {
    match value {
        "cross" => Shape::Cross,
        "rail" => Shape::Rail,
        "ladder" => Shape::Ladder,
        "slab" => Shape::Slab,
        "stairs" => Shape::Stairs,
        "layer" => Shape::Layer,
        "fence" => Shape::Fence,
        "pane" => Shape::Pane,
        "cactus" => Shape::Cactus,
        "farmland" => Shape::Farmland,
        "lily" => Shape::Lily,
        "none" => Shape::None,
        "trapdoor" => Shape::Trapdoor,
        "chest" => Shape::Chest,
        "plate" => Shape::Plate,
        "button" => Shape::Button,
        "cake" => Shape::Cake,
        "pot" => Shape::Pot,
        "anvil" => Shape::Anvil,
        "cauldron" => Shape::Cauldron,
        "hopper" => Shape::Hopper,
        "portal" => Shape::Portal,
        _ => Shape::Cube,
    }
}

fn parse_layer(value: &str) -> RenderLayer {
    match value {
        "cutout" => RenderLayer::Cutout,
        "translucent" => RenderLayer::Translucent,
        _ => RenderLayer::Solid,
    }
}

fn parse_collision(value: &str) -> CollisionKind {
    match value {
        "none" => CollisionKind::None,
        "full" => CollisionKind::Full,
        _ => CollisionKind::Auto,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_data_parses() {
        let registry = parse(EMBEDDED_BLOCKS).expect("embedded data parses");
        // Grass tints its top face and uses the dirt bottom.
        let grass = registry.def(2).unwrap();
        assert_eq!(grass.texture_name(0, BlockFace::Top), Some("grass_top"));
        assert_eq!(grass.texture_name(0, BlockFace::Bottom), Some("dirt"));
        assert_eq!(grass.tint(BlockFace::Top), Tint::Grass);
        // Wool selects its texture by meta color.
        let wool = registry.def(35).unwrap();
        assert_eq!(
            wool.texture_name(14, BlockFace::Side),
            Some("wool_colored_red")
        );
        // Logs share top/bottom and vary the side by wood type.
        let log = registry.def(17).unwrap();
        assert_eq!(log.texture_name(2, BlockFace::Side), Some("log_birch"));
        assert_eq!(
            log.texture_name(2, BlockFace::Bottom),
            Some("log_birch_top")
        );
        // Water is a non-colliding translucent block.
        let water = registry.def(8).unwrap();
        assert_eq!(water.collision, CollisionKind::None);
        assert_eq!(water.layer, RenderLayer::Translucent);
    }

    #[test]
    fn atlas_names_are_unique_and_sorted() {
        let registry = parse(EMBEDDED_BLOCKS).unwrap();
        let names = registry.all_texture_names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        for window in names.windows(2) {
            assert_ne!(window[0], window[1]);
        }
    }
}
