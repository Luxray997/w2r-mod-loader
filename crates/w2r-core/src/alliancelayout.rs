//! The pure part of the replacement Alliances screen (`alliancesscreen.rs`), ported from
//! wc2r-plus `features/ui/alliances_screen.cpp`: which players go under which team heading, in
//! what order, in what colour, and the column widths with and without the host's Kick column.
//! Layout numbers are in the game's 2160-high UI space.

use crate::screenkit::fl;

pub const TITLE_Y: f32 = 70.0;
pub const TITLE_W: f32 = 1650.0;
pub const TITLE_H: f32 = 120.0;
pub const FOOTER_X: f32 = 150.0;
pub const FOOTER_W: f32 = 1350.0;
pub const FOOTER_H: f32 = 135.0;
pub const FOOTER_BOTTOM_GAP: f32 = 230.0;
pub const CONTENT_X: f32 = 150.0;
pub const CONTENT_TOP: f32 = 210.0;
pub const CONTENT_W: f32 = 1350.0;
pub const CONTENT_FOOTER_GAP: f32 = 98.0;
pub const HEADER_ROW_H: f32 = 180.0;
pub const VISION_LINE_H: f32 = 78.0;
pub const PLAYER_ROW_H: f32 = 120.0;
pub const TEAM_HEADER_H: f32 = 70.0;

/// Column fractions of the header row: spacer, victory checkbox, "Allies", "Shared vision".
static HEADER: [f32; 4] = [0.02, 0.48, 0.2, 0.3];
/// Column fractions of a player row: colour icon, name, allied box, vision box.
static PLAYER: [f32; 4] = [0.1, 0.47, 0.25, 0.25];
/// What the host's Kick column takes from the row; the other columns shrink by the rest.
const KICK_SHARE: f32 = 0.16;

const fn scaled(base: [f32; 4]) -> [f32; 5] {
    let scale = 1.0 - KICK_SHARE;
    [
        base[0] * scale,
        base[1] * scale,
        base[2] * scale,
        base[3] * scale,
        KICK_SHARE,
    ]
}

// Statics, not temporaries: the game's row layout keeps the pointer it is given until the row's
// cells are drawn, so the ratios must outlive the call.
static HEADER_KICK: [f32; 5] = scaled(HEADER);
static PLAYER_KICK: [f32; 5] = scaled(PLAYER);

pub fn header_ratios(kick: bool) -> &'static [f32] {
    if kick { &HEADER_KICK } else { &HEADER }
}

pub fn player_ratios(kick: bool) -> &'static [f32] {
    if kick { &PLAYER_KICK } else { &PLAYER }
}

/// The height of the scrolling content group.
pub fn content_height(window_h: f32) -> f32 {
    fl(window_h - FOOTER_BOTTOM_GAP - CONTENT_FOOTER_GAP) - CONTENT_TOP
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    Plain,
    Teammate,
    /// Defeated, victorious or disconnected: out of the match.
    Out,
    /// Watching on the observer team: not a defeat, so not red.
    Observer,
}

/// Out beats everything, then observer, then teammate; teammates only exist in a team game.
pub fn tint(out: bool, observer: bool, team_game: bool, is_me: bool, teammate: bool) -> Tint {
    if out {
        Tint::Out
    } else if observer {
        Tint::Observer
    } else if team_game && (is_me || teammate) {
        Tint::Teammate
    } else {
        Tint::Plain
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub player: u8,
    /// The lobby team in a team game, else 0.
    pub team: u8,
    pub name: String,
    pub tint: Tint,
    /// The stock dialog has a checkbox row for this player.
    pub checkboxes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub team: u8,
    pub rows: Vec<Entry>,
}

/// Players under their team, the local player first in their own group and the local team's
/// group first of all, the others by team number. `entries` come in player order.
pub fn build_groups(entries: Vec<Entry>, me: u8, my_team: u8) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for e in entries {
        let at = match groups.iter().position(|g| g.team == e.team) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    team: e.team,
                    rows: Vec::new(),
                });
                groups.len() - 1
            }
        };
        if e.player == me {
            groups[at].rows.insert(0, e);
        } else {
            groups[at].rows.push(e);
        }
    }
    groups.sort_by_key(|g| (g.team != my_team, g.team));
    groups
}

/// The localization key of a stock clan name ("Nation of Azeroth" and the like), which the game
/// stores as English text in the player records.
pub fn clan_key(raw: &str) -> Option<&'static str> {
    CLANS.iter().find(|(name, _)| *name == raw).map(|(_, k)| *k)
}

