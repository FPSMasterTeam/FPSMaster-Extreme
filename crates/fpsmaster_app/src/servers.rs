//! The multiplayer server list: on-disk persistence (vanilla servers.dat
//! equivalent, stored as JSON), address parsing with SRV resolution, and the
//! background server-list ping that fetches MOTD/players/latency.

use std::cmp::Reverse;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use crate::chat;

/// One saved server (vanilla ServerData).
#[derive(Debug, Clone)]
pub struct ServerEntry {
    pub name: String,
    pub address: String,
}

/// The saved server list, persisted as `<config dir>/fpsmaster/servers.json`.
#[derive(Debug, Clone, Default)]
pub struct ServerList {
    pub entries: Vec<ServerEntry>,
}

impl ServerList {
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Self::default();
        };
        let entries = value
            .as_array()
            .map(|array| {
                array
                    .iter()
                    .filter_map(|entry| {
                        Some(ServerEntry {
                            name: entry["name"].as_str()?.to_owned(),
                            address: entry["address"].as_str()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { entries }
    }

    pub fn save(&self) {
        let array: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name,
                    "address": entry.address,
                })
            })
            .collect();
        let path = store_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&serde_json::Value::Array(array)) {
            Ok(text) => {
                if let Err(err) = std::fs::write(&path, text) {
                    log::warn!("failed to save servers to {}: {err}", path.display());
                }
            }
            Err(err) => log::warn!("failed to serialize server list: {err}"),
        }
    }
}

/// The fpsmaster config directory (`<config dir>/fpsmaster/`).
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("fpsmaster")
}

fn store_path() -> PathBuf {
    config_dir().join("servers.json")
}

/// Parse "host" or "host:port", resolving the Minecraft SRV record for bare
/// hostnames (so `example.com` joins via `_minecraft._tcp`).
pub fn parse_server_address(value: &str) -> Option<(String, u16)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        return Some((host.to_owned(), port.parse().unwrap_or(25565)));
    }
    resolve_minecraft_srv(value).or_else(|| Some((value.to_owned(), 25565)))
}

fn resolve_minecraft_srv(host: &str) -> Option<(String, u16)> {
    if host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let resolver = hickory_resolver::Resolver::from_system_conf().ok()?;
    let lookup = resolver
        .srv_lookup(format!("_minecraft._tcp.{host}.").as_str())
        .ok()?;
    let record = lookup
        .iter()
        .min_by_key(|record| (record.priority(), Reverse(record.weight())))?;
    let target = record.target().to_utf8();
    let target = target.trim_end_matches('.');
    if target.is_empty() {
        return None;
    }
    log::info!(
        "resolved Minecraft SRV {host} -> {target}:{}",
        record.port()
    );
    Some((target.to_owned(), record.port()))
}

/// A completed server-list ping, ready for display.
#[derive(Debug, Clone)]
pub struct PingInfo {
    /// MOTD flattened to `§`-coded text.
    pub motd: String,
    /// "online/max".
    pub players: String,
    /// The player-name sample from the ping (vanilla shows it on hover), if the
    /// server provided one.
    pub sample: Vec<String>,
    /// Server version name from the ping; parsed and kept but not surfaced yet
    /// (vanilla only shows it on a protocol mismatch, which we don't flag).
    #[allow(dead_code)]
    pub version: String,
    pub latency_ms: u32,
    /// Decoded RGBA favicon (vanilla `data:image/png;base64,...`), if any,
    /// shared via `Arc` so the UI can blit it cheaply each frame.
    pub favicon: Option<std::sync::Arc<image::RgbaImage>>,
}

/// The outcome of pinging one server-list row.
#[derive(Debug, Clone)]
pub enum PingOutcome {
    Ok(PingInfo),
    Failed(String),
}

/// Ping every entry on background threads; results arrive as
/// `(row index, outcome)` on the returned channel.
pub fn ping_all(entries: &[ServerEntry]) -> Receiver<(usize, PingOutcome)> {
    let (tx, rx) = mpsc::channel();
    for (index, entry) in entries.iter().enumerate() {
        spawn_ping(index, entry.address.clone(), tx.clone());
    }
    rx
}

fn spawn_ping(index: usize, address: String, tx: Sender<(usize, PingOutcome)>) {
    std::thread::spawn(move || {
        let outcome = match parse_server_address(&address) {
            Some((host, port)) => {
                match fpsmaster_protocol::net::ping_status_1_8_9(
                    &host,
                    port,
                    Duration::from_secs(5),
                ) {
                    Ok(status) => PingOutcome::Ok(parse_status(&status.json, status.latency_ms)),
                    Err(err) => PingOutcome::Failed(err.to_string()),
                }
            }
            None => PingOutcome::Failed("invalid address".to_owned()),
        };
        let _ = tx.send((index, outcome));
    });
}

/// Pull MOTD / players / version out of the status JSON.
fn parse_status(json: &str, latency_ms: u32) -> PingInfo {
    let value: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let motd = match &value["description"] {
        serde_json::Value::Null => String::new(),
        description => chat::flatten_chat_json(&description.to_string()),
    };
    // MOTDs are at most two lines; collapse newlines for the single-row list.
    let motd = motd.replace('\n', " §r");
    let players = format!(
        "{}/{}",
        value["players"]["online"].as_i64().unwrap_or(0),
        value["players"]["max"].as_i64().unwrap_or(0),
    );
    let version = value["version"]["name"].as_str().unwrap_or("?").to_owned();
    let sample = value["players"]["sample"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let favicon = value["favicon"].as_str().and_then(decode_favicon);
    PingInfo {
        motd,
        players,
        version,
        sample,
        latency_ms,
        favicon,
    }
}

/// Decode a `data:image/png;base64,<...>` favicon into a shared RGBA image.
fn decode_favicon(data_uri: &str) -> Option<std::sync::Arc<image::RgbaImage>> {
    use base64::Engine;
    let b64 = data_uri.strip_prefix("data:image/png;base64,")?;
    // Some servers include whitespace/newlines in the base64 payload.
    let cleaned: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(cleaned).ok()?;
    fpsmaster_render::texture::decode_png(&bytes).map(std::sync::Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_json() {
        let json = r#"{
            "version": {"name": "1.8.9", "protocol": 47},
            "players": {"max": 20, "online": 3},
            "description": {"text": "A ", "extra": [{"text": "server", "color": "red"}]}
        }"#;
        let info = parse_status(json, 42);
        assert_eq!(info.motd, "A §cserver");
        assert_eq!(info.players, "3/20");
        assert_eq!(info.version, "1.8.9");
        assert_eq!(info.latency_ms, 42);
    }

    #[test]
    fn parses_plain_string_motd() {
        let json = r#"{"description": "hello", "players": {"max": 1, "online": 0}}"#;
        assert_eq!(parse_status(json, 0).motd, "hello");
    }

    #[test]
    fn address_parsing_handles_explicit_port() {
        assert_eq!(
            parse_server_address("127.0.0.1:25566"),
            Some(("127.0.0.1".to_owned(), 25566))
        );
        assert_eq!(parse_server_address(""), None);
    }
}
