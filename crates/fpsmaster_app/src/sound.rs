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

/// Vanilla `SoundCategory` (MCP-919 `SoundCategory.java`). Each `sounds.json`
/// event declares its category via the `"category"` field; the manager scales a
/// sound's gain by the matching per-category volume (and the global master).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundCategory {
    Master,
    Music,
    Records,
    Weather,
    Blocks,
    Mobs,
    Animals,
    Players,
    Ambient,
}

impl SoundCategory {
    /// The lowercase vanilla enum string used in `sounds.json`'s `"category"`
    /// field (e.g. `Blocks` -> `"block"`, `Mobs` -> `"hostile"`).
    // Not yet used outside parsing round-trip tests; kept for the GUI/i18n layer.
    #[allow(dead_code)]
    pub fn category_name(self) -> &'static str {
        match self {
            SoundCategory::Master => "master",
            SoundCategory::Music => "music",
            SoundCategory::Records => "record",
            SoundCategory::Weather => "weather",
            SoundCategory::Blocks => "block",
            SoundCategory::Mobs => "hostile",
            SoundCategory::Animals => "neutral",
            SoundCategory::Players => "player",
            SoundCategory::Ambient => "ambient",
        }
    }

    /// Parse a `sounds.json` `"category"` string into a [`SoundCategory`].
    /// Returns `None` for an unknown category so callers can pick a default.
    pub fn from_str_name(s: &str) -> Option<SoundCategory> {
        Some(match s {
            "master" => SoundCategory::Master,
            "music" => SoundCategory::Music,
            "record" => SoundCategory::Records,
            "weather" => SoundCategory::Weather,
            "block" => SoundCategory::Blocks,
            "hostile" => SoundCategory::Mobs,
            "neutral" => SoundCategory::Animals,
            "player" => SoundCategory::Players,
            "ambient" => SoundCategory::Ambient,
            _ => return None,
        })
    }
}

/// A snapshot of the user's per-category volumes (each 0..=1) plus the global
/// master. `main.rs` pushes this into the [`SoundManager`] via
/// [`SoundManager::set_volumes`] whenever settings change; the manager reads it
/// when computing playback gain. Vanilla's final gain is
/// `queued * entry * category * master` (MCP-919 `getNormalizedVolume`).
#[derive(Debug, Clone, Copy)]
pub struct Volumes {
    pub master: f32,
    pub music: f32,
    pub record: f32,
    pub weather: f32,
    pub block: f32,
    pub hostile: f32,
    pub neutral: f32,
    pub player: f32,
    pub ambient: f32,
}

impl Default for Volumes {
    fn default() -> Self {
        Self {
            master: 1.0,
            music: 1.0,
            record: 1.0,
            weather: 1.0,
            block: 1.0,
            hostile: 1.0,
            neutral: 1.0,
            player: 1.0,
            ambient: 1.0,
        }
    }
}

