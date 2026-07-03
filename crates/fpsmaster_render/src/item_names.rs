//! Display names and tooltip lines for inventory items. Vanilla resolves an
//! item's name from the lang file via its unlocalized key; fpsmaster has no
//! numeric-id → key registry, so this maps the common 1.8.9 blocks/items to
//! their exact vanilla English names and falls back to a title-cased texture
//! base-name for the long tail. NBT is parsed for custom names, enchantments,
//! lore, unbreakable, and HideFlags.

use fpsmaster_core::{BlockFace, BlockState};
use fpsmaster_protocol::nbt::{NbtCompound, NbtTag};
use fpsmaster_protocol::v1_8_9::packets::SlotItem;

use crate::texture::item_texture_name;

/// The vanilla display name for an item/block stack (id + damage/meta),
/// localized to the active language where the name maps to a known lang key.
pub fn item_display_name(id: i16, damage: i16) -> String {
    crate::i18n::localize_name(&item_display_name_en(id, damage))
}

/// The hardcoded vanilla English name (the localization source / fallback).
fn item_display_name_en(id: i16, damage: i16) -> String {
    if (1..256).contains(&id) {
        let meta = (damage.max(0) & 15) as u8;
        if let Some(name) = block_name(id, meta) {
            return name.to_owned();
        }
        // Fall back to the block's top-face texture, title-cased.
        if let Some(tex) = BlockState::new(id as u16, meta).texture_name(BlockFace::Top) {
            return title_case(tex);
        }
        return format!("Block {id}");
    }
    if let Some(name) = item_name(id) {
        return name.to_owned();
    }
    match item_texture_name(id) {
        Some(tex) => title_case(tex),
        None => format!("Item {id}"),
    }
}

