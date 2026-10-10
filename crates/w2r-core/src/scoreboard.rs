//! Counting what each player has, for the replay panel (`hudpanel.rs`). The unit slots are the
//! game's 0x98-byte records (offsets from War2Mod's `names.h`, confirmed against the game's own
//! `RecomputeHidden`, which reads the flags byte at +0x1e and the type at +0x27). Pure, so it is
//! tested without the game.

pub const STRIDE: usize = 0x98;
const OFFSET_FLAGS: usize = 0x1e;
const OFFSET_TYPE: usize = 0x27;
const OFFSET_OWNER: usize = 0x2c;
/// A slot that is free (0x1), dying (0x2) or dead (0x4) is not counted; hidden (0x8, inside a
/// building or mine) still is.
const NOT_ALIVE: u8 = 0x07;
/// `UnitTypeFlags` bits (War2Mod `defs.h`).
pub const TYPE_BUILDING: u32 = 0x20;
pub const TYPE_WORKER: u32 = 0x100;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub army: u16,
    pub workers: u16,
    pub buildings: u16,
}

impl Counts {
    pub fn is_empty(&self) -> bool {
        self.army == 0 && self.workers == 0 && self.buildings == 0
    }
}

/// Adds one unit slot to the tallies. `type_flags` gives the flags of a unit type.
pub fn count(slot: &[u8], type_flags: impl Fn(u8) -> u32, counts: &mut [Counts; 8]) {
    if slot.len() < STRIDE || slot[OFFSET_FLAGS] & NOT_ALIVE != 0 {
        return;
    }
    let owner = usize::from(slot[OFFSET_OWNER]);
    let Some(c) = counts.get_mut(owner) else {
        return;
    };
    let flags = type_flags(slot[OFFSET_TYPE]);
    if flags & TYPE_BUILDING != 0 {
        c.buildings += 1;
    } else if flags & TYPE_WORKER != 0 {
        c.workers += 1;
    } else {
        c.army += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(flags: u8, ty: u8, owner: u8) -> Vec<u8> {
        let mut s = vec![0u8; STRIDE];
        s[OFFSET_FLAGS] = flags;
        s[OFFSET_TYPE] = ty;
        s[OFFSET_OWNER] = owner;
        s
    }

    fn flags_of(ty: u8) -> u32 {
        match ty {
            2 | 3 => TYPE_WORKER,
            74 | 75 => TYPE_BUILDING,
            _ => 0,
        }
    }

    #[test]
    fn units_are_split_by_owner_and_kind() {
        let mut c = [Counts::default(); 8];
        for s in [
            slot(0, 3, 1),
            slot(0, 3, 1),
            slot(0, 75, 1),
            slot(0, 0, 1),
            slot(0, 2, 0),
        ] {
            count(&s, flags_of, &mut c);
        }
        assert_eq!(c[1], Counts { army: 1, workers: 2, buildings: 1 });
        assert_eq!(c[0], Counts { army: 0, workers: 1, buildings: 0 });
        assert!(c[2].is_empty());
    }

    #[test]
    fn free_dying_and_dead_slots_do_not_count_but_hidden_ones_do() {
        let mut c = [Counts::default(); 8];
        for flags in [0x01, 0x02, 0x04] {
            count(&slot(flags, 3, 0), flags_of, &mut c);
        }
        assert!(c[0].is_empty());
        count(&slot(0x08, 3, 0), flags_of, &mut c);
        assert_eq!(c[0].workers, 1);
    }

    #[test]
    fn neutral_owners_and_short_slots_are_ignored() {
        let mut c = [Counts::default(); 8];
        count(&slot(0, 3, 15), flags_of, &mut c);
        count(&[0u8; 10], flags_of, &mut c);
        assert!(c.iter().all(Counts::is_empty));
    }
}
