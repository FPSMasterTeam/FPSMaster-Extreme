//! Sound playback for the 1.8.9 client.
//!
//! The renderer and core stay audio-free: `game.rs` queues [`QueuedSound`]
//! requests (event name, optional world position, volume, pitch) and `main.rs`
//! drains them each frame into the [`SoundManager`], which resolves the event
//! through the vanilla `sounds.json` map, applies vanilla distance attenuation
//! and stereo panning relative to the camera listener, and plays the ogg with
//! [`kira`].
//!
//! If no audio device can be opened (headless / CI) the manager degrades to a
//! silent no-op so the rest of the client runs unchanged.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use glam::Vec3;
use kira::sound::static_sound::StaticSoundData;
use kira::{AudioManager, AudioManagerSettings, DefaultBackend, Decibels, Panning, PlaybackRate};

/// Vanilla rolloff: a positioned sound at volume `1.0` is inaudible past this
/// many blocks; volume `> 1.0` extends the range proportionally.
const ATTENUATION_RANGE: f32 = 16.0;

/// A sound request produced by `game.rs`, drained and played by `main.rs`.
/// Carries no audio types so the game layer stays decoupled from the backend.
#[derive(Debug, Clone)]
pub struct QueuedSound {
    /// The `sounds.json` event key, e.g. `"dig.stone"` or `"random.click"`.
    pub event: String,
    /// World position of the emitter, or `None` for a non-positional UI sound.
    pub position: Option<Vec3>,
    /// Linear volume from the packet / event (1.0 = unattenuated).
    pub volume: f32,
    /// Playback-rate multiplier (1.0 = original pitch).
    pub pitch: f32,
}

/// One resolvable sound: the asset name (relative path without extension) plus
/// the per-sound volume/pitch/weight defaults from `sounds.json`.
#[derive(Debug, Clone)]
struct SoundEntry {
    name: String,
    volume: f32,
    pitch: f32,
    weight: u32,
}

/// The listener pose (camera). World and render coordinates are 1:1 in this
/// client, so the camera position doubles as the world-space listener position.
#[derive(Debug, Clone, Copy)]
pub struct Listener {
    pub position: Vec3,
    /// Camera yaw in degrees, Minecraft convention (matches `Camera::yaw`).
    pub yaw: f32,
}

pub struct SoundManager {
    /// `None` when no audio device could be opened — every play becomes a no-op.
    manager: Option<AudioManager<DefaultBackend>>,
    /// Event key -> the weighted sound variants for that event.
    events: HashMap<String, Vec<SoundEntry>>,
    /// Decoded ogg cache, keyed by sound asset name (e.g. `"dig/stone1"`).
    cache: HashMap<String, Option<StaticSoundData>>,
    /// Resolved asset roots to read ogg files from (directories or jars).
    asset_roots: Vec<PathBuf>,
    listener: Listener,
    /// Deterministic per-event variant picker (xorshift, no `rand` dependency).
    rng: u32,
}

