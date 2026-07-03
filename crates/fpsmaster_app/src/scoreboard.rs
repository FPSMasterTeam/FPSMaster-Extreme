//! Client-side scoreboard state (objectives, scores, teams, display slots),
//! fed by the S3B/S3C/S3D/S3E packets, and the sidebar view the HUD renders.
//! On 1.8 servers most sidebar "entries" are fake player names whose visible
//! text lives in the prefix/suffix of the team they belong to.

use std::collections::{HashMap, HashSet};

use fpsmaster_protocol::v1_8_9::packets::TeamAction;

/// The vanilla sidebar shows at most this many entries.
const SIDEBAR_MAX_ROWS: usize = 15;

#[derive(Debug, Default)]
struct Objective {
    display_name: String,
    /// Entry name → score value.
    scores: HashMap<String, i32>,
}

#[derive(Debug, Default)]
struct Team {
    prefix: String,
    suffix: String,
    members: HashSet<String>,
}

#[derive(Debug, Default)]
pub struct Scoreboard {
    objectives: HashMap<String, Objective>,
    /// Objective bound to display slot 1 (the sidebar).
    sidebar: Option<String>,
    teams: HashMap<String, Team>,
    /// Entry → team name (an entry is in at most one team).
    entry_team: HashMap<String, String>,
}

/// A ready-to-render sidebar: title plus `(decorated text, score)` rows,
/// highest score first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarView {
    pub title: String,
    pub rows: Vec<(String, i32)>,
}

impl Scoreboard {
    /// Apply S3B ScoreboardObjective (mode 0 create, 1 remove, 2 retitle).
    pub fn apply_objective(&mut self, name: String, mode: u8, display_name: String) {
        match mode {
            0 => {
                self.objectives.insert(
                    name,
                    Objective {
                        display_name,
                        scores: HashMap::new(),
                    },
                );
            }
            1 => {
                self.objectives.remove(&name);
                if self.sidebar.as_deref() == Some(name.as_str()) {
                    self.sidebar = None;
                }
            }
            2 => {
                if let Some(objective) = self.objectives.get_mut(&name) {
                    objective.display_name = display_name;
                }
            }
            _ => {}
        }
    }

    /// Apply S3C UpdateScore. `value` None removes the entry — from the named
    /// objective, or every objective when the name is empty (vanilla allows
    /// both forms of removal).
    pub fn apply_score(&mut self, entry: &str, objective: &str, value: Option<i32>) {
        match value {
            Some(value) => {
                if let Some(objective) = self.objectives.get_mut(objective) {
                    objective.scores.insert(entry.to_owned(), value);
                }
            }
            None if objective.is_empty() => {
                for objective in self.objectives.values_mut() {
                    objective.scores.remove(entry);
                }
            }
            None => {
                if let Some(objective) = self.objectives.get_mut(objective) {
                    objective.scores.remove(entry);
                }
            }
        }
    }

    /// Apply S3D DisplayScoreboard. Only the sidebar slot (1) is rendered.
    pub fn apply_display(&mut self, position: i8, objective: String) {
        if position == 1 {
            self.sidebar = (!objective.is_empty()).then_some(objective);
        }
    }

    /// Apply S3E Teams.
    pub fn apply_team(&mut self, name: String, action: TeamAction) {
        match action {
            TeamAction::Create {
                prefix,
                suffix,
                players,
            } => {
                let mut team = Team {
                    prefix,
                    suffix,
                    members: HashSet::new(),
                };
                for player in &players {
                    team.members.insert(player.clone());
                }
                self.teams.insert(name.clone(), team);
                for player in players {
                    self.move_entry_to_team(player, &name);
                }
            }
            TeamAction::Remove => {
                if let Some(team) = self.teams.remove(&name) {
                    for member in team.members {
                        self.entry_team.remove(&member);
                    }
                }
            }
            TeamAction::Update { prefix, suffix } => {
                if let Some(team) = self.teams.get_mut(&name) {
                    team.prefix = prefix;
                    team.suffix = suffix;
                }
            }
            TeamAction::AddPlayers { players } => {
                if !self.teams.contains_key(&name) {
                    return;
                }
                for player in players {
                    self.teams
                        .get_mut(&name)
                        .expect("team checked above")
                        .members
                        .insert(player.clone());
                    self.move_entry_to_team(player, &name);
                }
            }
            TeamAction::RemovePlayers { players } => {
                if let Some(team) = self.teams.get_mut(&name) {
                    for player in players {
                        team.members.remove(&player);
                        if self.entry_team.get(&player).map(String::as_str) == Some(&name) {
                            self.entry_team.remove(&player);
                        }
                    }
                }
            }
        }
    }

    /// Move an entry into `team`, removing it from its previous team (an entry
    /// belongs to at most one team).
    fn move_entry_to_team(&mut self, entry: String, team: &str) {
        if let Some(previous) = self.entry_team.insert(entry.clone(), team.to_owned()) {
            if previous != team {
                if let Some(old) = self.teams.get_mut(&previous) {
                    old.members.remove(&entry);
                }
            }
        }
    }

