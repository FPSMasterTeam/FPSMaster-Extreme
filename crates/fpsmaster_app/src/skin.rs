//! Per-player skin downloading and atlas-row allocation. Each tab-list roster
//! entry carries a signed GameProfile `textures` property; this manager decodes
//! it on a background thread, downloads and normalizes the skin PNG, then hands
//! it back to the render loop, which uploads it into a per-player atlas row and
//! records the uuid→row mapping the entity model resolves skins through.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base64::Engine;
use fpsmaster_render::texture::{normalize_skin_png, PLAYER_SKIN_SLOTS};

/// Background download threads shared by every skin request (a lobby join used
/// to spawn one thread per player).
const DOWNLOAD_THREADS: usize = 4;
/// Whole-request timeout for one skin download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);
/// Decoded skins kept in memory (16 KiB each) so a player whose atlas row was
/// recycled gets it back without a re-download.
const MAX_CACHED_SKINS: usize = 1024;

/// A decoded skin ready to upload: the owner uuid and 64×64 RGBA bytes.
struct LoadedSkin {
    uuid: [u8; 16],
    rgba: Vec<u8>,
}

struct DownloadJob {
    uuid: [u8; 16],
    property: String,
}

pub struct SkinManager {
    /// Allocated atlas row per player uuid (the value passed to the model).
    rows: HashMap<[u8; 16], u32>,
    /// Current owner uuid of each atlas row, so recycling a row can evict the
    /// stale uuid→row mapping (two uuids must never resolve to one row).
    row_owner: Vec<Option<[u8; 16]>>,
    /// Last `poll` frame each row's owner was wanted on screen; the least
    /// recently wanted row is recycled first.
    row_used: Vec<u64>,
    frame: u64,
    /// Downloaded skins by uuid, resident in a row or not.
    skins: HashMap<[u8; 16], Vec<u8>>,
    /// uuids already requested (downloading or done) — avoids re-fetching.
    requested: HashSet<[u8; 16]>,
    jobs: Sender<DownloadJob>,
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
        let (jobs, job_rx) = mpsc::channel::<DownloadJob>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let agent = ureq::AgentBuilder::new().timeout(DOWNLOAD_TIMEOUT).build();
        for index in 0..DOWNLOAD_THREADS {
            let job_rx = Arc::clone(&job_rx);
            let tx: Sender<LoadedSkin> = tx.clone();
            let agent = agent.clone();
            let spawned = thread::Builder::new()
                .name(format!("skin-download-{index}"))
                .spawn(move || loop {
                    let job = match job_rx.lock().unwrap_or_else(|e| e.into_inner()).recv() {
                        Ok(job) => job,
                        Err(_) => break, // manager dropped
                    };
                    if let Some(rgba) = download_skin(&agent, &job.property) {
                        if tx.send(LoadedSkin { uuid: job.uuid, rgba }).is_err() {
                            break;
                        }
                    }
                });
            if let Err(err) = spawned {
                log::warn!("skin download thread failed to start: {err}");
            }
        }
        Self {
            rows: HashMap::new(),
            row_owner: vec![None; PLAYER_SKIN_SLOTS as usize],
            row_used: vec![0; PLAYER_SKIN_SLOTS as usize],
            frame: 0,
            skins: HashMap::new(),
            requested: HashSet::new(),
            jobs,
            rx,
        }
    }

    /// Whether a skin download has already been started for this uuid.
    pub fn is_requested(&self, uuid: &[u8; 16]) -> bool {
        self.requested.contains(uuid)
    }

    /// Queue a background download for `uuid` from its base64 `textures`
    /// property. No-op if already requested.
    pub fn request(&mut self, uuid: [u8; 16], texture_property: &str) {
        if !self.requested.insert(uuid) {
            return;
        }
        let job = DownloadJob {
            uuid,
            property: texture_property.to_owned(),
        };
        if self.jobs.send(job).is_err() {
            log::warn!("skin download threads are gone; skin not fetched");
        }
    }

    /// Take finished downloads and keep the atlas rows assigned to the skins in
    /// use. `wanted` are the uuids drawn this frame (spawned players, skulls,
    /// the local player): each gets a row, recycling the least recently wanted
    /// one when all are taken. Other downloaded skins only take free rows.
    /// Call once per frame; `upload` writes a skin into an atlas row.
    pub fn poll(&mut self, upload: &mut dyn FnMut(u32, &[u8]), wanted: &HashSet<[u8; 16]>) {
        self.frame += 1;
        let mut fresh = Vec::new();
        while let Ok(skin) = self.rx.try_recv() {
            if let Some(&row) = self.rows.get(&skin.uuid) {
                upload(row, &skin.rgba);
            } else {
                fresh.push(skin.uuid);
            }
            self.skins.insert(skin.uuid, skin.rgba);
        }
        for uuid in wanted {
            if let Some(&row) = self.rows.get(uuid) {
                self.row_used[row as usize] = self.frame;
            }
        }
        for uuid in wanted {
            if !self.rows.contains_key(uuid) && self.skins.contains_key(uuid) {
                let Some(row) = self.pick_row(true) else { break };
                self.assign(upload, row, *uuid);
            }
        }
        for uuid in fresh {
            if !self.rows.contains_key(&uuid) {
                let Some(row) = self.pick_row(false) else { break };
                self.assign(upload, row, uuid);
            }
        }
        self.trim_cache();
    }

    /// A free row, or (if `recycle`) the least recently wanted row not wanted
    /// this frame.
    fn pick_row(&self, recycle: bool) -> Option<u32> {
        if let Some(free) = self.row_owner.iter().position(Option::is_none) {
            return Some(free as u32);
        }
        if !recycle {
            return None;
        }
        (0..self.row_owner.len())
            .filter(|&row| self.row_used[row] != self.frame)
            .min_by_key(|&row| self.row_used[row])
            .map(|row| row as u32)
    }

    fn assign(&mut self, upload: &mut dyn FnMut(u32, &[u8]), row: u32, uuid: [u8; 16]) {
        let Some(rgba) = self.skins.get(&uuid) else { return };
        // Recycling a row: drop the previous owner's mapping so it never
        // samples another player's skin. Its decoded skin stays cached, so it
        // gets a row back (without a download) once it is wanted again.
        if let Some(old) = self.row_owner[row as usize].replace(uuid) {
            self.rows.remove(&old);
        }
        self.row_used[row as usize] = self.frame;
        self.rows.insert(uuid, row);
        upload(row, rgba);
    }

    /// Bound the decoded-skin cache: drop skins not resident in a row and
    /// forget they were requested, so they are fetched again if needed.
    fn trim_cache(&mut self) {
        if self.skins.len() <= MAX_CACHED_SKINS {
            return;
        }
        let excess = self.skins.len() - MAX_CACHED_SKINS;
        let drop: Vec<[u8; 16]> = self
            .skins
            .keys()
            .filter(|uuid| !self.rows.contains_key(*uuid))
            .take(excess)
            .copied()
            .collect();
        for uuid in drop {
            self.skins.remove(&uuid);
            self.requested.remove(&uuid);
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
fn download_skin(agent: &ureq::Agent, texture_property: &str) -> Option<Vec<u8>> {
    let url = skin_url_from_property(texture_property)?;
    let mut bytes = Vec::new();
    agent
        .get(&url)
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

    fn uuid(n: u32) -> [u8; 16] {
        let mut id = [0u8; 16];
        id[..4].copy_from_slice(&n.to_le_bytes());
        id
    }

    /// With more players than atlas rows, rows go to the players on screen; a
    /// player whose row was recycled gets one back from the cache (no
    /// re-download) once it is wanted again, and nothing churns while the
    /// wanted set is stable.
    #[test]
    fn recycled_rows_return_to_wanted_players() {
        let slots = PLAYER_SKIN_SLOTS;
        let mut manager = SkinManager::new();
        for n in 0..slots + 10 {
            manager.skins.insert(uuid(n), vec![0; 4]);
            manager.requested.insert(uuid(n));
        }
        let mut uploads = 0;
        let first: HashSet<_> = (0..slots).map(uuid).collect();
        manager.poll(&mut |_, _| uploads += 1, &first);
        assert_eq!(uploads, slots);
        assert!(first.iter().all(|id| manager.rows().contains_key(id)));

        // Ten newcomers on screen take the rows of ten players who left.
        let second: HashSet<_> = (10..slots + 10).map(uuid).collect();
        manager.poll(&mut |_, _| uploads += 1, &second);
        assert_eq!(uploads, slots + 10);
        assert!(second.iter().all(|id| manager.rows().contains_key(id)));
        assert!(!manager.rows().contains_key(&uuid(0)));

        // Stable wanted set: no further uploads.
        manager.poll(&mut |_, _| uploads += 1, &second);
        assert_eq!(uploads, slots + 10);

        // A recycled player comes back into view and gets a row again.
        let mut third = second.clone();
        third.remove(&uuid(slots + 9));
        third.insert(uuid(0));
        manager.poll(&mut |_, _| uploads += 1, &third);
        assert!(manager.rows().contains_key(&uuid(0)));
        assert!(manager.is_requested(&uuid(0)), "served from cache, not re-fetched");
    }

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