impl SoundManager {
    pub fn new() -> Self {
        let asset_roots = candidate_asset_paths();
        let events = load_sound_events(&asset_roots);
        let manager = match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(manager) => {
                log::info!("audio: {} sound events loaded", events.len());
                Some(manager)
            }
            Err(err) => {
                log::warn!("audio: no output device ({err}); sound disabled");
                None
            }
        };
        Self {
            manager,
            events,
            cache: HashMap::new(),
            asset_roots,
            listener: Listener {
                position: Vec3::ZERO,
                yaw: 0.0,
            },
            rng: 0x1234_5678,
        }
    }

    pub fn set_listener(&mut self, listener: Listener) {
        self.listener = listener;
    }

    /// Resolve and play a queued sound. Positional sounds are attenuated and
    /// panned relative to the listener; out-of-range sounds are dropped.
    pub fn play(&mut self, sound: &QueuedSound) {
        if self.manager.is_none() {
            return;
        }
        let Some(entry) = self.pick_entry(&sound.event) else {
            return;
        };

        // Combine the packet volume with the event's per-sound volume.
        let base_volume = sound.volume * entry.volume;
        let (gain, panning) = match sound.position {
            Some(pos) => {
                let Some((gain, panning)) = spatial_gain_pan(self.listener, pos, base_volume)
                else {
                    return; // beyond the rolloff range — vanilla plays nothing
                };
                (gain, panning)
            }
            None => (base_volume.min(1.0), 0.0),
        };
        if gain <= 0.0 {
            return;
        }

        let Some(data) = self.sound_data(&entry.name) else {
            return;
        };
        let rate = (sound.pitch * entry.pitch).max(0.01) as f64;
        let data = data
            .volume(amplitude_to_decibels(gain))
            .panning(Panning(panning))
            .playback_rate(PlaybackRate(rate));

        if let Some(manager) = self.manager.as_mut() {
            // The handle is dropped immediately; the sound plays to completion.
            if let Err(err) = manager.play(data) {
                log::debug!("audio: play failed: {err}");
            }
        }
    }

    /// Pick one variant of an event by weight, using the deterministic xorshift.
    fn pick_entry(&mut self, event: &str) -> Option<SoundEntry> {
        let entries = self.events.get(event)?;
        if entries.is_empty() {
            return None;
        }
        let total: u32 = entries.iter().map(|e| e.weight.max(1)).sum();
        let mut roll = next_xorshift(&mut self.rng) % total;
        for entry in entries {
            let w = entry.weight.max(1);
            if roll < w {
                return Some(entry.clone());
            }
            roll -= w;
        }
        entries.last().cloned()
    }

    /// Decoded ogg for a sound asset name, loaded lazily and cached (including
    /// the negative result, so a missing file isn't retried every play).
    fn sound_data(&mut self, name: &str) -> Option<StaticSoundData> {
        if let Some(cached) = self.cache.get(name) {
            return cached.clone();
        }
        let data = load_ogg(&self.asset_roots, name);
        if data.is_none() {
            log::debug!("audio: missing sound asset {name}");
        }
        self.cache.insert(name.to_string(), data.clone());
        data
    }
}

/// Parse `sounds.json` into the event -> variants map. Pure over the JSON text
/// so it is unit-testable without a filesystem.
fn parse_sounds_json(text: &str) -> HashMap<String, Vec<SoundEntry>> {
    let mut events = HashMap::new();
    let Ok(root) = serde_json::from_str::<serde_json::Value>(text) else {
        return events;
    };
    let Some(map) = root.as_object() else {
        return events;
    };
    for (event, value) in map {
        let Some(list) = value.get("sounds").and_then(|s| s.as_array()) else {
            continue;
        };
        let mut entries = Vec::new();
        for sound in list {
            let entry = match sound {
                serde_json::Value::String(name) => SoundEntry {
                    name: name.clone(),
                    volume: 1.0,
                    pitch: 1.0,
                    weight: 1,
                },
                serde_json::Value::Object(obj) => {
                    let Some(name) = obj.get("name").and_then(|n| n.as_str()) else {
                        continue;
                    };
                    SoundEntry {
                        name: name.to_string(),
                        volume: obj.get("volume").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
                        pitch: obj.get("pitch").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
                        weight: obj.get("weight").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
                    }
                }
                _ => continue,
            };
            entries.push(entry);
        }
        if !entries.is_empty() {
            events.insert(event.clone(), entries);
        }
    }
    events
}