const CLANS: &[(&str, &str)] = &[
    ("Nation of Azeroth", "clan_human_blue"),
    ("Stormreaver Clan", "clan_orc_blue"),
    ("Nation of Lordaeron", "clan_human_white"),
    ("Dragonmaw Clan", "clan_orc_white"),
    ("Nation of Stromgarde", "clan_human_red"),
    ("Blackrock Clan", "clan_orc_red"),
    ("Nation of Kul Tiras", "clan_human_green"),
    ("Bleeding Hollow Clan", "clan_orc_green"),
    ("Nation of Gilneas", "clan_human_black"),
    ("Black Tooth Grin Clan", "clan_orc_black"),
    ("Nation of Dalaran", "clan_human_purple"),
    ("Twilight's Hammer Clan", "clan_orc_purple"),
    ("Nation of Alterac", "clan_human_orange"),
    ("Burning Blade Clan", "clan_orc_orange"),
    ("Alliance Traitors", "clan_human_yellow"),
    ("Horde Traitors", "clan_orc_yellow"),
    ("Flowerpicker Clan", "clan_expansion_blue"),
    ("Shattered Hand Clan", "clan_expansion_white"),
    ("Warsong Clan", "clan_expansion_red"),
    ("Bonechewer Clan", "clan_expansion_green"),
    ("Shadowmoon Clan", "clan_expansion_black"),
    ("Thunderlord Clan", "clan_expansion_purple"),
    ("Laughing Skull Clan", "clan_expansion_yellow"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(player: u8, team: u8) -> Entry {
        Entry {
            player,
            team,
            name: format!("p{player}"),
            tint: Tint::Plain,
            checkboxes: true,
        }
    }

    fn order(groups: &[Group]) -> Vec<(u8, Vec<u8>)> {
        groups
            .iter()
            .map(|g| (g.team, g.rows.iter().map(|r| r.player).collect()))
            .collect()
    }

    #[test]
    fn without_teams_everyone_is_one_group_with_me_first() {
        let g = build_groups((0..4).map(|p| entry(p, 0)).collect(), 2, 0);
        assert_eq!(order(&g), vec![(0, vec![2, 0, 1, 3])]);
    }

    #[test]
    fn my_team_comes_first_then_the_rest_by_number() {
        let teams = [3, 1, 2, 1, 2, 3];
        let entries = teams
            .iter()
            .enumerate()
            .map(|(p, &t)| entry(p as u8, t))
            .collect();
        let g = build_groups(entries, 4, 2);
        assert_eq!(
            order(&g),
            vec![(2, vec![4, 2]), (1, vec![1, 3]), (3, vec![0, 5])]
        );
    }

    #[test]
    fn a_local_player_outside_every_group_changes_nothing_but_the_order_of_teams() {
        let g = build_groups(vec![entry(0, 2), entry(1, 1)], 9, 0);
        assert_eq!(order(&g), vec![(1, vec![1]), (2, vec![0])]);
    }

    #[test]
    fn out_beats_teammate_and_teammates_need_a_team_game() {
        assert_eq!(tint(true, false, true, true, true), Tint::Out);
        assert_eq!(tint(false, false, true, true, false), Tint::Teammate);
        assert_eq!(tint(false, false, true, false, true), Tint::Teammate);
        assert_eq!(tint(false, false, false, true, true), Tint::Plain);
        assert_eq!(tint(false, false, true, false, false), Tint::Plain);
    }

    #[test]
    fn an_observer_is_blue_unless_they_left() {
        assert_eq!(tint(false, true, false, false, false), Tint::Observer);
        assert_eq!(tint(false, true, true, false, true), Tint::Observer);
        assert_eq!(tint(true, true, false, false, false), Tint::Out);
    }

    #[test]
    fn the_kick_column_takes_its_share_and_the_rest_keeps_its_proportions() {
        let plain = player_ratios(false);
        let host = player_ratios(true);
        assert_eq!(plain.len(), 4);
        assert_eq!(host.len(), 5);
        assert!((host[4] - KICK_SHARE).abs() < 1e-6);
        for i in 0..4 {
            assert!((host[i] / plain[i] - (1.0 - KICK_SHARE)).abs() < 1e-5);
        }
        let header: f32 = header_ratios(true).iter().sum();
        assert!(
            (header - 1.0).abs() < 1e-5,
            "the header still fills the row"
        );
        assert_eq!(header_ratios(false).len(), 4);
    }

    #[test]
    fn the_content_leaves_room_for_the_footer() {
        assert_eq!(content_height(2160.0), 2160.0 - 230.0 - 98.0 - 210.0);
    }

    #[test]
    fn stock_clan_names_have_keys() {
        assert_eq!(clan_key("Nation of Azeroth"), Some("clan_human_blue"));
        assert_eq!(
            clan_key("Laughing Skull Clan"),
            Some("clan_expansion_yellow")
        );
        assert_eq!(clan_key("Beau"), None);
        assert_eq!(clan_key(""), None);
    }
}
