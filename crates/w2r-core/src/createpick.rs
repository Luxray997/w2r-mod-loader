//! What a plugin's create-screen callback may do to the map selection (docs/abi.md, "Create
//! screen"): count the indexed maps with a given player count and select one. The functions work
//! only while the host is inside that callback, on the game's main thread; the screen supplies
//! the [`MapPicker`] for exactly that long. Platform-neutral so the rules are tested anywhere.

use std::cell::{Cell, RefCell};

use w2r_abi::{W2R_E_INVALID, W2R_E_NOT_FOUND, W2R_E_STATE, W2R_OK, W2rStatus};

/// The most players a map has (Warcraft II).
pub const MAX_PLAYERS: u32 = 8;

/// The screen's side of the two host functions.
pub trait MapPicker {
    /// Maps in the index with `players` maximum players (0 = any). `Err(W2R_E_BUSY)` while the
    /// index is not built yet.
    /// `folder` limits it to that folder under the maps folder and below ("" = all).
    fn count(&mut self, players: u32, folder: &str) -> Result<u32, W2rStatus>;
    /// Makes the `nth` such map (index order) the selected map. `nth` is below the last count.
    fn select(&mut self, players: u32, folder: &str, nth: u32) -> W2rStatus;
}

thread_local! {
    /// The picker of the callback that is running right now, or none. The pointer is valid only
    /// inside [`with_picker`].
    static ACTIVE: Cell<Option<*mut (dyn MapPicker + 'static)>> = const { Cell::new(None) };
    /// The folder limit the running callback set, normalised; empty = none.
    static FOLDER: RefCell<String> = const { RefCell::new(String::new()) };
}

/// The longest folder limit kept.
const MAX_FOLDER: usize = 200;

/// `create_map_folder`: `\` separators, no leading or trailing ones. Only inside a callback.
pub fn set_folder(folder: &str) -> W2rStatus {
    if ACTIVE.with(|a| a.get()).is_none() {
        return W2R_E_STATE;
    }
    if folder.len() > MAX_FOLDER {
        return W2R_E_INVALID;
    }
    let parts: Vec<&str> = folder
        .split(['\\', '/'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.iter().any(|p| *p == "." || *p == ".." || p.contains(':')) {
        return W2R_E_INVALID;
    }
    FOLDER.with(|f| *f.borrow_mut() = parts.join("\\"));
    W2R_OK
}

fn folder() -> String {
    FOLDER.with(|f| f.borrow().clone())
}

/// Runs `f` with `picker` reachable from [`count`] and [`select`], restoring the previous one
/// (none, unless `f` nests) afterwards.
pub fn with_picker<R>(picker: &mut dyn MapPicker, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<*mut (dyn MapPicker + 'static)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|a| a.set(self.0));
            FOLDER.with(|f| f.borrow_mut().clear());
        }
    }
    FOLDER.with(|f| f.borrow_mut().clear());
    // SAFETY: the lifetime is erased only for as long as `Restore` is alive on this stack frame,
    // which is the whole time the pointer can be reached.
    let erased: *mut (dyn MapPicker + 'static) = unsafe { core::mem::transmute(picker) };
    let _restore = Restore(ACTIVE.with(|a| a.replace(Some(erased))));
    f()
}

fn with_active<R>(f: impl FnOnce(&mut dyn MapPicker) -> R) -> Result<R, W2rStatus> {
    // Taken out while it runs, so a re-entrant call from inside the picker finds nothing.
    let picker = ACTIVE.with(|a| a.take()).ok_or(W2R_E_STATE)?;
    struct Put(*mut (dyn MapPicker + 'static));
    impl Drop for Put {
        fn drop(&mut self) {
            ACTIVE.with(|a| a.set(Some(self.0)));
        }
    }
    let _put = Put(picker);
    // SAFETY: set by `with_picker`, which outlives this call; nothing else holds it meanwhile.
    Ok(f(unsafe { &mut *picker }))
}

/// `create_map_count`: `players` 0 to 8.
pub fn count(players: u32) -> Result<u32, W2rStatus> {
    if players > MAX_PLAYERS {
        return Err(W2R_E_INVALID);
    }
    let folder = folder();
    with_active(|p| p.count(players, &folder))?
}

/// `create_map_select`: `players` 0 to 8, `nth` below the current count.
pub fn select(players: u32, nth: u32) -> W2rStatus {
    if players > MAX_PLAYERS {
        return W2R_E_INVALID;
    }
    let folder = folder();
    match with_active(|p| match p.count(players, &folder) {
        Ok(n) if nth < n => p.select(players, &folder, nth),
        Ok(_) => W2R_E_NOT_FOUND,
        Err(e) => e,
    }) {
        Ok(status) => status,
        Err(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w2r_abi::{W2R_E_BUSY};

    struct Fake {
        ready: bool,
        maps: Vec<u32>,
        chosen: Option<usize>,
        folder: String,
    }

    impl Fake {
        fn matching(&self, players: u32) -> Vec<usize> {
            (0..self.maps.len())
                .filter(|&i| players == 0 || self.maps[i] == players)
                .collect()
        }
    }

    impl MapPicker for Fake {
        fn count(&mut self, players: u32, folder: &str) -> Result<u32, W2rStatus> {
            self.folder = folder.to_owned();
            if !self.ready {
                return Err(W2R_E_BUSY);
            }
            Ok(self.matching(players).len() as u32)
        }
        fn select(&mut self, players: u32, _folder: &str, nth: u32) -> W2rStatus {
            self.chosen = Some(self.matching(players)[nth as usize]);
            W2R_OK
        }
    }

    fn fake(ready: bool) -> Fake {
        Fake {
            ready,
            maps: vec![2, 4, 2, 8, 4],
            chosen: None,
            folder: String::new(),
        }
    }

    #[test]
    fn refused_outside_a_callback() {
        assert_eq!(count(0), Err(W2R_E_STATE));
        assert_eq!(select(0, 0), W2R_E_STATE);
    }

    #[test]
    fn counts_and_selects_by_player_count() {
        let mut f = fake(true);
        with_picker(&mut f, || {
            assert_eq!(count(0), Ok(5));
            assert_eq!(count(4), Ok(2));
            assert_eq!(count(3), Ok(0));
            assert_eq!(select(4, 1), W2R_OK);
        });
        assert_eq!(f.chosen, Some(4));
        // The picker is gone again afterwards.
        assert_eq!(count(0), Err(W2R_E_STATE));
    }

    #[test]
    fn the_folder_limit_is_normalised_and_lasts_one_callback() {
        assert_eq!(set_folder("mp"), W2R_E_STATE);
        let mut f = fake(true);
        with_picker(&mut f, || {
            assert_eq!(set_folder(" /mp\\Duels/ "), W2R_OK);
            assert_eq!(count(0), Ok(5));
            assert_eq!(set_folder("..\\x"), W2R_E_INVALID);
            assert_eq!(set_folder("C:\\x"), W2R_E_INVALID);
        });
        assert_eq!(f.folder, "mp\\Duels");
        with_picker(&mut f, || {
            assert_eq!(count(0), Ok(5));
        });
        assert_eq!(f.folder, "", "cleared with the callback");
    }

    #[test]
    fn bad_arguments() {
        let mut f = fake(true);
        with_picker(&mut f, || {
            assert_eq!(count(9), Err(W2R_E_INVALID));
            assert_eq!(select(9, 0), W2R_E_INVALID);
            assert_eq!(select(4, 2), W2R_E_NOT_FOUND);
            assert_eq!(select(3, 0), W2R_E_NOT_FOUND);
        });
        assert_eq!(f.chosen, None);
    }

    #[test]
    fn busy_while_the_index_builds() {
        let mut f = fake(false);
        with_picker(&mut f, || {
            assert_eq!(count(0), Err(W2R_E_BUSY));
            assert_eq!(select(0, 0), W2R_E_BUSY);
        });
    }
}