    /// Entry text as displayed (team prefix + name + suffix), for the tab list
    /// and player nametags. Vanilla `ScorePlayerTeam.formatPlayerName`.
    pub fn decorate_entry(&self, entry: &str) -> String {
        self.decorate(entry)
    }

    /// Entry text as displayed: team prefix + name + suffix.
    fn decorate(&self, entry: &str) -> String {
        match self
            .entry_team
            .get(entry)
            .and_then(|team| self.teams.get(team))
        {
            Some(team) => format!("{}{}{}", team.prefix, entry, team.suffix),
            None => entry.to_owned(),
        }
    }

    /// The sidebar to render, if an objective is bound to the sidebar slot.
    /// Rows follow the vanilla order: sort ascending by (score, name), keep
    /// the top 15, display highest first.
    pub fn sidebar_view(&self) -> Option<SidebarView> {
        let objective = self.objectives.get(self.sidebar.as_deref()?)?;
        let mut entries: Vec<(&String, &i32)> = objective.scores.iter().collect();
        entries.sort_by(|a, b| {
            a.1.cmp(b.1)
                .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });
        if entries.len() > SIDEBAR_MAX_ROWS {
            entries.drain(..entries.len() - SIDEBAR_MAX_ROWS);
        }
        entries.reverse();
        Some(SidebarView {
            title: objective.display_name.clone(),
            rows: entries
                .into_iter()
                .map(|(entry, score)| (self.decorate(entry), *score))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidebar_with_scores(scores: &[(&str, i32)]) -> Scoreboard {
        let mut board = Scoreboard::default();
        board.apply_objective("obj".to_owned(), 0, "Title".to_owned());
        board.apply_display(1, "obj".to_owned());
        for (entry, value) in scores {
            board.apply_score(entry, "obj", Some(*value));
        }
        board
    }

    #[test]
    fn sidebar_sorts_descending_and_caps_at_fifteen() {
        let mut board = Scoreboard::default();
        board.apply_objective("obj".to_owned(), 0, "Title".to_owned());
        board.apply_display(1, "obj".to_owned());
        for i in 0..20 {
            board.apply_score(&format!("line{i}"), "obj", Some(i));
        }
        let view = board.sidebar_view().unwrap();
        assert_eq!(view.title, "Title");
        assert_eq!(view.rows.len(), 15);
        assert_eq!(view.rows[0], ("line19".to_owned(), 19));
        assert_eq!(view.rows[14], ("line5".to_owned(), 5));
    }

    #[test]
    fn no_sidebar_without_display_slot() {
        let mut board = Scoreboard::default();
        board.apply_objective("obj".to_owned(), 0, "Title".to_owned());
        assert!(board.sidebar_view().is_none());
        board.apply_display(1, "obj".to_owned());
        assert!(board.sidebar_view().is_some());
        board.apply_display(1, String::new());
        assert!(board.sidebar_view().is_none());
    }

    #[test]
    fn team_prefix_suffix_decorate_entries() {
        let mut board = sidebar_with_scores(&[("entry", 1)]);
        board.apply_team(
            "t1".to_owned(),
            TeamAction::Create {
                prefix: "§a".to_owned(),
                suffix: "§r!".to_owned(),
                players: vec!["entry".to_owned()],
            },
        );
        let view = board.sidebar_view().unwrap();
        assert_eq!(view.rows[0].0, "§aentry§r!");
        // Updating the team retitles the line (the usual animation mechanism).
        board.apply_team(
            "t1".to_owned(),
            TeamAction::Update {
                prefix: "§b".to_owned(),
                suffix: String::new(),
            },
        );
        assert_eq!(board.sidebar_view().unwrap().rows[0].0, "§bentry");
    }

    #[test]
    fn entry_moves_between_teams() {
        let mut board = sidebar_with_scores(&[("entry", 1)]);
        board.apply_team(
            "t1".to_owned(),
            TeamAction::Create {
                prefix: "A".to_owned(),
                suffix: String::new(),
                players: vec!["entry".to_owned()],
            },
        );
        board.apply_team(
            "t2".to_owned(),
            TeamAction::Create {
                prefix: "B".to_owned(),
                suffix: String::new(),
                players: Vec::new(),
            },
        );
        board.apply_team(
            "t2".to_owned(),
            TeamAction::AddPlayers {
                players: vec!["entry".to_owned()],
            },
        );
        assert_eq!(board.sidebar_view().unwrap().rows[0].0, "Bentry");
        board.apply_team("t2".to_owned(), TeamAction::Remove);
        assert_eq!(board.sidebar_view().unwrap().rows[0].0, "entry");
    }

    #[test]
    fn score_removal_clears_rows() {
        let mut board = sidebar_with_scores(&[("a", 1), ("b", 2)]);
        board.apply_score("b", "obj", None);
        assert_eq!(board.sidebar_view().unwrap().rows.len(), 1);
        board.apply_score("a", "", None); // empty objective = remove everywhere
        assert!(board.sidebar_view().unwrap().rows.is_empty());
    }

    #[test]
    fn objective_remove_clears_sidebar() {
        let mut board = sidebar_with_scores(&[("a", 1)]);
        board.apply_objective("obj".to_owned(), 1, String::new());
        assert!(board.sidebar_view().is_none());
    }
}
