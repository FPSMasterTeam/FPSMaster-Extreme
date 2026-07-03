//! Client-side tab-list roster (vanilla `NetHandlerPlayClient.playerInfoMap`),
//! fed by the S38 PlayerListItem and S47 PlayerListHeaderFooter packets. Each
//! entry is a player's GameProfile keyed by account UUID: the display name, the
//! latency/gamemode shown in the tab overlay, and the signed `textures`
//! property that carries the skin URL (consumed by the skin loader).

use std::collections::HashMap;

use fpsmaster_protocol::v1_8_9::packets::{PlayerListItemAction, PlayerListItemEntry, PlayerProperty};

/// Vanilla tab overlay shows at most 80 players.
const TAB_MAX_PLAYERS: usize = 80;

/// One roster entry (vanilla `NetworkPlayerInfo`).
#[derive(Debug, Clone, Default)]
pub struct PlayerInfo {
    pub name: String,
    /// 0 survival, 1 creative, 2 adventure, 3 spectator.
    pub gamemode: i32,
    /// Round-trip latency in ms (negative = unknown, drawn as no-signal).
    pub ping: i32,
    /// Chat-JSON display name overriding the plain name, when the server sets it.
    pub display_name: Option<String>,
    /// Signed GameProfile properties; the `textures` one holds the skin.
    pub properties: Vec<PlayerProperty>,
}

impl PlayerInfo {
    /// The base64 value of the `textures` property, if present. Consumed by the
    /// skin loader to download this player's skin.
    pub fn texture_property(&self) -> Option<&str> {
        self.properties
            .iter()
            .find(|p| p.name == "textures")
            .map(|p| p.value.as_str())
    }
}

#[derive(Debug, Default)]
pub struct PlayerList {
    players: HashMap<[u8; 16], PlayerInfo>,
    /// Flattened header/footer text (may contain newlines), drawn above/below
    /// the tab overlay.
    pub header: String,
    pub footer: String,
}

impl PlayerList {
    /// Apply an S38 PlayerListItem packet (add/update/remove entries).
    pub fn apply(&mut self, entries: Vec<PlayerListItemEntry>) {
        for entry in entries {
            match entry.action {
                PlayerListItemAction::Add {
                    name,
                    properties,
                    gamemode,
                    ping,
                    display_name,
                } => {
                    self.players.insert(
                        entry.uuid,
                        PlayerInfo {
                            name,
                            gamemode,
                            ping,
                            display_name,
                            properties,
                        },
                    );
                }
                PlayerListItemAction::UpdateGameMode { gamemode } => {
                    if let Some(info) = self.players.get_mut(&entry.uuid) {
                        info.gamemode = gamemode;
                    }
                }
                PlayerListItemAction::UpdateLatency { ping } => {
                    if let Some(info) = self.players.get_mut(&entry.uuid) {
                        info.ping = ping;
                    }
                }
                PlayerListItemAction::UpdateDisplayName { display_name } => {
                    if let Some(info) = self.players.get_mut(&entry.uuid) {
                        info.display_name = display_name;
                    }
                }
                PlayerListItemAction::Remove => {
                    self.players.remove(&entry.uuid);
                }
            }
        }
    }

    pub fn set_header_footer(&mut self, header: String, footer: String) {
        self.header = header;
        self.footer = footer;
    }

    /// Roster entry for a UUID; used to resolve player nametags (P3) and skins.
    pub fn get(&self, uuid: &[u8; 16]) -> Option<&PlayerInfo> {
        self.players.get(uuid)
    }

    /// Iterate the roster (uuid, info) — used to kick off skin downloads.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8; 16], &PlayerInfo)> {
        self.players.iter()
    }

    /// Players in vanilla tab order: spectators (gamemode 3) last, then by name
    /// case-insensitively; capped at 80 like the vanilla overlay.
    pub fn sorted(&self) -> Vec<&PlayerInfo> {
        let mut players: Vec<&PlayerInfo> = self.players.values().collect();
        players.sort_by(|a, b| {
            let spectator = (a.gamemode == 3).cmp(&(b.gamemode == 3));
            spectator.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        players.truncate(TAB_MAX_PLAYERS);
        players
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(uuid: u8, name: &str, gamemode: i32, ping: i32) -> PlayerListItemEntry {
        PlayerListItemEntry {
            uuid: [uuid; 16],
            action: PlayerListItemAction::Add {
                name: name.to_owned(),
                properties: Vec::new(),
                gamemode,
                ping,
                display_name: None,
            },
        }
    }

    #[test]
    fn add_update_remove_round_trip() {
        let mut list = PlayerList::default();
        list.apply(vec![add(1, "alice", 0, 50)]);
        assert_eq!(list.get(&[1; 16]).unwrap().ping, 50);

        list.apply(vec![PlayerListItemEntry {
            uuid: [1; 16],
            action: PlayerListItemAction::UpdateLatency { ping: 120 },
        }]);
        assert_eq!(list.get(&[1; 16]).unwrap().ping, 120);

        list.apply(vec![PlayerListItemEntry {
            uuid: [1; 16],
            action: PlayerListItemAction::Remove,
        }]);
        assert!(list.sorted().is_empty());
    }

    #[test]
    fn sorted_puts_spectators_last_then_alphabetical() {
        let mut list = PlayerList::default();
        list.apply(vec![
            add(1, "Zed", 0, 10),
            add(2, "spec", 3, 10), // spectator
            add(3, "amy", 0, 10),
        ]);
        let order: Vec<&str> = list.sorted().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(order, vec!["amy", "Zed", "spec"]);
    }

    #[test]
    fn texture_property_extracted() {
        let mut list = PlayerList::default();
        list.apply(vec![PlayerListItemEntry {
            uuid: [4; 16],
            action: PlayerListItemAction::Add {
                name: "skinned".to_owned(),
                properties: vec![PlayerProperty {
                    name: "textures".to_owned(),
                    value: "BASE64".to_owned(),
                    signature: None,
                }],
                gamemode: 0,
                ping: 0,
                display_name: None,
            },
        }]);
        assert_eq!(list.get(&[4; 16]).unwrap().texture_property(), Some("BASE64"));
    }
}
