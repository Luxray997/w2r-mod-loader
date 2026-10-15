//! Where and when the fighting was, from the kill events of a recording: a heat grid over the map,
//! an intensity timeline, and the battles (kills that belong together in time and place). Pure
//! arithmetic over `Event` lists, so it is tested without the game.

use crate::reader::Event;

/// What a kill is worth on the map: losing a building is a bigger moment than losing a unit.
pub const UNIT_WEIGHT: u32 = 1;
pub const BUILDING_WEIGHT: u32 = 2;

fn weight(ev: &Event) -> u32 {
    if ev.kind == 1 { BUILDING_WEIGHT } else { UNIT_WEIGHT }
}

/// The first unit type id that is a building (the game's kill credit tests `type < 0x3a`).
pub const FIRST_BUILDING_TYPE: u8 = 0x3a;

/// The event for the game's kill credit (`FUN_004def00`, docs/unit-data-loading.md and
/// docs/replay-research.md): `killer` is the player who gets the credit, the rest is read from the
/// unit that was lost. The game credits nothing when both are the same player, and neither do we.
/// `px` and `py` are the unit's map pixels (32 per tile); a position off the map is clamped to
/// its edge. The turn is filled in by the recorder.
pub fn kill_event(killer: u8, victim: u8, unit_type: u8, px: i16, py: i16) -> Option<Event> {
    if killer == victim || killer >= 8 || victim >= 8 {
        return None;
    }
    let tile = |p: i16| (p >> 5).clamp(0, 127) as u8;
    Some(Event {
        turn: 0,
        kind: u8::from(unit_type >= FIRST_BUILDING_TYPE),
        killer,
        victim,
        unit_type,
        x: tile(px),
        y: tile(py),
    })
}

/// A square grid of weights, one cell per map tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    pub side: usize,
    pub cells: Vec<u32>,
}

impl Grid {
    pub fn new(side: usize) -> Grid {
        Grid { side, cells: vec![0; side * side] }
    }

    pub fn at(&self, x: usize, y: usize) -> u32 {
        if x < self.side && y < self.side { self.cells[y * self.side + x] } else { 0 }
    }

    pub fn max(&self) -> u32 {
        self.cells.iter().copied().max().unwrap_or(0)
    }

    /// Spreads every cell over its neighbours within `radius` tiles with a falloff, so a few kills
    /// read as a warm area rather than single dots. A cell's own weight stays the strongest.
    pub fn blurred(&self, radius: usize) -> Grid {
        let mut out = Grid::new(self.side);
        let r = radius as i32;
        for y in 0..self.side {
            for x in 0..self.side {
                let w = self.cells[y * self.side + x];
                if w == 0 {
                    continue;
                }
                for dy in -r..=r {
                    for dx in -r..=r {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx < 0 || ny < 0 || nx >= self.side as i32 || ny >= self.side as i32 {
                            continue;
                        }
                        let d = dx.abs().max(dy.abs()) as u32;
                        // full weight at the centre, a share of it that shrinks with distance
                        let share = (r as u32 + 1 - d) * 100 / (r as u32 + 1);
                        out.cells[ny as usize * self.side + nx as usize] += w * share;
                    }
                }
            }
        }
        out
    }

    /// The grid scaled to bytes, the hottest cell being 255, for drawing a colour ramp.
    pub fn bytes(&self) -> Vec<u8> {
        let m = self.max().max(1);
        self.cells.iter().map(|&c| ((u64::from(c) * 255) / u64::from(m)) as u8).collect()
    }
}

/// Weights of the kills at or before `upto` (`None` for the whole game), one cell per tile.
/// Events outside a `side` x `side` map are ignored.
pub fn grid(events: &[Event], side: usize, upto: Option<u32>) -> Grid {
    let mut g = Grid::new(side);
    for ev in events {
        if upto.is_some_and(|t| ev.turn > t) {
            break;
        }
        let (x, y) = (usize::from(ev.x), usize::from(ev.y));
        if x < side && y < side {
            g.cells[y * side + x] += weight(ev);
        }
    }
    g
}

/// The same, for kills in the turn range `from..=to`: a time slider over the heat map.
pub fn grid_between(events: &[Event], side: usize, from: u32, to: u32) -> Grid {
    let mut g = Grid::new(side);
    for ev in events.iter().filter(|e| e.turn >= from && e.turn <= to) {
        let (x, y) = (usize::from(ev.x), usize::from(ev.y));
        if x < side && y < side {
            g.cells[y * side + x] += weight(ev);
        }
    }
    g
}