impl Volumes {
    /// The stored 0..=1 volume for `category`. MASTER always reports `1.0`
    /// (vanilla `getSoundCategoryVolume(MASTER)`); the master slider is applied
    /// separately as a global multiplier.
    pub fn category(&self, category: SoundCategory) -> f32 {
        match category {
            SoundCategory::Master => 1.0,
            SoundCategory::Music => self.music,
            SoundCategory::Records => self.record,
            SoundCategory::Weather => self.weather,
            SoundCategory::Blocks => self.block,
            SoundCategory::Mobs => self.hostile,
            SoundCategory::Animals => self.neutral,
            SoundCategory::Players => self.player,
            SoundCategory::Ambient => self.ambient,
        }
    }
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

/// One `sounds.json` event: its category (shared by all variants, vanilla puts
/// `"category"` on the event) plus its weighted variant list.
#[derive(Debug, Clone)]
struct SoundEvent {
    category: SoundCategory,
    entries: Vec<SoundEntry>,
}

/// The listener pose (camera). World and render coordinates are 1:1 in this
/// client, so the camera position doubles as the world-space listener position.
#[derive(Debug, Clone, Copy)]
pub struct Listener {
    pub position: Vec3,
    /// Camera yaw in degrees, Minecraft convention (matches `Camera::yaw`).
    pub yaw: f32,
}

/// A live positioned sound that follows an entity (minecart, mob, …). The game
/// spawns it with an id, updates its position each frame, and stops it when the
/// emitter dies. The kira handle is kept alive so gain/pan/rate can be mutated
/// on the running source (dropping it would stop playback).
// `handle` and `category` are used every update; `base_volume`/`base_pitch`
// are recorded on each update as the last-applied pre-spatial values but not
// read back yet — kept for debugging and a future re-gain-without-move path.
struct MovingSound {
    handle: kira::sound::static_sound::StaticSoundHandle,
    /// The event's category, for applying the per-category volume on updates.
    category: SoundCategory,
    /// The base (queued * entry) linear volume, before spatial attenuation.
    #[allow(dead_code)]
    base_volume: f32,
    /// The base playback rate (queued.pitch * entry.pitch), before clamping.
    #[allow(dead_code)]
    base_pitch: f32,
}

pub struct SoundManager {
    /// `None` when no audio device could be opened — every play becomes a no-op.
    manager: Option<AudioManager<DefaultBackend>>,
    /// Event key -> its category and weighted sound variants.
    events: HashMap<String, SoundEvent>,
    /// Decoded ogg cache, keyed by sound asset name (e.g. `"dig/stone1"`).
    cache: HashMap<String, Option<StaticSoundData>>,
    /// Resolved asset roots to read ogg files from (directories or jars).
    asset_roots: Vec<PathBuf>,
    listener: Listener,
    /// Deterministic per-event variant picker (xorshift, no `rand` dependency).
    rng: u32,
    /// Current per-category + master volumes, pushed from `main.rs` on change.
    volumes: Volumes,
    /// The single background music track, if one is playing (MUSIC category).
    /// Driven by the music methods, which `main.rs` calls from the `MusicTicker`
    /// commands each frame.
    music: Option<kira::sound::static_sound::StaticSoundHandle>,
    /// Entity-attached positioned sounds, keyed by a game-assigned id.
    moving: HashMap<u64, MovingSound>,
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
            volumes: Volumes::default(),
            music: None,
            moving: HashMap::new(),
        }
    }

    pub fn set_listener(&mut self, listener: Listener) {
        self.listener = listener;
    }

    /// Update the per-category + master volume snapshot. `main.rs` pushes this
    /// from `Settings` each in-world frame (via `volumes_from_settings`), so the
    /// category sliders take effect immediately. Affects newly played sounds and
    /// the next update of moving/music sounds.
    pub fn set_volumes(&mut self, volumes: Volumes) {
        self.volumes = volumes;
    }

    /// Resolve and play a queued sound. Positional sounds are attenuated and
    /// panned relative to the listener; out-of-range sounds are dropped.
    pub fn play(&mut self, sound: &QueuedSound) {
        if self.manager.is_none() {
            return;
        }
        let Some((entry, category)) = self.pick_entry(&sound.event) else {
            return;
        };

        // Combine the packet volume with the event's per-sound volume, then scale
        // by the per-category volume and the global master (vanilla
        // getNormalizedVolume: queued * entry * category * master).
        let category_gain = self.volumes.category(category) * self.volumes.master;
        let base_volume = sound.volume * entry.volume * category_gain;
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
        // Vanilla clamps the combined pitch to [0.5, 2.0] (MCP-919 line 421).
        let rate = clamp_pitch(sound.pitch * entry.pitch) as f64;
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
    /// Returns the chosen variant and the event's category.
    fn pick_entry(&mut self, event: &str) -> Option<(SoundEntry, SoundCategory)> {
        let def = self.events.get(event)?;
        let category = def.category;
        let entries = &def.entries;
        if entries.is_empty() {
            return None;
        }
        let total: u32 = entries.iter().map(|e| e.weight.max(1)).sum();
        let mut roll = next_xorshift(&mut self.rng) % total;
        for entry in entries {
            let w = entry.weight.max(1);
            if roll < w {
                return Some((entry.clone(), category));
            }
            roll -= w;
        }
        entries.last().cloned().map(|e| (e, category))
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

    // ─── Music channel ──────────────────────────────────────────────────────

    /// Start a background music track by its `sounds.json` asset name (e.g.
    /// `"music/menu/menu"`), replacing any track already playing. Played
    /// non-positionally (centered) at the MUSIC category × master volume. The
    /// handle is kept so [`is_music_playing`](Self::is_music_playing) can poll it
    /// and [`stop_music`](Self::stop_music) can end it. No-op with no audio
    /// device or a missing asset.
    #[allow(dead_code)]
    pub fn play_music(&mut self, asset_name: &str) {
        // Drop any prior track (dropping the handle stops playback immediately).
        self.music = None;
        if self.manager.is_none() {
            return;
        }
        let Some(data) = self.sound_data(asset_name) else {
            return;
        };
        let gain = (self.volumes.music * self.volumes.master).clamp(0.0, 1.0);
        let data = data.volume(amplitude_to_decibels(gain));
        if let Some(manager) = self.manager.as_mut() {
            match manager.play(data) {
                Ok(handle) => self.music = Some(handle),
                Err(err) => log::debug!("audio: music play failed: {err}"),
            }
        }
    }

    /// Start background music from a `sounds.json` *event* key (e.g.
    /// `"music.menu"`), resolving one weighted variant like [`play`](Self::play)
    /// and playing it as the MUSIC track. This is the game-layer entry point:
    /// `game.rs` only knows event keys, not the underlying ogg asset paths.
    pub fn play_music_event(&mut self, event: &str) {
        let Some((entry, _category)) = self.pick_entry(event) else {
            log::debug!("audio: music event {event} not found");
            return;
        };
        self.play_music(&entry.name);
    }

    /// Stop the current music track, if any.
    pub fn stop_music(&mut self) {
        if let Some(mut handle) = self.music.take() {
            handle.stop(kira::Tween::default());
        }
    }

    /// Whether a music track is currently playing (not stopped/finished). Used by
    /// the `MusicTicker` to know when to queue the next track.
    pub fn is_music_playing(&self) -> bool {
        use kira::sound::PlaybackState;
        self.music
            .as_ref()
            .map(|h| !matches!(h.state(), PlaybackState::Stopped))
            .unwrap_or(false)
    }

    // ─── Movable positioned emitters (entity-attached sounds) ────────────────

    /// Spawn a positioned sound attached to a game-assigned `id`, resolving
    /// `event` like [`play`](Self::play). The initial spatial gain/pan is baked
    /// in from `pos`; call [`update_moving_sound`](Self::update_moving_sound)
    /// each frame with the emitter's new position, and
    /// [`stop_moving_sound`](Self::stop_moving_sound) when it dies. Returns
    /// `true` if the sound started (in range and audio available). If `looping`,
    /// the whole clip loops (minecart-style); otherwise it is a one-shot.
    pub fn attach_moving_sound(
        &mut self,
        id: u64,
        event: &str,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        looping: bool,
    ) -> bool {
        if self.manager.is_none() {
            return false;
        }
        let Some((entry, category)) = self.pick_entry(event) else {
            return false;
        };
        let base_volume = volume * entry.volume;
        let base_pitch = pitch * entry.pitch;
        let category_gain = self.volumes.category(category) * self.volumes.master;
        let Some((gain, panning)) =
            spatial_gain_pan(self.listener, pos, base_volume * category_gain)
        else {
            return false; // out of range at spawn — nothing to attach
        };
        let Some(data) = self.sound_data(&entry.name) else {
            return false;
        };
        let mut data = data
            .volume(amplitude_to_decibels(gain))
            .panning(Panning(panning))
            .playback_rate(PlaybackRate(clamp_pitch(base_pitch) as f64));
        if looping {
            data = data.loop_region(0.0..);
        }
        let Some(manager) = self.manager.as_mut() else {
            return false;
        };
        match manager.play(data) {
            Ok(handle) => {
                self.moving.insert(
                    id,
                    MovingSound {
                        handle,
                        category,
                        base_volume,
                        base_pitch,
                    },
                );
                true
            }
            Err(err) => {
                log::debug!("audio: moving sound play failed: {err}");
                false
            }
        }
    }

    /// Recompute a moving sound's gain/pan from its new world position (and an
    /// optionally changed base volume/pitch), applying them to the live source.
    /// If it has moved out of range, it is stopped and removed. No-op for an
    /// unknown `id`.
    pub fn update_moving_sound(&mut self, id: u64, pos: Vec3, volume: f32, pitch: f32) {
        let category_gain = {
            let Some(sound) = self.moving.get(&id) else {
                return;
            };
            self.volumes.category(sound.category) * self.volumes.master
        };
        let base_volume = volume;
        let spatial = spatial_gain_pan(self.listener, pos, base_volume * category_gain);
        let Some(sound) = self.moving.get_mut(&id) else {
            return;
        };
        match spatial {
            Some((gain, panning)) => {
                sound.base_volume = base_volume;
                sound.base_pitch = pitch;
                let tween = kira::Tween::default();
                sound
                    .handle
                    .set_volume(amplitude_to_decibels(gain), tween);
                sound.handle.set_panning(Panning(panning), tween);
                sound
                    .handle
                    .set_playback_rate(PlaybackRate(clamp_pitch(pitch) as f64), tween);
            }
            None => {
                // Out of range: stop and forget it.
                if let Some(mut sound) = self.moving.remove(&id) {
                    sound.handle.stop(kira::Tween::default());
                }
            }
        }
    }

    /// Stop and remove a moving sound (e.g. the emitter died). No-op if unknown.
    pub fn stop_moving_sound(&mut self, id: u64) {
        if let Some(mut sound) = self.moving.remove(&id) {
            sound.handle.stop(kira::Tween::default());
        }
    }

    /// Whether a moving sound with `id` is currently tracked.
    #[allow(dead_code)]
    pub fn has_moving_sound(&self, id: u64) -> bool {
        self.moving.contains_key(&id)
    }
}

/// Parse `sounds.json` into the event -> variants map. Pure over the JSON text
/// so it is unit-testable without a filesystem.
fn parse_sounds_json(text: &str) -> HashMap<String, SoundEvent> {
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
        // Category is per-event; default to Master when absent/unknown (vanilla
        // treats an unspecified category as the master group).
        let category = value
            .get("category")
            .and_then(|c| c.as_str())
            .and_then(SoundCategory::from_str_name)
            .unwrap_or(SoundCategory::Master);
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
            events.insert(event.clone(), SoundEvent { category, entries });
        }
    }
    events
}

