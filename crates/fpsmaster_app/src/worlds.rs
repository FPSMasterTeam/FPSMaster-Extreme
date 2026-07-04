//! Persistence for the single-player world list shown on the world-select
//! screen. Only lightweight metadata — a display name and a terrain seed — is
//! stored; generated worlds are ephemeral (block edits are not saved), so
//! re-entering a world regenerates identical terrain from its seed. Stored as a
//! small JSON file next to the options file.

use std::path::Path;

const WORLDS_FILE: &str = "fpsmaster_worlds.json";

#[derive(Clone)]
pub struct WorldEntry {
    pub name: String,
    pub seed: i64,
}

pub fn load() -> Vec<WorldEntry> {
    load_from(Path::new(WORLDS_FILE))
}

fn load_from(path: &Path) -> Vec<WorldEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&text)
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.to_string();
            let seed = item.get("seed")?.as_i64()?;
            (!name.is_empty()).then_some(WorldEntry { name, seed })
        })
        .collect()
}

pub fn save(worlds: &[WorldEntry]) {
    save_to(Path::new(WORLDS_FILE), worlds);
}

fn save_to(path: &Path, worlds: &[WorldEntry]) {
    let array: Vec<serde_json::Value> = worlds
        .iter()
        .map(|w| serde_json::json!({ "name": w.name, "seed": w.seed }))
        .collect();
    if let Ok(text) = serde_json::to_string_pretty(&serde_json::Value::Array(array)) {
        let _ = std::fs::write(path, text);
    }
}

/// A fresh pseudo-random terrain seed from the wall clock — enough to get a
/// different world each time without pulling in an RNG crate.
pub fn random_seed() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as u64;
    // Mix so seeds differ in more than their low bits.
    ((nanos ^ (nanos >> 29)).wrapping_mul(0x9e37_79b9_7f4a_7c15)) as i64
}

/// Append a new world with a unique default name and a random seed, persist the
/// list, and return the new entry.
pub fn create(worlds: &mut Vec<WorldEntry>) -> WorldEntry {
    let mut name = "New World".to_string();
    let mut n = 1;
    while worlds.iter().any(|w| w.name == name) {
        n += 1;
        name = format!("New World {n}");
    }
    let entry = WorldEntry {
        name,
        seed: random_seed(),
    };
    worlds.push(entry.clone());
    save(worlds);
    entry
}