/// Weight of kills per time bucket across `total_turns`, `buckets` of them. The last event turn
/// can pass `total_turns` in a cut-off file; it lands in the last bucket.
pub fn intensity(events: &[Event], total_turns: u32, buckets: usize) -> Vec<u32> {
    let mut out = vec![0u32; buckets];
    if buckets == 0 || total_turns == 0 {
        return out;
    }
    for ev in events {
        let i = (u64::from(ev.turn) * buckets as u64 / u64::from(total_turns)) as usize;
        out[i.min(buckets - 1)] += weight(ev);
    }
    out
}

/// One fight: kills that follow each other closely in time and lie close together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battle {
    pub start: u32,
    pub end: u32,
    pub weight: u32,
    pub kills: u32,
    /// How many of the kills were units (a fight needs some, a raid on buildings alone is not one).
    pub units: u32,
    /// The kill-weighted centre, in map tiles.
    pub x: u8,
    pub y: u8,
    /// Bit `p` is set for each player who lost something.
    pub victims: u8,
}

struct Open {
    start: u32,
    last: u32,
    weight: u32,
    kills: u32,
    units: u32,
    sum_x: u64,
    sum_y: u64,
    victims: u8,
}

impl Open {
    fn centre(&self) -> (i64, i64) {
        let w = i64::from(self.weight.max(1));
        (self.sum_x as i64 / w, self.sum_y as i64 / w)
    }

    fn close(&self) -> Battle {
        let (x, y) = self.centre();
        Battle {
            start: self.start,
            end: self.last,
            weight: self.weight,
            kills: self.kills,
            units: self.units,
            x: x as u8,
            y: y as u8,
            victims: self.victims,
        }
    }
}

/// Groups events into battles. A kill joins an open fight when the last kill there was at most
/// `gap` turns ago and it is within `radius` tiles of the fight's centre; otherwise it starts a new
/// one, so two fights at once in different places stay apart. Fights with less than `min_weight`
/// or fewer than `min_units` unit kills are dropped (a lone worker dying, or farms burning with
/// nobody fighting, is not a battle). Sorted by start turn.
pub fn battles(
    events: &[Event],
    gap: u32,
    radius: u32,
    min_weight: u32,
    min_units: u32,
) -> Vec<Battle> {
    let mut open: Vec<Open> = Vec::new();
    let mut done: Vec<Battle> = Vec::new();
    for ev in events {
        // retire fights that went quiet
        let mut i = 0;
        while i < open.len() {
            if ev.turn.saturating_sub(open[i].last) > gap {
                done.push(open.swap_remove(i).close());
            } else {
                i += 1;
            }
        }
        let w = weight(ev);
        let (ex, ey) = (i64::from(ev.x), i64::from(ev.y));
        let near = open.iter().position(|o| {
            let (cx, cy) = o.centre();
            (cx - ex).abs().max((cy - ey).abs()) <= i64::from(radius)
        });
        let o = match near {
            Some(i) => &mut open[i],
            None => {
                open.push(Open { start: ev.turn, last: ev.turn, weight: 0, kills: 0, units: 0, sum_x: 0, sum_y: 0, victims: 0 });
                open.last_mut().unwrap()
            }
        };
        o.last = ev.turn;
        o.weight += w;
        o.kills += 1;
        o.units += u32::from(ev.kind == 0);
        o.sum_x += u64::from(w) * ex as u64;
        o.sum_y += u64::from(w) * ey as u64;
        if ev.victim < 8 {
            o.victims |= 1 << ev.victim;
        }
    }
    done.extend(open.iter().map(Open::close));
    done.retain(|b| b.weight >= min_weight && b.units >= min_units);
    done.sort_by_key(|b| (b.start, b.end));
    done
}

/// How kills are grouped into battles for the timeline and jump keys: a fight goes quiet after
/// `BATTLE_GAP` turns (about nine seconds), two kills are the same fight within `BATTLE_RADIUS`
/// tiles, and a fight needs `BATTLE_MIN_WEIGHT` in all and `BATTLE_MIN_UNITS` unit deaths to count,
/// so a lone loss, or buildings burning with nobody fighting, is not a battle.
pub const BATTLE_GAP: u32 = 150;
pub const BATTLE_RADIUS: u32 = 14;
pub const BATTLE_MIN_WEIGHT: u32 = 6;
pub const BATTLE_MIN_UNITS: u32 = 3;