/// Clamp a combined playback rate (pitch) to vanilla's [0.5, 2.0] range
/// (MCP-919 `SoundManager.getNormalizedPitch`, `clamp_double(pitch, 0.5, 2.0)`).
fn clamp_pitch(pitch: f32) -> f32 {
    pitch.clamp(0.5, 2.0)
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
fn load_sound_events(roots: &[PathBuf]) -> HashMap<String, SoundEvent> {
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
/// `FPSMASTER_*` env vars) and the bundled `local_assets` both work.
fn candidate_asset_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for var in ["FPSMASTER_ASSET_PATH", "FPSMASTER_ASSET_ZIP", "FPSMASTER_MINECRAFT_JAR"] {
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

        let dig = &events["dig.stone"].entries;
        assert_eq!(dig.len(), 4);
        assert_eq!(dig[0].name, "dig/stone1");
        assert_eq!(dig[0].volume, 1.0);

        let rabbit = &events["mob.rabbit.idle"].entries;
        assert_eq!(rabbit.len(), 1);
        assert_eq!(rabbit[0].name, "mob/rabbit/idle1");
        assert!((rabbit[0].volume - 0.25).abs() < 1e-6);
    }

    #[test]
    fn parses_event_category() {
        let events = parse_sounds_json(SAMPLE);
        assert_eq!(events["dig.stone"].category, SoundCategory::Blocks);
        assert_eq!(events["mob.rabbit.idle"].category, SoundCategory::Animals);
        // An event with no "category" field defaults to Master.
        let e = parse_sounds_json(r#"{ "x": { "sounds": ["a"] } }"#);
        assert_eq!(e["x"].category, SoundCategory::Master);
    }

    #[test]
    fn category_name_round_trips() {
        for c in [
            SoundCategory::Master,
            SoundCategory::Music,
            SoundCategory::Records,
            SoundCategory::Weather,
            SoundCategory::Blocks,
            SoundCategory::Mobs,
            SoundCategory::Animals,
            SoundCategory::Players,
            SoundCategory::Ambient,
        ] {
            assert_eq!(SoundCategory::from_str_name(c.category_name()), Some(c));
        }
        assert_eq!(SoundCategory::from_str_name("bogus"), None);
    }

    #[test]
    fn master_category_volume_is_always_unity() {
        let mut v = Volumes::default();
        v.music = 0.3;
        // MASTER slot ignores stored value; it is applied via the master field.
        assert_eq!(v.category(SoundCategory::Master), 1.0);
        assert!((v.category(SoundCategory::Music) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn pitch_clamps_to_vanilla_range() {
        assert!((clamp_pitch(0.1) - 0.5).abs() < 1e-6);
        assert!((clamp_pitch(3.0) - 2.0).abs() < 1e-6);
        assert!((clamp_pitch(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn category_gain_composes_category_and_master() {
        // Vanilla getNormalizedVolume: the per-category volume and the master
        // slider multiply. A half-master, quarter-block world attenuates a block
        // sound to 1/8 of its base before spatial attenuation.
        let mut v = Volumes::default();
        v.master = 0.5;
        v.block = 0.25;
        let gain = v.category(SoundCategory::Blocks) * v.master;
        assert!((gain - 0.125).abs() < 1e-6, "block gain = category*master");
        // The MASTER category itself always reports unity, so a MASTER-category
        // sound is scaled by the master slider alone.
        let master_gain = v.category(SoundCategory::Master) * v.master;
        assert!((master_gain - 0.5).abs() < 1e-6);
        // A zeroed category fully silences its sounds regardless of master.
        v.ambient = 0.0;
        assert_eq!(v.category(SoundCategory::Ambient) * v.master, 0.0);
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
        let entry = &events["dig.stone"].entries[2];
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
