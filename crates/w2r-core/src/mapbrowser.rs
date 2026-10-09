//! The map browser's state and rules (docs/create-screen.md, slice 2), ported from wc2r-plus
//! `features/ui/map_browser.cpp`: folder navigation under the maps root, the size filter, the
//! search index and the choice. No game access: the game sits behind [`Source`], so everything
//! here is tested on any target. Paths are the game's, with `\` separators.
//!
//! The search index is kept on disk between launches ([`crate::mapsearchcache`]) and reload also
//! tells the game's registry to rescan the maps folder, so a map copied in while the game runs
//! shows up.

/// The index is capped so a huge maps folder cannot stall the game or eat memory.
pub const MAX_INDEX_DEPTH: usize = 8;
pub const MAX_INDEXED_MAPS: usize = 4000;
/// Longest name, path or description kept from the game's strings.
pub const MAX_TEXT: usize = 512;
pub const MAX_SEARCH: usize = 95;

/// A map in the game's live list for the current folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveEntry {
    pub name: String,
    pub path: String,
    pub desc: String,
    pub players: u32,
    pub dim: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedMap {
    pub name: String,
    pub path: String,
    pub desc: String,
    pub rel_dir: String,
    pub players: u32,
    pub dim: u32,
}

/// The map the user highlighted in the browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub name: String,
    pub path: String,
    pub desc: String,
    pub rel_dir: String,
    pub players: u32,
    pub dim: u32,
}

/// What the browser needs from the game. Implemented over the game's lists by the view and by a
/// fake in tests.
pub trait Source {
    /// The folder the game's map list currently shows (relative path, `\` separated).
    fn rel_path(&self) -> String;
    /// Points the game's list at `rel` and refreshes it, without touching the folder label.
    fn set_rel_path(&mut self, rel: &str);
    /// The label of the folder dropdown (the last path component).
    fn set_folder_label(&mut self, label: &str);
    /// The maps in the current folder.
    fn live_entries(&self) -> Vec<LiveEntry>;
    /// The sub-folders of the current folder, without the parent entry.
    fn folder_names(&self) -> Vec<String>;
    /// Asks the game to rescan the whole maps folder into its registry (reload). Default: nothing.
    fn rescan_registry(&mut self) {}
    /// Where the search-index cache lives; `None` keeps the index in memory only.
    fn cache_file(&self) -> Option<std::path::PathBuf> {
        None
    }
    /// Size and modification time of a file or folder named by a game path, for the cache.
    fn stat(&self, _path: &str) -> Option<crate::mapsearchcache::Stamp> {
        None
    }
}

/// Control characters and overlong text would reach the game's label functions; both are cut.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect()
}

/// The size filter from the dropdown: "all" is the first option, otherwise a name ending in
/// `_<dim>` (`mapsize_64`) or the dimension itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SizeFilter {
    all: bool,
    wanted: String,
}

impl SizeFilter {
    pub fn new(options: &[String], current: &str) -> SizeFilter {
        let all = options.is_empty() || options[0] == current;
        let wanted = current.rsplit_once('_').map_or(current, |(_, d)| d);
        SizeFilter {
            all,
            wanted: wanted.to_owned(),
        }
    }

    pub fn passes(&self, dim: u32) -> bool {
        self.all || dim.to_string() == self.wanted
    }
}

pub fn last_component(path: &str) -> &str {
    path.rsplit_once('\\').map_or(path, |(_, last)| last)
}

/// ASCII case-insensitive substring test; an empty needle matches everything.
pub fn contains_no_case(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    // No allocation: this runs for every indexed map on every frame while the player types.
    let (hay, needle) = (hay.as_bytes(), needle.as_bytes());
    needle.len() <= hay.len()
        && hay
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
}

#[derive(Debug, Default)]
pub struct Browser {
    open: bool,
    search: String,
    choice: Option<Choice>,
    index: Vec<IndexedMap>,
    index_built: bool,
    /// Every folder the last walk visited, for the cache to stamp.
    dirs: Vec<String>,
    /// After a reload the next index is built from the game, not read from the cache.
    skip_cache_once: bool,
    index_from_cache: bool,
    maps_root: Option<String>,
    /// A walk that is spread over several frames and not finished yet.
    walk: Option<Walk>,
    /// How long the last walk took in total, across all its slices.
    last_walk: std::time::Duration,
}