/// The battles of a recording with the settings above.
pub fn detect(events: &[Event]) -> Vec<Battle> {
    battles(events, BATTLE_GAP, BATTLE_RADIUS, BATTLE_MIN_WEIGHT, BATTLE_MIN_UNITS)
}

/// Timeline bar heights, 0 to 255, from `intensity` buckets. The square root keeps a small skirmish
/// visible next to a huge battle; any bucket with a kill is at least 1 so it shows at all.
pub fn graph(intensity: &[u32]) -> Vec<u8> {
    let top = intensity.iter().copied().max().unwrap_or(0).max(1) as f32;
    intensity
        .iter()
        .map(|&v| {
            if v == 0 {
                0
            } else {
                ((v as f32 / top).sqrt() * 255.0).round().clamp(1.0, 255.0) as u8
            }
        })
        .collect()
}

/// The battle to jump to from `turn`: the first one that starts more than `lead` turns after it
/// (going forward), or the last one that started more than `lead` turns before it (going back).
/// The caller aims `lead` turns ahead of the returned start so the fight is seen coming.
pub fn jump(battles: &[Battle], turn: u32, forward: bool, lead: u32) -> Option<&Battle> {
    if forward {
        battles.iter().find(|b| b.start > turn.saturating_add(lead))
    } else {
        battles.iter().rev().find(|b| b.start.saturating_add(lead) < turn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(turn: u32, kind: u8, victim: u8, x: u8, y: u8) -> Event {
        Event { turn, kind, killer: 1 - victim.min(1), victim, unit_type: 0, x, y }
    }

    #[test]
    fn the_graph_keeps_small_fights_visible_and_empty_time_flat() {
        assert_eq!(graph(&[0, 1, 100, 25]), vec![0, 26, 255, 128]);
        assert_eq!(graph(&[0, 0]), vec![0, 0]);
        assert!(graph(&[]).is_empty());
        // a single kill next to a huge fight still shows
        assert_eq!(graph(&[1, 1_000_000])[0], 1);
    }

    #[test]
    fn buildings_burning_with_nobody_fighting_is_not_a_battle() {
        // a raid on four farms: plenty of weight, but no unit died
        let farms: Vec<Event> = (0..4).map(|i| ev(300 + i * 20, 1, 0, 40, 40)).collect();
        assert!(farms.iter().map(weight).sum::<u32>() >= BATTLE_MIN_WEIGHT);
        assert!(detect(&farms).is_empty());
        // the same farms with a few units dying around them are one
        let mut fight = farms.clone();
        fight.extend((0..3).map(|i| ev(310 + i * 15, 0, 1, 41, 40)));
        fight.sort_by_key(|e| e.turn);
        let b = detect(&fight);
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].units, b[0].kills), (3, 7));
    }

    #[test]
    fn detection_drops_lone_losses() {
        let lone = [ev(100, 1, 0, 5, 5)];
        assert!(detect(&lone).is_empty());
        let fight: Vec<Event> = (0..8).map(|i| ev(200 + i * 5, 0, 0, 30, 30)).collect();
        assert_eq!(detect(&fight).len(), 1);
    }

    #[test]
    fn the_games_kill_credit_becomes_an_event() {
        let e = kill_event(1, 0, 6, 32 * 20 + 5, 32 * 40).unwrap();
        assert_eq!((e.kind, e.killer, e.victim, e.unit_type, e.x, e.y), (0, 1, 0, 6, 20, 40));
        assert_eq!(kill_event(1, 0, 0x3a, 0, 0).unwrap().kind, 1);
        assert_eq!(kill_event(1, 0, 0x39, 0, 0).unwrap().kind, 0);
        // the same player gets no credit, and players above 7 are not players
        assert_eq!(kill_event(2, 2, 6, 0, 0), None);
        assert_eq!(kill_event(9, 0, 6, 0, 0), None);
        // off the map is clamped to its edge
        let e = kill_event(1, 0, 6, -40, i16::MAX).unwrap();
        assert_eq!((e.x, e.y), (0, 127));
    }

    #[test]
    fn grid_weights_buildings_more_and_honours_the_time_limit() {
        let e = [ev(5, 0, 0, 3, 4), ev(6, 1, 0, 3, 4), ev(50, 0, 1, 9, 9)];
        let g = grid(&e, 16, None);
        assert_eq!(g.at(3, 4), UNIT_WEIGHT + BUILDING_WEIGHT);
        assert_eq!(g.at(9, 9), UNIT_WEIGHT);
        assert_eq!(grid(&e, 16, Some(10)).at(9, 9), 0);
        assert_eq!(grid_between(&e, 16, 6, 60).at(3, 4), BUILDING_WEIGHT);
        // off the map is ignored, not a panic
        assert_eq!(grid(&[ev(1, 0, 0, 200, 200)], 16, None).max(), 0);
    }

    #[test]
    fn blur_spreads_weight_with_the_centre_strongest() {
        let mut g = Grid::new(9);
        g.cells[4 * 9 + 4] = 10;
        let b = g.blurred(2);
        assert!(b.at(4, 4) > b.at(5, 4));
        assert!(b.at(5, 4) > b.at(6, 4));
        assert_eq!(b.at(7, 4), 0);
        assert_eq!(b.bytes()[4 * 9 + 4], 255);
        assert_eq!(Grid::new(4).bytes(), vec![0; 16]);
    }

    #[test]
    fn blur_at_the_map_edge_stays_inside() {
        let mut g = Grid::new(4);
        g.cells[0] = 3;
        g.cells[15] = 3;
        let b = g.blurred(3);
        assert!(b.at(0, 0) > 0 && b.at(3, 3) > 0);
    }

    #[test]
    fn intensity_buckets_cover_the_game() {
        let e = [ev(0, 0, 0, 1, 1), ev(49, 0, 0, 1, 1), ev(50, 1, 0, 1, 1), ev(99, 0, 0, 1, 1), ev(500, 0, 0, 1, 1)];
        assert_eq!(intensity(&e, 100, 2), vec![2, 3 + BUILDING_WEIGHT - 1]);
        assert_eq!(intensity(&e, 0, 4), vec![0; 4]);
        assert!(intensity(&e, 100, 0).is_empty());
    }

    #[test]
    fn close_kills_form_one_battle_and_far_ones_another() {
        let e = [
            ev(100, 0, 0, 20, 20),
            ev(110, 0, 1, 22, 21),
            ev(130, 0, 0, 21, 20),
            // far away, at the same time: a second fight
            ev(135, 0, 1, 90, 90),
            ev(140, 0, 1, 91, 90),
            // the first area is quiet for a long time, then flares again
            ev(900, 0, 0, 20, 20),
            ev(905, 0, 0, 20, 21),
        ];
        let b = battles(&e, 60, 12, 2, 0);
        assert_eq!(b.len(), 3);
        assert_eq!((b[0].start, b[0].end, b[0].kills), (100, 130, 3));
        assert_eq!((b[0].x, b[0].y), (21, 20));
        assert_eq!(b[0].victims, 0b11);
        assert_eq!((b[1].start, b[1].x, b[1].y), (135, 90, 90));
        assert_eq!(b[2].start, 900);
    }

    #[test]
    fn a_lone_kill_is_not_a_battle() {
        let e = [ev(10, 0, 0, 5, 5), ev(500, 1, 0, 60, 60)];
        let b = battles(&e, 60, 12, 2, 0);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].start, 500);
        assert!(battles(&[], 60, 12, 1, 0).is_empty());
    }

    #[test]
    fn jumping_skips_the_fight_in_progress() {
        let e = [ev(100, 1, 0, 5, 5), ev(400, 1, 0, 50, 50), ev(900, 1, 1, 80, 80)];
        let b = battles(&e, 30, 10, 1, 0);
        assert_eq!(jump(&b, 0, true, 20).map(|x| x.start), Some(100));
        assert_eq!(jump(&b, 100, true, 20).map(|x| x.start), Some(400));
        assert_eq!(jump(&b, 880, true, 20).map(|x| x.start), None);
        assert_eq!(jump(&b, 400, false, 20).map(|x| x.start), Some(100));
        assert_eq!(jump(&b, 1000, false, 20).map(|x| x.start), Some(900));
        assert_eq!(jump(&b, 100, false, 20).map(|x| x.start), None);
    }
}