/// Vanilla names for the common blocks (with metadata variants). `None` falls
/// back to a derived name.
fn block_name(id: i16, meta: u8) -> Option<&'static str> {
    Some(match (id, meta) {
        (1, 0) => "Stone",
        (1, 1) => "Granite",
        (1, 2) => "Polished Granite",
        (1, 3) => "Diorite",
        (1, 4) => "Polished Diorite",
        (1, 5) => "Andesite",
        (1, 6) => "Polished Andesite",
        (2, _) => "Grass Block",
        (3, 1) => "Coarse Dirt",
        (3, 2) => "Podzol",
        (3, _) => "Dirt",
        (4, _) => "Cobblestone",
        (5, 0) => "Oak Wood Planks",
        (5, 1) => "Spruce Wood Planks",
        (5, 2) => "Birch Wood Planks",
        (5, 3) => "Jungle Wood Planks",
        (5, 4) => "Acacia Wood Planks",
        (5, 5) => "Dark Oak Wood Planks",
        (6, 0) => "Oak Sapling",
        (6, 1) => "Spruce Sapling",
        (6, 2) => "Birch Sapling",
        (6, 3) => "Jungle Sapling",
        (6, 4) => "Acacia Sapling",
        (6, 5) => "Dark Oak Sapling",
        (7, _) => "Bedrock",
        (8, _) | (9, _) => "Water",
        (10, _) | (11, _) => "Lava",
        (12, 1) => "Red Sand",
        (12, _) => "Sand",
        (13, _) => "Gravel",
        (14, _) => "Gold Ore",
        (15, _) => "Iron Ore",
        (16, _) => "Coal Ore",
        (17, 0) => "Oak Wood",
        (17, 1) => "Spruce Wood",
        (17, 2) => "Birch Wood",
        (17, 3) => "Jungle Wood",
        (18, 1) => "Spruce Leaves",
        (18, 2) => "Birch Leaves",
        (18, 3) => "Jungle Leaves",
        (18, _) => "Oak Leaves",
        (19, 1) => "Wet Sponge",
        (19, _) => "Sponge",
        (20, _) => "Glass",
        (21, _) => "Lapis Lazuli Ore",
        (22, _) => "Lapis Lazuli Block",
        (23, _) => "Dispenser",
        (24, 1) => "Chiseled Sandstone",
        (24, 2) => "Smooth Sandstone",
        (24, _) => "Sandstone",
        (25, _) => "Note Block",
        (27, _) => "Powered Rail",
        (28, _) => "Detector Rail",
        (29, _) => "Sticky Piston",
        (30, _) => "Cobweb",
        (31, 2) => "Fern",
        (31, _) => "Grass",
        (32, _) => "Dead Bush",
        (33, _) => "Piston",
        (35, _) => return Some(wool_name(meta)),
        (37, _) => "Dandelion",
        (38, 0) => "Poppy",
        (38, 1) => "Blue Orchid",
        (38, 2) => "Allium",
        (38, 3) => "Azure Bluet",
        (38, 4) => "Red Tulip",
        (38, 5) => "Orange Tulip",
        (38, 6) => "White Tulip",
        (38, 7) => "Pink Tulip",
        (38, 8) => "Oxeye Daisy",
        (39, _) => "Brown Mushroom",
        (40, _) => "Red Mushroom",
        (41, _) => "Block of Gold",
        (42, _) => "Block of Iron",
        (44, _) | (43, _) => "Stone Slab",
        (45, _) => "Bricks",
        (46, _) => "TNT",
        (47, _) => "Bookshelf",
        (48, _) => "Moss Stone",
        (49, _) => "Obsidian",
        (50, _) => "Torch",
        (53, _) => "Oak Wood Stairs",
        (54, _) => "Chest",
        (56, _) => "Diamond Ore",
        (57, _) => "Block of Diamond",
        (58, _) => "Crafting Table",
        (60, _) => "Farmland",
        (61, _) => "Furnace",
        (65, _) => "Ladder",
        (66, _) => "Rail",
        (73, _) | (74, _) => "Redstone Ore",
        (78, _) => "Snow",
        (79, _) => "Ice",
        (80, _) => "Snow Block",
        (81, _) => "Cactus",
        (82, _) => "Clay",
        (84, _) => "Jukebox",
        (85, _) => "Oak Fence",
        (86, _) => "Pumpkin",
        (87, _) => "Netherrack",
        (88, _) => "Soul Sand",
        (89, _) => "Glowstone",
        (95, _) => return Some(stained_glass_name(meta)),
        (98, 1) => "Mossy Stone Bricks",
        (98, 2) => "Cracked Stone Bricks",
        (98, 3) => "Chiseled Stone Bricks",
        (98, _) => "Stone Bricks",
        (102, _) => "Glass Pane",
        (103, _) => "Melon",
        (110, _) => "Mycelium",
        (112, _) => "Nether Brick",
        (121, _) => "End Stone",
        (129, _) => "Emerald Ore",
        (133, _) => "Block of Emerald",
        (152, _) => "Block of Redstone",
        (155, 1) => "Chiseled Quartz Block",
        (155, 2) => "Pillar Quartz Block",
        (155, _) => "Block of Quartz",
        (159, _) => return Some(stained_clay_name(meta)),
        (162, 1) => "Acacia Wood",
        (162, _) => "Dark Oak Wood",
        (170, _) => "Hay Bale",
        (172, _) => "Hardened Clay",
        (173, _) => "Block of Coal",
        _ => return None,
    })
}

fn wool_name(meta: u8) -> &'static str {
    // Metadata 0 (white) is just "Wool" in 1.8.9; the rest are "<Color> Wool".
    match meta & 15 {
        0 => "Wool",
        1 => "Orange Wool",
        2 => "Magenta Wool",
        3 => "Light Blue Wool",
        4 => "Yellow Wool",
        5 => "Lime Wool",
        6 => "Pink Wool",
        7 => "Gray Wool",
        8 => "Light Gray Wool",
        9 => "Cyan Wool",
        10 => "Purple Wool",
        11 => "Blue Wool",
        12 => "Brown Wool",
        13 => "Green Wool",
        14 => "Red Wool",
        _ => "Black Wool",
    }
}

fn stained_glass_name(meta: u8) -> &'static str {
    STAINED_GLASS[(meta & 15) as usize]
}

fn stained_clay_name(meta: u8) -> &'static str {
    STAINED_CLAY[(meta & 15) as usize]
}