/// The unfinished part of a time-sliced walk: folders still to visit, depth first, in the order
/// a recursive walk would visit them.
#[derive(Debug)]
struct Walk {
    stack: Vec<(String, usize)>,
    root: String,
    /// The folder the game's list showed when the walk began.
    here: String,
    cache: Option<std::path::PathBuf>,
    spent: std::time::Duration,
}

impl Browser {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Opens with the map the game currently has selected highlighted (if it has one).
    pub fn open(&mut self, selected: Option<Choice>) {
        self.open = true;
        self.search.clear();
        self.choice = selected.filter(|c| !c.name.is_empty());
    }

    pub fn search(&self) -> &str {
        &self.search
    }

    pub fn search_mut(&mut self) -> &mut String {
        &mut self.search
    }

    pub fn searching(&self) -> bool {
        !self.search.is_empty()
    }

    pub fn choice(&self) -> Option<&Choice> {
        self.choice.as_ref()
    }

    /// The first folder seen (the shortest) is the maps root, which nothing may go above.
    pub fn observe_root(&mut self, rel: &str) {
        if rel.is_empty() {
            return;
        }
        if self.maps_root.as_ref().is_none_or(|r| rel.len() < r.len()) {
            self.maps_root = Some(rel.to_owned());
        }
    }

    pub fn maps_root(&self) -> Option<&str> {
        self.maps_root.as_deref()
    }

    pub fn at_root(&self, rel: &str) -> bool {
        self.maps_root.as_ref().is_none_or(|r| rel.len() <= r.len())
    }

    pub fn set_folder(&self, src: &mut dyn Source, rel: &str) {
        src.set_rel_path(rel);
        src.set_folder_label(last_component(rel));
    }

    pub fn descend(&self, src: &mut dyn Source, folder: &str) {
        let rel = format!("{}\\{}", src.rel_path(), folder);
        self.set_folder(src, &rel);
    }

    pub fn go_up(&self, src: &mut dyn Source) {
        let rel = src.rel_path();
        if self.at_root(&rel) {
            return;
        }
        let Some((up, _)) = rel.rsplit_once('\\') else {
            return;
        };
        let root = self.maps_root.as_deref().unwrap_or("");
        let target = if up.len() < root.len() { root } else { up };
        let target = target.to_owned();
        self.set_folder(src, &target);
    }

    /// The reload button: the game rescans the maps folder, the current folder is re-read, and
    /// the search index is dropped (the next one is walked afresh, not read from the cache).
    pub fn reload(&mut self, src: &mut dyn Source) {
        src.rescan_registry();
        let rel = src.rel_path();
        src.set_rel_path(&rel);
        self.index.clear();
        self.dirs.clear();
        self.walk = None;
        self.index_built = false;
        self.skip_cache_once = true;
    }

    /// True once the search index has been filled (from the cache or by a walk).
    pub fn index_ready(&self) -> bool {
        self.index_built
    }

    pub fn index_len(&self) -> usize {
        self.index.len()
    }

    pub fn indexed(&self) -> &[IndexedMap] {
        &self.index
    }

    /// The indexed maps with `players` maximum players (0 = any), in index order.
    pub fn with_players(&self, players: u32) -> impl Iterator<Item = &IndexedMap> {
        self.index
            .iter()
            .filter(move |m| players == 0 || m.players == players)
    }

    /// Fills the search index in one go (tests and callers that do not mind waiting); see
    /// [`Browser::step_index`].
    pub fn build_index(&mut self, src: &mut dyn Source) {
        self.step_index(src, None);
    }