/// Vanilla distance attenuation + panning for a positioned sound.
///
/// Volume falls off linearly to zero at `16 * volume` blocks (clamped so a
/// `volume <= 1` sound reaches zero at 16 blocks). Returns `None` when the
/// emitter is out of range. Panning is `-1` (left) .. `+1` (right) from the
/// emitter's offset along the listener's right vector.
fn spatial_gain_pan(listener: Listener, emitter: Vec3, volume: f32) -> Option<(f32, f32)> {
    let range = ATTENUATION_RANGE * volume.max(1.0);
    let offset = emitter - listener.position;
    let distance = offset.length();
    if distance >= range {
        return None;
    }
    let gain = (volume * (1.0 - distance / range)).clamp(0.0, 1.0);
    if gain <= 0.0 {
        return None;
    }

    // Right vector from the listener yaw (Minecraft yaw: 0 faces +Z, and the
    // camera direction is (-sin yaw, .., cos yaw); right is its horizontal
    // perpendicular).
    let yaw = listener.yaw.to_radians();
    let right = Vec3::new(yaw.cos(), 0.0, yaw.sin());
    let horizontal = Vec3::new(offset.x, 0.0, offset.z);
    let panning = if horizontal.length() > 1e-4 {
        right.dot(horizontal.normalize()).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    Some((gain, panning))
}

/// Convert a linear amplitude (0..1) to kira's `Decibels`. `20*log10(amp)`,
/// with silence floored at -60 dB (kira's `Decibels::SILENCE`).
fn amplitude_to_decibels(amplitude: f32) -> Decibels {
    if amplitude <= 0.0 {
        return Decibels::SILENCE;
    }
    Decibels((20.0 * amplitude.log10()).max(Decibels::SILENCE.0))
}

/// xorshift32 — a tiny deterministic PRNG so weighted variant selection needs
/// no `rand` dependency.
fn next_xorshift(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Load `sounds.json` from the first asset root that has it.
fn load_sound_events(roots: &[PathBuf]) -> HashMap<String, Vec<SoundEntry>> {
    for root in roots {
        if let Some(bytes) = read_asset(root, "assets/minecraft/sounds.json") {
            if let Ok(text) = String::from_utf8(bytes) {
                let events = parse_sounds_json(&text);
                if !events.is_empty() {
                    return events;
                }
            }
        }
    }
    log::warn!("audio: sounds.json not found; sound events unavailable");
    HashMap::new()
}

/// Load and decode an ogg by its `sounds.json` asset name.
fn load_ogg(roots: &[PathBuf], name: &str) -> Option<StaticSoundData> {
    let asset = format!("assets/minecraft/sounds/{name}.ogg");
    for root in roots {
        if let Some(bytes) = read_asset(root, &asset) {
            if let Ok(data) = StaticSoundData::from_cursor(Cursor::new(bytes)) {
                return Some(data);
            }
        }
    }
    None
}

/// Read an asset file from a directory root, also tolerating the `assets/`
/// prefix being stripped (the texture loader does the same). The 1.8.9 ogg
/// sounds ship as loose files (they are never inside the client jar), so a
/// directory root is all that is needed.
fn read_asset(root: &Path, asset: &str) -> Option<Vec<u8>> {
    if let Ok(bytes) = std::fs::read(root.join(asset)) {
        return Some(bytes);
    }
    let stripped = asset.strip_prefix("assets/").unwrap_or(asset);
    std::fs::read(root.join(stripped)).ok()
}

/// Asset roots to search for `sounds.json` and the ogg files, mirroring the
/// renderer's texture-asset discovery so a custom `--assets` path (via the
/// `RECRAFT_*` env vars) and the bundled `local_assets` both work.
fn candidate_asset_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for var in ["RECRAFT_ASSET_PATH", "RECRAFT_ASSET_ZIP", "RECRAFT_MINECRAFT_JAR"] {
        if let Some(path) = std::env::var_os(var) {
            candidates.push(PathBuf::from(path));
        }
    }
    candidates.push(PathBuf::from("local_assets/minecraft-1.8.9"));
    candidates.push(PathBuf::from("local_assets/default"));
    candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "dig.stone": {
            "category": "block",
            "sounds": ["dig/stone1", "dig/stone2", "dig/stone3", "dig/stone4"]
        },
        "mob.rabbit.idle": {
            "category": "neutral",
            "sounds": [{"name": "mob/rabbit/idle1", "volume": 0.25}]
        },
        "random.click": {
            "category": "block",
            "sounds": ["random/click"]
        }
    }"#;

    #[test]
    fn parses_string_and_object_sound_forms() {
        let events = parse_sounds_json(SAMPLE);
        assert_eq!(events.len(), 3);

        let dig = &events["dig.stone"];
        assert_eq!(dig.len(), 4);
        assert_eq!(dig[0].name, "dig/stone1");
        assert_eq!(dig[0].volume, 1.0);

        let rabbit = &events["mob.rabbit.idle"];
        assert_eq!(rabbit.len(), 1);
        assert_eq!(rabbit[0].name, "mob/rabbit/idle1");
        assert!((rabbit[0].volume - 0.25).abs() < 1e-6);
    }

    #[test]
    fn ignores_malformed_or_empty_events() {
        let events = parse_sounds_json(r#"{ "a": {}, "b": { "sounds": [] }, "c": 5 }"#);
        assert!(events.is_empty());
    }

    #[test]
    fn resolves_event_to_ogg_asset_path() {
        // The event name maps to a weighted list; each variant's `name` resolves
        // to `assets/minecraft/sounds/<name>.ogg`.
        let events = parse_sounds_json(SAMPLE);
        let entry = &events["dig.stone"][2];
        let asset = format!("assets/minecraft/sounds/{}.ogg", entry.name);
        assert_eq!(asset, "assets/minecraft/sounds/dig/stone3.ogg");
    }

    #[test]
    fn attenuation_is_linear_and_clamps_at_range() {
        let listener = Listener {
            position: Vec3::ZERO,
            yaw: 0.0,
        };
        // At the listener: full volume.
        let (gain, _) = spatial_gain_pan(listener, Vec3::ZERO, 1.0).unwrap();
        assert!((gain - 1.0).abs() < 1e-6);

        // Halfway to the 16-block range: half volume.
        let (gain, _) = spatial_gain_pan(listener, Vec3::new(0.0, 0.0, 8.0), 1.0).unwrap();
        assert!((gain - 0.5).abs() < 1e-4);

        // At/just inside the edge: ~zero; past it: dropped.
        assert!(spatial_gain_pan(listener, Vec3::new(0.0, 0.0, 16.0), 1.0).is_none());
        assert!(spatial_gain_pan(listener, Vec3::new(0.0, 0.0, 20.0), 1.0).is_none());
    }

    #[test]
    fn loud_sounds_extend_their_range() {
        let listener = Listener {
            position: Vec3::ZERO,
            yaw: 0.0,
        };
        // volume 2.0 reaches 32 blocks; 20 is still audible.
        assert!(spatial_gain_pan(listener, Vec3::new(0.0, 0.0, 20.0), 2.0).is_some());
    }

    #[test]
    fn panning_follows_listener_right_vector() {
        // Facing +Z (yaw 0): the right vector is +X, so an emitter to the +X
        // side pans right (+1) and -X pans left (-1).
        let listener = Listener {
            position: Vec3::ZERO,
            yaw: 0.0,
        };
        let (_, right) = spatial_gain_pan(listener, Vec3::new(5.0, 0.0, 0.0), 1.0).unwrap();
        assert!(right > 0.9, "expected right pan, got {right}");
        let (_, left) = spatial_gain_pan(listener, Vec3::new(-5.0, 0.0, 0.0), 1.0).unwrap();
        assert!(left < -0.9, "expected left pan, got {left}");
        // Directly ahead: centered.
        let (_, center) = spatial_gain_pan(listener, Vec3::new(0.0, 0.0, 5.0), 1.0).unwrap();
        assert!(center.abs() < 0.1, "expected centered, got {center}");
    }

    #[test]
    fn amplitude_to_decibels_maps_silence_and_unity() {
        assert_eq!(amplitude_to_decibels(0.0).0, Decibels::SILENCE.0);
        assert!((amplitude_to_decibels(1.0).0 - 0.0).abs() < 1e-4);
        // Half amplitude ~ -6 dB.
        assert!((amplitude_to_decibels(0.5).0 - (-6.0206)).abs() < 1e-3);
    }
}
