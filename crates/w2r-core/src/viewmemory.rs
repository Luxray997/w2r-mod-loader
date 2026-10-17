//! What each watched view remembers, kept apart (replay viewing; see `viewmem.rs`). The game
//! remembers buildings and mines it has seen but cannot see now, and it updates that memory while
//! it draws, for whichever player the screen shows. Watching another player, or everything, would
//! write their sightings into the recorded player's memory, so each view gets its own memory,
//! and the real one is put back before the simulation continues.
//!
//! Generic over what is remembered (`S`) and pure, so it is tested without the game: the glue
//! passes closures that read the game's memory into an `S` and write one back.

/// Memory slots 0..=7 are players; this one is full vision, and the next all players' vision.
pub const FULL_VISION_SLOT: usize = 8;
pub const ALL_PLAYERS_SLOT: usize = 9;
const SLOTS: usize = 10;

pub struct ViewMemory<S> {
    /// The recorded player's own memory, held while another view is loaded.
    real: Option<S>,
    /// The slot whose memory is loaded in the game now.
    current: Option<usize>,
    /// Each slot's memory while it is not loaded.
    private: Vec<Option<S>>,
}

impl<S> Default for ViewMemory<S> {
    fn default() -> Self {
        ViewMemory {
            real: None,
            current: None,
            private: (0..SLOTS).map(|_| None).collect(),
        }
    }
}

impl<S> ViewMemory<S> {
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Loads `slot`'s memory into the game (a blank one the first time), saving what is there:
    /// the real memory on the way in, or the memory of the view that was loaded before.
    pub fn enter(
        &mut self,
        slot: usize,
        read: impl FnOnce() -> S,
        write: impl FnOnce(&S),
        blank: impl FnOnce() -> S,
    ) {
        if slot >= SLOTS || self.current == Some(slot) {
            return;
        }
        match self.current {
            Some(loaded) => self.private[loaded] = Some(read()),
            None => self.real = Some(read()),
        }
        let next = self.private[slot].take().unwrap_or_else(blank);
        write(&next);
        self.current = Some(slot);
    }

    /// Saves the loaded view's memory and puts the real one back. Does nothing if none is loaded.
    pub fn leave(&mut self, read: impl FnOnce() -> S, write: impl FnOnce(&S)) {
        let Some(loaded) = self.current.take() else {
            return;
        };
        self.private[loaded] = Some(read());
        if let Some(real) = self.real.take() {
            write(&real);
        }
    }

    /// Forgets every view (the replay ended); nothing is written.
    pub fn reset(&mut self) {
        *self = ViewMemory::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A fake game memory: one number.
    struct Game(RefCell<u32>);
    impl Game {
        fn enter(&self, m: &mut ViewMemory<u32>, slot: usize) {
            m.enter(
                slot,
                || *self.0.borrow(),
                |s| *self.0.borrow_mut() = *s,
                || 0,
            );
        }
        fn leave(&self, m: &mut ViewMemory<u32>) {
            m.leave(|| *self.0.borrow(), |s| *self.0.borrow_mut() = *s);
        }
    }

    #[test]
    fn a_view_starts_blank_and_the_real_memory_comes_back() {
        let game = Game(RefCell::new(7)); // the recorded player remembers 7
        let mut m = ViewMemory::default();
        game.enter(&mut m, 1);
        assert_eq!(*game.0.borrow(), 0, "player 1 starts remembering nothing");
        *game.0.borrow_mut() = 99; // the draw pass writes what player 1 sees
        game.leave(&mut m);
        assert_eq!(*game.0.borrow(), 7, "the real memory is untouched");
        assert_eq!(m.current(), None);
    }

    #[test]
    fn a_view_remembers_across_visits() {
        let game = Game(RefCell::new(7));
        let mut m = ViewMemory::default();
        game.enter(&mut m, 1);
        *game.0.borrow_mut() = 11;
        game.leave(&mut m);
        game.enter(&mut m, 1);
        assert_eq!(*game.0.borrow(), 11);
        game.leave(&mut m);
        assert_eq!(*game.0.borrow(), 7);
    }

    #[test]
    fn switching_straight_between_views_keeps_each_and_the_real_one() {
        let game = Game(RefCell::new(7));
        let mut m = ViewMemory::default();
        game.enter(&mut m, 1);
        *game.0.borrow_mut() = 11;
        game.enter(&mut m, 2); // no leave in between
        assert_eq!(*game.0.borrow(), 0);
        *game.0.borrow_mut() = 22;
        game.enter(&mut m, 1);
        assert_eq!(*game.0.borrow(), 11);
        game.leave(&mut m);
        assert_eq!(*game.0.borrow(), 7, "the real memory is back after view 1");
        game.enter(&mut m, 2);
        assert_eq!(*game.0.borrow(), 22);
    }

    #[test]
    fn entering_the_loaded_view_again_and_leaving_with_none_are_no_ops() {
        let game = Game(RefCell::new(7));
        let mut m = ViewMemory::default();
        game.leave(&mut m);
        assert_eq!(*game.0.borrow(), 7);
        game.enter(&mut m, FULL_VISION_SLOT);
        *game.0.borrow_mut() = 5;
        game.enter(&mut m, FULL_VISION_SLOT);
        assert_eq!(*game.0.borrow(), 5, "not reloaded");
        game.enter(&mut m, 99); // no such slot
        assert_eq!(m.current(), Some(FULL_VISION_SLOT));
    }

    #[test]
    fn reset_forgets_everything() {
        let game = Game(RefCell::new(7));
        let mut m = ViewMemory::default();
        game.enter(&mut m, 3);
        *game.0.borrow_mut() = 1;
        game.leave(&mut m);
        m.reset();
        game.enter(&mut m, 3);
        assert_eq!(*game.0.borrow(), 0, "blank again");
    }
}