    /// Fills the search index, once, until a reload: from the cache if it is still true of the
    /// disk, else by walking every folder under the maps root through the game's own list (the
    /// only way to read them), which is then cached. Each folder costs the game a list refresh, so
    /// with a big collection the walk takes seconds: with a `budget` it stops after that long and
    /// carries on at the next call (one per frame), and the game's folder is put back at the end of
    /// every slice so nothing else sees a half-walked list. Returns true once the index is ready.
    pub fn step_index(
        &mut self,
        src: &mut dyn Source,
        budget: Option<std::time::Duration>,
    ) -> bool {
        if self.index_built {
            return true;
        }
        if self.walk.is_none() {
            let Some(root) = self.maps_root.clone() else {
                return false;
            };
            let cache = src.cache_file();
            let skip = core::mem::take(&mut self.skip_cache_once);
            if !skip
                && let Some(file) = &cache
                && let Some(maps) = crate::mapsearchcache::load(file, &root, &|p| src.stat(p))
            {
                self.index = maps;
                self.index_built = true;
                self.index_from_cache = true;
                self.last_walk = std::time::Duration::ZERO;
                return true;
            }
            self.index_from_cache = false;
            self.index.clear();
            self.dirs.clear();
            self.walk = Some(Walk {
                stack: vec![(root.clone(), 0)],
                root,
                here: src.rel_path(),
                cache,
                spent: std::time::Duration::ZERO,
            });
        }
        let mut walk = self.walk.take().expect("set above");
        let started = std::time::Instant::now();
        while let Some((rel, depth)) = walk.stack.pop() {
            if depth > MAX_INDEX_DEPTH || self.index.len() >= MAX_INDEXED_MAPS {
                continue;
            }
            src.set_rel_path(&rel);
            self.dirs.push(rel.clone());
            for e in src.live_entries() {
                if self.index.len() >= MAX_INDEXED_MAPS {
                    break;
                }
                self.index.push(IndexedMap {
                    name: e.name,
                    path: e.path,
                    desc: e.desc,
                    rel_dir: rel.clone(),
                    players: e.players,
                    dim: e.dim,
                });
            }
            // Pushed backwards so the first child is popped (visited) first.
            for child in src.folder_names().into_iter().rev() {
                walk.stack.push((format!("{rel}\\{child}"), depth + 1));
            }
            if budget.is_some_and(|b| started.elapsed() >= b) && !walk.stack.is_empty() {
                walk.spent += started.elapsed();
                self.set_folder(src, &walk.here);
                self.walk = Some(walk);
                return false;
            }
        }
        walk.spent += started.elapsed();
        self.last_walk = walk.spent;
        self.index_built = true;
        self.set_folder(src, &walk.here);
        if let Some(file) = &walk.cache {
            // A cache that cannot be written only costs the next launch a walk.
            let _ = crate::mapsearchcache::save(file, &walk.root, &self.dirs, &self.index, &|p| {
                src.stat(p)
            });
        }
        true
    }

    /// True while a walk has started and not finished.
    pub fn indexing(&self) -> bool {
        self.walk.is_some()
    }

    /// How long the last walk took, all its slices together.
    pub fn last_walk(&self) -> std::time::Duration {
        self.last_walk
    }

    /// Drops an unfinished walk (the search was cleared or the browser closed). The game's
    /// folder was already put back at the end of the last slice, so there is nothing to undo.
    pub fn cancel_walk(&mut self) {
        if self.walk.take().is_some() {
            self.index.clear();
            self.dirs.clear();
        }
    }

    /// True when the current index came from the cache rather than a walk.
    pub fn index_from_cache(&self) -> bool {
        self.index_from_cache
    }

    pub fn choose_live(&mut self, e: &LiveEntry, rel_dir: &str) {
        self.choice = Some(Choice {
            name: e.name.clone(),
            path: e.path.clone(),
            desc: e.desc.clone(),
            rel_dir: rel_dir.to_owned(),
            players: e.players,
            dim: e.dim,
        });
    }

    pub fn choose_indexed(&mut self, m: &IndexedMap) {
        self.choice = Some(Choice {
            name: m.name.clone(),
            path: m.path.clone(),
            desc: m.desc.clone(),
            rel_dir: m.rel_dir.clone(),
            players: m.players,
            dim: m.dim,
        });
    }

    /// The position of the chosen map in the game's live list, after pointing the list at the
    /// choice's folder. `None` when the choice is gone (the folder changed under it).
    pub fn resolve_choice(&self, src: &mut dyn Source) -> Option<usize> {
        let c = self.choice.as_ref()?;
        if src.rel_path() != c.rel_dir {
            self.set_folder(src, &c.rel_dir);
        }
        src.live_entries()
            .iter()
            .position(|e| e.name == c.name && e.path == c.path)
    }