const STAINED_GLASS: [&str; 16] = [
    "White Stained Glass", "Orange Stained Glass", "Magenta Stained Glass",
    "Light Blue Stained Glass", "Yellow Stained Glass", "Lime Stained Glass",
    "Pink Stained Glass", "Gray Stained Glass", "Light Gray Stained Glass",
    "Cyan Stained Glass", "Purple Stained Glass", "Blue Stained Glass",
    "Brown Stained Glass", "Green Stained Glass", "Red Stained Glass",
    "Black Stained Glass",
];

const STAINED_CLAY: [&str; 16] = [
    "White Stained Clay", "Orange Stained Clay", "Magenta Stained Clay",
    "Light Blue Stained Clay", "Yellow Stained Clay", "Lime Stained Clay",
    "Pink Stained Clay", "Gray Stained Clay", "Light Gray Stained Clay",
    "Cyan Stained Clay", "Purple Stained Clay", "Blue Stained Clay",
    "Brown Stained Clay", "Green Stained Clay", "Red Stained Clay",
    "Black Stained Clay",
];

/// Vanilla names for the items whose display name differs from their texture
/// base-name (tools: wood→Wooden, gold→Golden; armor; bow). Everything else
/// title-cases cleanly from [`item_texture_name`].
fn item_name(id: i16) -> Option<&'static str> {
    Some(match id {
        261 => "Bow",
        // Wooden tool set (texture "wood_*").
        268 => "Wooden Sword",
        269 => "Wooden Shovel",
        270 => "Wooden Pickaxe",
        271 => "Wooden Axe",
        290 => "Wooden Hoe",
        // Golden tool set (texture "gold_*").
        283 => "Golden Sword",
        284 => "Golden Shovel",
        285 => "Golden Pickaxe",
        286 => "Golden Axe",
        294 => "Golden Hoe",
        // Armor.
        298 => "Leather Cap",
        299 => "Leather Tunic",
        300 => "Leather Pants",
        301 => "Leather Boots",
        302 => "Chainmail Helmet",
        303 => "Chainmail Chestplate",
        304 => "Chainmail Leggings",
        305 => "Chainmail Boots",
        306 => "Iron Helmet",
        307 => "Iron Chestplate",
        308 => "Iron Leggings",
        309 => "Iron Boots",
        310 => "Diamond Helmet",
        311 => "Diamond Chestplate",
        312 => "Diamond Leggings",
        313 => "Diamond Boots",
        314 => "Golden Helmet",
        315 => "Golden Chestplate",
        316 => "Golden Leggings",
        317 => "Golden Boots",
        _ => return None,
    })
}

