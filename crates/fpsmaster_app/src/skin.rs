//! Per-player skin downloading and atlas-row allocation. Each tab-list roster
//! entry carries a signed GameProfile `textures` property; this manager decodes
//! it on a background thread, downloads and normalizes the skin PNG, then hands
//! it back to the render loop, which uploads it into a per-player atlas row and
//! records the uuid→row mapping the entity model resolves skins through.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use base64::Engine;
use fpsmaster_render::texture::{normalize_skin_png, PLAYER_SKIN_SLOTS};
use fpsmaster_render::Renderer;

/// A decoded skin ready to upload: the owner uuid and 64×64 RGBA bytes.
struct LoadedSkin {
    uuid: [u8; 16],
    rgba: Vec<u8>,
}

pub struct SkinManager {
    /// Allocated atlas row per player uuid (the value passed to the model).
    rows: HashMap<[u8; 16], u32>,
    /// Current owner uuid of each atlas row, so recycling a row can evict the
    /// stale uuid→row mapping (two uuids must never resolve to one row).
    row_owner: Vec<Option<[u8; 16]>>,
    /// uuids already requested (downloading or done) — avoids re-fetching.
    requested: HashSet<[u8; 16]>,
    /// Next free row in the per-player region, cycling round-robin.
    next_row: u32,
    tx: Sender<LoadedSkin>,
    rx: Receiver<LoadedSkin>,
}

impl Default for SkinManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SkinManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            rows: HashMap::new(),
            row_owner: vec![None; PLAYER_SKIN_SLOTS as usize],
            requested: HashSet::new(),
            next_row: 0,
            tx,
            rx,
        }
    }

    /// Whether a skin download has already been started for this uuid.
    pub fn is_requested(&self, uuid: &[u8; 16]) -> bool {
        self.requested.contains(uuid)
    }

    /// Kick off a background download for `uuid` from its base64 `textures`
    /// property. No-op if already requested.
    pub fn request(&mut self, uuid: [u8; 16], texture_property: &str) {
        if !self.requested.insert(uuid) {
            return;
        }
        let property = texture_property.to_owned();
        let tx = self.tx.clone();
        thread::spawn(move || {
            if let Some(rgba) = download_skin(&property) {
                let _ = tx.send(LoadedSkin { uuid, rgba });
            }
        });
    }

    /// Upload any completed downloads into their atlas rows and record the
    /// uuid→row mapping. Call once per frame with the renderer.
    pub fn poll(&mut self, renderer: &mut Renderer) {
        while let Ok(skin) = self.rx.try_recv() {
            let row = match self.rows.get(&skin.uuid) {
                Some(&row) => row,
                None => {
                    let row = self.next_row;
                    self.next_row = (self.next_row + 1) % PLAYER_SKIN_SLOTS;
                    // Recycling a row: drop the previous owner's mapping so it
                    // never samples another player's skin (it falls back to the
                    // default player slot until re-fetched).
                    if let Some(old) = self.row_owner[row as usize].take() {
                        self.rows.remove(&old);
                    }
                    self.row_owner[row as usize] = Some(skin.uuid);
                    self.rows.insert(skin.uuid, row);
                    row
                }
            };
            renderer.upload_player_skin(row, &skin.rgba);
        }
    }

    /// The uuid→atlas-row map the entity model resolves player skins through.
    pub fn rows(&self) -> &HashMap<[u8; 16], u32> {
        &self.rows
    }
}

/// Decode the base64 `textures` property and extract the SKIN url.
fn skin_url_from_property(texture_property: &str) -> Option<String> {
    let json = base64::engine::general_purpose::STANDARD
        .decode(texture_property)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&json).ok()?;
    Some(
        value
            .get("textures")?
            .get("SKIN")?
            .get("url")?
            .as_str()?
            .to_owned(),
    )
}

/// Decode the `textures` property, download the SKIN PNG and normalize it to a
/// 64×64 RGBA skin.
fn download_skin(texture_property: &str) -> Option<Vec<u8>> {
    let url = skin_url_from_property(texture_property)?;
    let mut bytes = Vec::new();
    ureq::get(&url)
        .call()
        .ok()?
        .into_reader()
        .take(512 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    normalize_skin_png(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_skin_url_from_a_textures_property() {
        let json = r#"{"timestamp":1,"profileId":"abc","profileName":"Notch",
            "textures":{"SKIN":{"url":"http://textures.minecraft.net/texture/deadbeef"}}}"#;
        let property = base64::engine::general_purpose::STANDARD.encode(json);
        assert_eq!(
            skin_url_from_property(&property).as_deref(),
            Some("http://textures.minecraft.net/texture/deadbeef")
        );
    }

    #[test]
    fn rejects_malformed_or_skinless_properties() {
        assert_eq!(skin_url_from_property("not base64 !!!"), None);
        let no_skin = base64::engine::general_purpose::STANDARD.encode(r#"{"textures":{}}"#);
        assert_eq!(skin_url_from_property(&no_skin), None);
    }
}