    /// "Maps/sub/folder/", or just "Maps/" while searching (wc2r-plus `DirectoryText`).
    pub fn directory_text(&self, rel: &str) -> String {
        let mut shown = String::from("Maps/");
        if self.searching() {
            return shown;
        }
        if let Some(root) = &self.maps_root
            && rel.len() > root.len()
            && let Some(below) = rel.get(root.len()..)
        {
            let below = below.replace('\\', "/");
            let below = below.strip_prefix('/').unwrap_or(&below);
            shown.push_str(below);
            if !shown.ends_with('/') {
                shown.push('/');
            }
        }
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A tree of folders to maps, standing in for the game's list.
    struct Fake {
        rel: String,
        label: String,
        tree: BTreeMap<String, (Vec<&'static str>, Vec<(&'static str, u32)>)>,
        refreshes: usize,
        cache: Option<std::path::PathBuf>,
        bump: BTreeMap<String, u64>,
        rescans: usize,
    }

    impl Fake {
        fn new() -> Fake {
            let mut tree = BTreeMap::new();
            tree.insert(
                "Maps".to_owned(),
                (vec!["Duels", "Big"], vec![("Ridge", 64)]),
            );
            tree.insert(
                "Maps\\Duels".to_owned(),
                (vec![], vec![("Gold Rush", 32), ("ridge two", 64)]),
            );
            tree.insert("Maps\\Big".to_owned(), (vec!["Deep"], vec![("Arena", 128)]));
            tree.insert("Maps\\Big\\Deep".to_owned(), (vec![], vec![("Abyss", 128)]));
            Fake {
                rel: "Maps".into(),
                label: String::new(),
                tree,
                refreshes: 0,
                cache: None,
                bump: BTreeMap::new(),
                rescans: 0,
            }
        }
    }

    impl Source for Fake {
        fn rel_path(&self) -> String {
            self.rel.clone()
        }
        fn set_rel_path(&mut self, rel: &str) {
            self.rel = rel.to_owned();
            self.refreshes += 1;
        }
        fn set_folder_label(&mut self, label: &str) {
            self.label = label.to_owned();
        }
        fn live_entries(&self) -> Vec<LiveEntry> {
            self.tree.get(&self.rel).map_or(Vec::new(), |(_, maps)| {
                maps.iter()
                    .map(|&(name, dim)| LiveEntry {
                        name: name.into(),
                        path: format!("{}\\{name}.pud", self.rel),
                        desc: String::new(),
                        players: 4,
                        dim,
                    })
                    .collect()
            })
        }
        fn folder_names(&self) -> Vec<String> {
            self.tree.get(&self.rel).map_or(Vec::new(), |(f, _)| {
                f.iter().map(|s| (*s).to_owned()).collect()
            })
        }
        fn rescan_registry(&mut self) {
            self.rescans += 1;
        }
        fn cache_file(&self) -> Option<std::path::PathBuf> {
            self.cache.clone()
        }
        fn stat(&self, path: &str) -> Option<crate::mapsearchcache::Stamp> {
            let bump = self.bump.get(path).copied().unwrap_or(0);
            if self.tree.contains_key(path) {
                return Some((0, 10 + bump));
            }
            let (dir, file) = path.rsplit_once('\\')?;
            let stem = file.strip_suffix(".pud")?;
            let (_, maps) = self.tree.get(dir)?;
            maps.iter()
                .any(|(n, _)| *n == stem)
                .then_some((100, 7 + bump))
        }
    }

    fn browser_at_root(f: &Fake) -> Browser {
        let mut b = Browser::default();
        b.observe_root(&f.rel);
        b
    }

    #[test]
    fn the_size_filter_reads_the_dropdown_the_way_the_game_names_it() {
        let opts = vec![
            "mapsize_all".to_owned(),
            "mapsize_32".into(),
            "mapsize_64".into(),
        ];
        let all = SizeFilter::new(&opts, "mapsize_all");
        assert!(all.passes(32) && all.passes(128));
        let f64 = SizeFilter::new(&opts, "mapsize_64");
        assert!(f64.passes(64) && !f64.passes(32));
        assert!(
            SizeFilter::new(&[], "whatever").passes(7),
            "no options is all"
        );
        let bare = SizeFilter::new(&opts, "128");
        assert!(bare.passes(128) && !bare.passes(64));
    }