/// "iron_shovel" → "Iron Shovel": split on `_`/` `, capitalize each word.
fn title_case(s: &str) -> String {
    s.split(['_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the vanilla-style tooltip lines for an item stack, including NBT data.
/// Each line may contain `§` formatting codes (rarity color, enchant gray, lore
/// purple/italic). The first line is always the display name with rarity color.
pub fn build_tooltip(item: &SlotItem) -> Vec<String> {
    let nbt = item.nbt.as_ref();
    let hide_flags = nbt
        .and_then(|t| t.get("HideFlags"))
        .and_then(|t| t.as_i32())
        .unwrap_or(0);

    let mut lines = Vec::new();

    // Line 1: display name with rarity color.
    let has_custom_name = has_display_name(nbt);
    let name = if has_custom_name {
        custom_display_name(nbt).unwrap_or_default()
    } else {
        item_display_name(item.id, item.damage)
    };

    let rarity_color = item_rarity_color(item);
    let name_line = if has_custom_name {
        // Custom names are italic.
        format!("{rarity_color}\u{00a7}o{name}\u{00a7}r")
    } else {
        format!("{rarity_color}{name}\u{00a7}r")
    };
    lines.push(name_line);

    // Enchantments (flag bit 0).
    if hide_flags & 1 == 0 {
        if let Some(ench_list) = nbt
            .and_then(|t| t.get("ench"))
            .and_then(|t| t.as_list())
        {
            for ench in ench_list {
                if let NbtTag::Compound(c) = ench {
                    let id = c.get("id").and_then(|t| t.as_short()).unwrap_or(0);
                    let lvl = c.get("lvl").and_then(|t| t.as_short()).unwrap_or(0);
                    if let Some(name) = enchantment_name(id, lvl) {
                        lines.push(format!("\u{00a7}7{name}"));
                    }
                }
            }
        }
    }

    // Lore (under display compound).
    if let Some(display) = nbt.and_then(|t| t.get("display")).and_then(|t| t.as_compound()) {
        if let Some(lore_list) = display.get("Lore").and_then(|t| t.as_list()) {
            for lore_tag in lore_list {
                if let Some(text) = lore_tag.as_str() {
                    lines.push(format!("\u{00a7}5\u{00a7}o{text}"));
                }
            }
        }
    }

    // Unbreakable (flag bit 2).
    if hide_flags & 4 == 0 {
        if nbt.and_then(|t| t.get("Unbreakable")).map(|t| t.as_bool()).unwrap_or(false) {
            lines.push(format!("\u{00a7}9{}", crate::i18n::localize_name("Unbreakable")));
        }
    }

    // Attribute modifiers (flag bit 1).
    if hide_flags & 2 == 0 {
        let modifiers = if let Some(list) = nbt
            .and_then(|t| t.get("AttributeModifiers"))
            .and_then(|t| t.as_list())
        {
            nbt_attribute_modifiers(list)
        } else {
            default_attribute_modifiers(item.id)
        };
        if !modifiers.is_empty() {
            lines.push(String::new());
            for (attr, amount, operation) in modifiers {
                let display = format_modifier(&attr, amount, operation);
                if let Some(line) = display {
                    lines.push(line);
                }
            }
        }
    }

    lines
}

fn has_display_name(nbt: Option<&NbtCompound>) -> bool {
    nbt.and_then(|t| t.get("display"))
        .and_then(|t| t.as_compound())
        .and_then(|d| d.get("Name"))
        .and_then(|t| t.as_str())
        .is_some()
}

fn custom_display_name(nbt: Option<&NbtCompound>) -> Option<String> {
    nbt.and_then(|t| t.get("display"))
        .and_then(|t| t.as_compound())
        .and_then(|d| d.get("Name"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_owned())
}

/// Vanilla item rarity color code. Enchanted items are AQUA (rare),
/// everything else is WHITE (common) by default.
fn item_rarity_color(item: &SlotItem) -> &'static str {
    let is_enchanted = item
        .nbt
        .as_ref()
        .and_then(|t| t.get("ench"))
        .and_then(|t| t.as_list())
        .is_some_and(|l| !l.is_empty());
    if is_enchanted {
        "\u{00a7}b" // AQUA
    } else {
        "\u{00a7}f" // WHITE
    }
}

/// Read attribute modifiers from the NBT `AttributeModifiers` list.
fn nbt_attribute_modifiers(list: &[NbtTag]) -> Vec<(String, f64, i32)> {
    let mut out = Vec::new();
    for tag in list {
        if let NbtTag::Compound(c) = tag {
            let attr = match c.get("AttributeName").and_then(|t| t.as_str()) {
                Some(s) => s.to_owned(),
                None => continue,
            };
            let amount = c.get("Amount").and_then(|t| t.as_f64()).unwrap_or(0.0);
            let operation = c
                .get("Operation")
                .and_then(|t| t.as_i32())
                .unwrap_or(0);
            out.push((attr, amount, operation));
        }
    }
    out
}

/// Default attribute modifiers for vanilla weapons/tools (1.8.9).
fn default_attribute_modifiers(id: i16) -> Vec<(String, f64, i32)> {
    let damage = match id {
        // Swords: 4.0 + material bonus
        268 | 283 => 4.0,       // wood, gold
        272 => 5.0,             // stone
        267 => 6.0,             // iron
        276 => 7.0,             // diamond
        // Axes: 3.0 + material bonus
        271 | 286 => 3.0,       // wood, gold
        275 => 4.0,             // stone
        258 => 5.0,             // iron
        279 => 6.0,             // diamond
        // Pickaxes: 2.0 + material bonus
        270 | 285 => 2.0,       // wood, gold
        274 => 3.0,             // stone
        257 => 4.0,             // iron
        278 => 5.0,             // diamond
        // Shovels: 1.0 + material bonus
        269 | 284 => 1.0,       // wood, gold
        273 => 2.0,             // stone
        256 => 3.0,             // iron
        277 => 4.0,             // diamond
        _ => return Vec::new(),
    };
    vec![("generic.attackDamage".to_owned(), damage, 0)]
}

/// Format one attribute modifier line with the vanilla color/sign convention.
fn format_modifier(attr: &str, amount: f64, operation: i32) -> Option<String> {
    let mut display_amount = amount;

    if operation == 1 || operation == 2 {
        display_amount *= 100.0;
    } else if attr == "generic.attackDamage" {
        display_amount += 1.0; // player base attack damage
    }

    if display_amount.abs() < 1e-9 {
        return None;
    }

    let name = crate::i18n::localize_name(attribute_display_name(attr));
    let abs = display_amount.abs();
    let formatted = format_amount(abs);
    let pct = if operation == 1 || operation == 2 { "%" } else { "" };

    if display_amount > 0.0 {
        Some(format!("\u{00a7}9 +{formatted}{pct} {name}"))
    } else {
        Some(format!("\u{00a7}c -{formatted}{pct} {name}"))
    }
}

fn attribute_display_name(attr: &str) -> &str {
    match attr {
        "generic.attackDamage" => "Attack Damage",
        "generic.movementSpeed" => "Speed",
        "generic.maxHealth" => "Max Health",
        "generic.knockbackResistance" => "Knockback Resistance",
        "generic.followRange" => "Follow Range",
        _ => attr,
    }
}

fn format_amount(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0');
    s.trim_end_matches('.').to_owned()
}

fn enchantment_name(id: i16, level: i16) -> Option<String> {
    let base = match id {
        0 => "Protection",
        1 => "Fire Protection",
        2 => "Feather Falling",
        3 => "Blast Protection",
        4 => "Projectile Protection",
        5 => "Respiration",
        6 => "Aqua Affinity",
        7 => "Thorns",
        8 => "Depth Strider",
        16 => "Sharpness",
        17 => "Smite",
        18 => "Bane of Arthropods",
        19 => "Knockback",
        20 => "Fire Aspect",
        21 => "Looting",
        32 => "Efficiency",
        33 => "Silk Touch",
        34 => "Unbreaking",
        35 => "Fortune",
        48 => "Power",
        49 => "Punch",
        50 => "Flame",
        51 => "Infinity",
        61 => "Luck of the Sea",
        62 => "Lure",
        _ => return None,
    };
    let base = crate::i18n::localize_name(base);
    let base = base.as_str();
    let roman = match level {
        1 => "I",
        2 => "II",
        3 => "III",
        4 => "IV",
        5 => "V",
        6 => "VI",
        7 => "VII",
        8 => "VIII",
        9 => "IX",
        10 => "X",
        n => return Some(format!("{base} {n}")),
    };
    Some(format!("{base} {roman}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_use_vanilla_names_with_metadata() {
        assert_eq!(item_display_name(1, 0), "Stone");
        assert_eq!(item_display_name(1, 3), "Diorite");
        assert_eq!(item_display_name(5, 5), "Dark Oak Wood Planks");
        assert_eq!(item_display_name(35, 0), "Wool");
        assert_eq!(item_display_name(35, 14), "Red Wool");
        assert_eq!(item_display_name(159, 1), "Orange Stained Clay");
    }

    #[test]
    fn items_fix_systematic_name_differences() {
        assert_eq!(item_display_name(268, 0), "Wooden Sword");
        assert_eq!(item_display_name(283, 0), "Golden Sword");
        assert_eq!(item_display_name(261, 0), "Bow");
        // Title-cased straight from the texture name.
        assert_eq!(item_display_name(276, 0), "Diamond Sword");
        assert_eq!(item_display_name(265, 0), "Iron Ingot");
    }

    #[test]
    fn unknown_ids_fall_back_gracefully() {
        assert_eq!(item_display_name(20000, 0), "Item 20000");
    }

    #[test]
    fn tooltip_plain_item_shows_white_name() {
        let item = SlotItem::new(1, 64, 0);
        let lines = build_tooltip(&item);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "\u{00a7}fStone\u{00a7}r");
    }

    #[test]
    fn tooltip_custom_name_is_italic() {
        let mut nbt = NbtCompound::new();
        let mut display = NbtCompound::new();
        display.insert("Name".into(), NbtTag::String("My Sword".into()));
        nbt.insert("display".into(), NbtTag::Compound(display));
        let item = SlotItem { id: 276, count: 1, damage: 0, nbt: Some(nbt) };
        let lines = build_tooltip(&item);
        assert_eq!(lines[0], "\u{00a7}f\u{00a7}oMy Sword\u{00a7}r");
    }

    #[test]
    fn tooltip_enchanted_item_shows_aqua_name_and_enchantments() {
        let mut nbt = NbtCompound::new();
        let mut ench = NbtCompound::new();
        ench.insert("id".into(), NbtTag::Short(16)); // Sharpness
        ench.insert("lvl".into(), NbtTag::Short(5));
        nbt.insert("ench".into(), NbtTag::List(vec![NbtTag::Compound(ench)]));
        let item = SlotItem { id: 276, count: 1, damage: 0, nbt: Some(nbt) };
        let lines = build_tooltip(&item);
        // Aqua rarity for enchanted items.
        assert!(lines[0].starts_with("\u{00a7}b"));
        assert_eq!(lines[1], "\u{00a7}7Sharpness V");
    }

    #[test]
    fn tooltip_lore_lines_are_dark_purple_italic() {
        let mut nbt = NbtCompound::new();
        let mut display = NbtCompound::new();
        display.insert(
            "Lore".into(),
            NbtTag::List(vec![
                NbtTag::String("Line one".into()),
                NbtTag::String("Line two".into()),
            ]),
        );
        nbt.insert("display".into(), NbtTag::Compound(display));
        let item = SlotItem { id: 1, count: 1, damage: 0, nbt: Some(nbt) };
        let lines = build_tooltip(&item);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1], "\u{00a7}5\u{00a7}oLine one");
        assert_eq!(lines[2], "\u{00a7}5\u{00a7}oLine two");
    }

    #[test]
    fn tooltip_unbreakable_shown_in_blue() {
        let mut nbt = NbtCompound::new();
        nbt.insert("Unbreakable".into(), NbtTag::Byte(1));
        let item = SlotItem { id: 276, count: 1, damage: 0, nbt: Some(nbt) };
        let lines = build_tooltip(&item);
        assert!(lines.iter().any(|l| l == "\u{00a7}9Unbreakable"));
    }

    #[test]
    fn tooltip_hide_flags_suppresses_enchantments() {
        let mut nbt = NbtCompound::new();
        let mut ench = NbtCompound::new();
        ench.insert("id".into(), NbtTag::Short(16));
        ench.insert("lvl".into(), NbtTag::Short(5));
        nbt.insert("ench".into(), NbtTag::List(vec![NbtTag::Compound(ench)]));
        nbt.insert("HideFlags".into(), NbtTag::Int(1)); // hide enchantments
        let item = SlotItem { id: 276, count: 1, damage: 0, nbt: Some(nbt) };
        let lines = build_tooltip(&item);
        // Name + blank + attack damage (enchantments hidden, attributes shown).
        assert_eq!(lines.len(), 3);
        assert!(!lines.iter().any(|l| l.contains("Sharpness")));
        assert_eq!(lines[2], "\u{00a7}9 +8 Attack Damage");
    }

    #[test]
    fn tooltip_hide_flags_suppresses_attributes() {
        let mut nbt = NbtCompound::new();
        nbt.insert("HideFlags".into(), NbtTag::Int(2)); // hide attributes
        let item = SlotItem { id: 276, count: 1, damage: 0, nbt: Some(nbt) };
        let lines = build_tooltip(&item);
        assert_eq!(lines.len(), 1); // only the name
    }

    #[test]
    fn tooltip_diamond_sword_shows_attack_damage() {
        let item = SlotItem::new(276, 1, 0);
        let lines = build_tooltip(&item);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "\u{00a7}9 +8 Attack Damage");
    }
}
