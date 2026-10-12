//! One entry of the game's lobby slot table (`MpLobbySlots`, 8 entries of 0x26 bytes), as seen in
//! the logged snapshots of 2026-10-09 on build 2818:
//!
//! | Offset | Meaning |
//! |---|---|
//! | 0x00 | slot index |
//! | 0x08 | kind: 0 human, 1 computer, 3 empty |
//! | 0x09 | race |
//! | 0x0a | team, 1-based |
//! | 0x0d | name; a human's BattleTag is `Name#12345` in the lobby and `Name\0` then `12345\0` in a match |

pub const SLOT_LEN: usize = 0x26;
pub const SLOTS: usize = 8;
pub const NAME_AT: usize = 0x0d;
const MAX_DISCRIMINATOR: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Human,
    Computer,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Human => "human",
            Kind::Computer => "computer",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "human" => Some(Kind::Human),
            "computer" => Some(Kind::Computer),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Player {
    pub slot: u8,
    pub kind: Kind,
    /// The display name without the discriminator.
    pub name: String,
    /// `Name#12345`, humans only, when the game gave a discriminator.
    pub tag: Option<String>,
    pub team: u8,
    pub race: u8,
}

impl Player {
    /// What identifies the player in a roster: the BattleTag, else the plain name.
    pub fn identity(&self) -> &str {
        self.tag.as_deref().unwrap_or(&self.name)
    }
}

/// `None` for an empty slot or any kind this build does not know.
pub fn parse_slot(index: u8, bytes: &[u8; SLOT_LEN]) -> Option<Player> {
    let kind = match bytes[8] {
        0 => Kind::Human,
        1 => Kind::Computer,
        _ => return None,
    };
    let area = &bytes[NAME_AT..];
    let end = area.iter().position(|&b| b == 0).unwrap_or(area.len());
    let first = String::from_utf8_lossy(&area[..end]).into_owned();
    let (name, discriminator) = match first.split_once('#') {
        Some((n, d)) => (n.to_string(), Some(d.to_string())),
        None => {
            // In a match the '#' is a NUL: the digits are the next string.
            let rest = area.get(end + 1..).unwrap_or(&[]);
            let end2 = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            let digits = &rest[..end2];
            let d = (kind == Kind::Human && !digits.is_empty())
                .then(|| String::from_utf8_lossy(digits).into_owned());
            (first, d)
        }
    };
    let tag = match (kind, discriminator) {
        (Kind::Human, Some(d))
            if !d.is_empty()
                && d.len() <= MAX_DISCRIMINATOR
                && d.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some(format!("{name}#{d}"))
        }
        _ => None,
    };
    Some(Player {
        slot: index,
        kind,
        name,
        tag,
        team: bytes[10],
        race: bytes[9],
    })
}

/// Every occupied slot of the table.
pub fn parse_table(table: &[[u8; SLOT_LEN]; SLOTS]) -> Vec<Player> {
    table
        .iter()
        .enumerate()
        .filter_map(|(i, s)| parse_slot(i as u8, s))
        .collect()
}

/// The observer team in the lobby's team list (the ninth option, added by the observer plugin).
pub const OBSERVER_TEAM: u8 = 9;

/// True when at least two humans play on different teams: a game against another person, as
/// opposed to one human against computers or humans only on the same side. Observers do not count.
pub fn has_human_opponents(players: &[Player]) -> bool {
    let mut teams = players
        .iter()
        .filter(|p| p.kind == Kind::Human && p.team != OBSERVER_TEAM)
        .map(|p| p.team);
    match teams.next() {
        Some(first) => teams.any(|t| t != first),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(kind: u8, race: u8, team: u8, name: &[u8]) -> [u8; SLOT_LEN] {
        let mut s = [0u8; SLOT_LEN];
        s[8] = kind;
        s[9] = race;
        s[10] = team;
        s[NAME_AT..NAME_AT + name.len()].copy_from_slice(name);
        s
    }

    #[test]
    fn in_match_human_tag_is_two_strings() {
        // Captured 2026-10-09: `MrKrinkle`, NUL, `11951`.
        let p = parse_slot(6, &slot(0, 0, 1, b"MrKrinkle\x0011951")).unwrap();
        assert_eq!(p.kind, Kind::Human);
        assert_eq!(p.name, "MrKrinkle");
        assert_eq!(p.tag.as_deref(), Some("MrKrinkle#11951"));
        assert_eq!((p.slot, p.team, p.race), (6, 1, 0));
    }

    #[test]
    fn lobby_human_tag_has_the_hash() {
        let p = parse_slot(0, &slot(0, 0, 1, b"MrKrinkle#11951")).unwrap();
        assert_eq!(p.tag.as_deref(), Some("MrKrinkle#11951"));
        assert_eq!(p.identity(), "MrKrinkle#11951");
    }

    #[test]
    fn computer_has_no_tag_even_with_trailing_text() {
        let p = parse_slot(4, &slot(1, 1, 2, b"Burning Blade Clan")).unwrap();
        assert_eq!(p.kind, Kind::Computer);
        assert_eq!(p.name, "Burning Blade Clan");
        assert_eq!(p.tag, None);
        assert_eq!(p.identity(), "Burning Blade Clan");
        let q = parse_slot(4, &slot(1, 1, 2, b"Bot\x0012345")).unwrap();
        assert_eq!(q.tag, None);
    }

    #[test]
    fn human_without_a_discriminator_has_no_tag() {
        let p = parse_slot(0, &slot(0, 0, 1, b"Local")).unwrap();
        assert_eq!(p.tag, None);
        assert_eq!(p.identity(), "Local");
    }

    #[test]
    fn rejects_non_numeric_or_overlong_discriminators() {
        assert_eq!(parse_slot(0, &slot(0, 0, 1, b"A#12x")).unwrap().tag, None);
        assert_eq!(
            parse_slot(0, &slot(0, 0, 1, b"A#12345678901")).unwrap().tag,
            None
        );
        assert_eq!(parse_slot(0, &slot(0, 0, 1, b"A#")).unwrap().tag, None);
    }

    #[test]
    fn empty_and_unknown_kinds_are_skipped() {
        assert!(parse_slot(0, &slot(3, 0, 0, b"")).is_none());
        assert!(parse_slot(0, &slot(7, 0, 0, b"")).is_none());
        let mut table = [[0u8; SLOT_LEN]; SLOTS];
        for t in table.iter_mut() {
            t[8] = 3;
        }
        table[6] = slot(0, 0, 1, b"MrKrinkle\x0011951");
        let players = parse_table(&table);
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].slot, 6);
    }

    fn p(kind: Kind, team: u8) -> Player {
        Player {
            slot: 0,
            kind,
            name: "x".into(),
            tag: None,
            team,
            race: 0,
        }
    }

    #[test]
    fn human_opponents_need_two_humans_on_different_teams() {
        use Kind::{Computer as C, Human as H};
        assert!(has_human_opponents(&[p(H, 1), p(H, 2)]));
        assert!(has_human_opponents(&[p(H, 1), p(H, 1), p(C, 2), p(H, 3)]));
        // one human against the computer, or humans allied against it
        assert!(!has_human_opponents(&[p(H, 1), p(C, 2)]));
        assert!(!has_human_opponents(&[p(H, 1), p(H, 1), p(C, 2)]));
        // an observer is not an opponent
        assert!(!has_human_opponents(&[p(H, 1), p(H, OBSERVER_TEAM)]));
        assert!(!has_human_opponents(&[]));
    }
}