    #[test]
    fn small_text_helpers() {
        assert_eq!(last_component("Maps\\Big\\Deep"), "Deep");
        assert_eq!(last_component("Maps"), "Maps");
        assert!(contains_no_case("Gold Rush", "RUSH"));
        assert!(contains_no_case("anything", ""));
        assert!(!contains_no_case("Gold", "rush"));
        assert_eq!(sanitize("a\u{7}b\nc"), "abc");
        assert_eq!(sanitize(&"x".repeat(MAX_TEXT + 50)).len(), MAX_TEXT);
    }

    #[test]
    fn the_shortest_folder_seen_is_the_root_and_nothing_goes_above_it() {
        let mut f = Fake::new();
        let mut b = Browser::default();
        assert!(b.at_root("Maps\\Duels"), "no root known yet");
        b.observe_root("");
        assert_eq!(b.maps_root(), None, "empty is ignored");
        b.observe_root("Maps\\Duels");
        b.observe_root("Maps");
        b.observe_root("Maps\\Big");
        assert_eq!(b.maps_root(), Some("Maps"));
        b.descend(&mut f, "Big");
        assert_eq!((f.rel.as_str(), f.label.as_str()), ("Maps\\Big", "Big"));
        b.descend(&mut f, "Deep");
        assert_eq!(f.rel, "Maps\\Big\\Deep");
        b.go_up(&mut f);
        assert_eq!(f.rel, "Maps\\Big");
        b.go_up(&mut f);
        assert_eq!((f.rel.as_str(), f.label.as_str()), ("Maps", "Maps"));
        let before = f.refreshes;
        b.go_up(&mut f);
        assert_eq!(
            (f.rel.as_str(), f.refreshes),
            ("Maps", before),
            "at the root"
        );
    }

    #[test]
    fn directory_text_shows_the_path_below_the_root_or_just_maps_while_searching() {
        let f = Fake::new();
        let mut b = browser_at_root(&f);
        assert_eq!(b.directory_text("Maps"), "Maps/");
        assert_eq!(b.directory_text("Maps\\Big\\Deep"), "Maps/Big/Deep/");
        b.search_mut().push_str("ab");
        assert_eq!(b.directory_text("Maps\\Big"), "Maps/");
    }

    #[test]
    fn the_index_walks_every_folder_then_puts_the_game_back() {
        let mut f = Fake::new();
        let mut b = browser_at_root(&f);
        b.descend(&mut f, "Duels");
        b.build_index(&mut f);
        let names: Vec<&str> = b.indexed().iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Ridge", "Gold Rush", "ridge two", "Arena", "Abyss"]);
        assert_eq!(b.indexed()[1].rel_dir, "Maps\\Duels");
        assert_eq!(b.indexed()[4].rel_dir, "Maps\\Big\\Deep");
        assert_eq!(f.rel, "Maps\\Duels", "back where the user was");
        let walked = f.refreshes;
        b.build_index(&mut f);
        assert_eq!(f.refreshes, walked, "built once");
        b.reload(&mut f);
        assert_eq!(b.index_len(), 0);
        b.build_index(&mut f);
        assert_eq!(b.index_len(), 5);
    }

    fn temp_cache(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("w2r-mbcache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("map_search_index.txt")
    }

    #[test]
    fn a_second_browser_reads_the_index_from_the_cache_without_walking() {
        let file = temp_cache("hit");
        let mut f = Fake::new();
        f.cache = Some(file.clone());
        let mut b = browser_at_root(&f);
        b.build_index(&mut f);
        assert!(!b.index_from_cache(), "the first time it is walked");
        assert_eq!(b.index_len(), 5);
        assert!(file.exists(), "and then saved");

        let mut again = Fake::new();
        again.cache = Some(file.clone());
        let mut b2 = browser_at_root(&again);
        b2.build_index(&mut again);
        assert!(b2.index_from_cache());
        assert_eq!(b2.indexed(), b.indexed());
        assert_eq!(again.refreshes, 0, "the game's list was never touched");
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_changed_folder_or_map_sends_the_browser_back_to_walking() {
        let file = temp_cache("stale");
        let mut f = Fake::new();
        f.cache = Some(file.clone());
        browser_at_root(&f).build_index(&mut f);

        let mut touched = Fake::new();
        touched.cache = Some(file.clone());
        touched.bump.insert("Maps\\Duels".to_owned(), 5);
        let mut b = browser_at_root(&touched);
        b.build_index(&mut touched);
        assert!(!b.index_from_cache(), "a folder changed");
        assert_eq!(b.index_len(), 5);

        // That walk re-saved the cache against the changed disk, so now it is a hit again.
        let mut later = Fake::new();
        later.cache = Some(file.clone());
        later.bump.insert("Maps\\Duels".to_owned(), 5);
        let mut b = browser_at_root(&later);
        b.build_index(&mut later);
        assert!(b.index_from_cache());

        let mut edited = Fake::new();
        edited.cache = Some(file.clone());
        edited.bump.insert("Maps\\Duels".to_owned(), 5);
        edited.bump.insert("Maps\\Ridge.pud".to_owned(), 1);
        let mut b = browser_at_root(&edited);
        b.build_index(&mut edited);
        assert!(!b.index_from_cache(), "a map file changed");
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn reload_rescans_the_registry_and_skips_the_cache_for_the_next_index_only() {
        let file = temp_cache("reload");
        let mut f = Fake::new();
        f.cache = Some(file.clone());
        let mut b = browser_at_root(&f);
        b.build_index(&mut f);
        b.reload(&mut f);
        assert_eq!(f.rescans, 1, "the game was told to rescan");
        assert_eq!(b.index_len(), 0, "the index was dropped");
        b.build_index(&mut f);
        assert!(!b.index_from_cache(), "walked afresh after a reload");
        assert_eq!(b.index_len(), 5);
        b.reload(&mut f);
        b.build_index(&mut f);
        assert!(!b.index_from_cache());
        // With no reload in between, a new browser may use what the last walk saved.
        let mut g = Fake::new();
        g.cache = Some(file.clone());
        let mut c = browser_at_root(&g);
        c.build_index(&mut g);
        assert!(c.index_from_cache());
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_time_sliced_walk_gives_the_same_index_and_never_leaves_the_game_mid_walk() {
        let mut whole = Fake::new();
        let mut a = browser_at_root(&whole);
        a.build_index(&mut whole);

        let mut f = Fake::new();
        let mut b = browser_at_root(&f);
        b.descend(&mut f, "Duels");
        let mut calls = 0;
        // A zero budget stops after every folder: the worst case for slicing.
        while !b.step_index(&mut f, Some(std::time::Duration::ZERO)) {
            calls += 1;
            assert!(b.indexing());
            assert_eq!(
                f.rel, "Maps\\Duels",
                "the user's folder is back after each slice"
            );
            assert!(calls < 50, "it must make progress");
        }
        assert!(
            calls >= 3,
            "four folders cannot fit in one zero-budget slice"
        );
        assert!(!b.indexing());
        assert_eq!(b.indexed(), a.indexed(), "same maps in the same order");
        assert_eq!(f.rel, "Maps\\Duels");
        assert!(
            b.step_index(&mut f, Some(std::time::Duration::ZERO)),
            "ready stays ready"
        );
    }

    #[test]
    fn cancelling_or_reloading_a_walk_throws_the_partial_index_away() {
        let mut f = Fake::new();
        let mut b = browser_at_root(&f);
        assert!(!b.step_index(&mut f, Some(std::time::Duration::ZERO)));
        assert!(b.index_len() > 0 && b.indexing());
        b.cancel_walk();
        assert!(!b.indexing() && b.index_len() == 0 && !b.index_ready());
        assert!(!b.step_index(&mut f, Some(std::time::Duration::ZERO)));
        b.reload(&mut f);
        assert!(!b.indexing() && b.index_len() == 0);
        b.build_index(&mut f);
        assert_eq!(b.index_len(), 5, "a fresh full walk after both");
    }

    #[test]
    fn name_matching_ignores_ascii_case_without_allocating_and_handles_edges() {
        assert!(contains_no_case("Gold Rush", "gOLD"));
        assert!(contains_no_case("Gold Rush", "rush"));
        assert!(contains_no_case("Gold Rush", ""));
        assert!(contains_no_case("", ""));
        assert!(!contains_no_case("Gold", "Gold Rush"));
        assert!(!contains_no_case("Gold Rush", "rushes"));
        assert!(
            contains_no_case("Caf\u{e9} Map", "caf\u{e9}"),
            "non-ASCII must match byte for byte"
        );
    }

    #[test]
    fn with_no_cache_file_the_index_is_only_ever_walked() {
        let mut f = Fake::new();
        let mut b = browser_at_root(&f);
        b.build_index(&mut f);
        assert!(!b.index_from_cache());
        assert_eq!(b.index_len(), 5);
    }

    #[test]
    fn the_index_is_capped_in_depth_and_count() {
        let mut f = Fake::new();
        f.tree.clear();
        let mut path = String::from("Maps");
        for d in 0..=MAX_INDEX_DEPTH + 3 {
            f.tree.insert(path.clone(), (vec!["d"], vec![("m", 8)]));
            path.push_str("\\d");
            let _ = d;
        }
        let mut b = browser_at_root(&f);
        b.build_index(&mut f);
        assert_eq!(b.index_len(), MAX_INDEX_DEPTH + 1, "levels 0 to the cap");

        let mut g = Fake::new();
        g.tree.clear();
        g.tree.insert(
            "Maps".into(),
            (vec![], vec![("m", 8); MAX_INDEXED_MAPS + 10]),
        );
        let mut b = browser_at_root(&g);
        b.build_index(&mut g);
        assert_eq!(b.index_len(), MAX_INDEXED_MAPS);
    }

    #[test]
    fn a_choice_is_found_again_in_its_own_folder() {
        let mut f = Fake::new();
        let mut b = browser_at_root(&f);
        b.descend(&mut f, "Duels");
        let live = f.live_entries();
        b.choose_live(&live[1], &f.rel);
        b.set_folder(&mut f, "Maps");
        assert_eq!(f.rel, "Maps");
        assert_eq!(b.resolve_choice(&mut f), Some(1));
        assert_eq!(
            f.rel, "Maps\\Duels",
            "the list was pointed at the choice's folder"
        );
        b.search_mut().push_str("abyss");
        b.build_index(&mut f);
        let abyss = b
            .indexed()
            .iter()
            .find(|m| m.name == "Abyss")
            .unwrap()
            .clone();
        b.choose_indexed(&abyss);
        assert_eq!(b.resolve_choice(&mut f), Some(0));
        assert_eq!(f.rel, "Maps\\Big\\Deep");
        f.tree.get_mut("Maps\\Big\\Deep").unwrap().1.clear();
        assert_eq!(b.resolve_choice(&mut f), None, "gone");
    }

    #[test]
    fn with_players_filters_in_index_order() {
        let map = |name: &str, players| IndexedMap {
            name: name.into(),
            path: format!("Maps\\{name}.pud"),
            desc: String::new(),
            rel_dir: "Maps".into(),
            players,
            dim: 64,
        };
        let mut b = Browser::default();
        b.index = vec![map("a", 2), map("b", 4), map("c", 2), map("d", 8)];
        let names = |players| {
            b.with_players(players)
                .map(|m| m.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(0), ["a", "b", "c", "d"]);
        assert_eq!(names(2), ["a", "c"]);
        assert_eq!(names(8), ["d"]);
        assert!(names(3).is_empty());
    }

    #[test]
    fn opening_highlights_the_current_map_and_clears_the_search() {
        let mut b = Browser::default();
        b.search_mut().push_str("old");
        let current = Choice {
            name: "Ridge".into(),
            path: "p".into(),
            desc: String::new(),
            rel_dir: "Maps".into(),
            players: 2,
            dim: 64,
        };
        b.open(Some(current.clone()));
        assert!(b.is_open() && !b.searching());
        assert_eq!(b.choice(), Some(&current));
        b.open(Some(Choice {
            name: String::new(),
            ..current
        }));
        assert_eq!(b.choice(), None, "no selected map");
        b.close();
        assert!(!b.is_open());
    }
}
