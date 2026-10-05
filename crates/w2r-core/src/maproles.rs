//! The map-sharing roles (docs/map-download.md, slice 3): what the host serves and to whom, and
//! what a player does about a lobby map it does not have. Ported from wc2r-plus
//! `map_download/host_role.cpp` and `client_role.cpp`. They drive the pure state machines
//! ([`DownloadHost`], [`DownloadClient`]) from what the game says (the lobby's map, the selected
//! file, who runs the mod) and carry out what comes back. The game and the network sit behind
//! [`Env`], so every decision here is tested without them.
//!
//! The download prompt and leaving the lobby when a map cannot be had are slice 4; here a player
//! who asked to be asked and cannot be shown the prompt declines after 1.5 s, and a lookup that
//! stays missing falls back to the game's own "map missing" handling.

use std::path::{Path, PathBuf};

use crate::downloadclient::{ClientState, DownloadClient};
use crate::downloadhost::DownloadHost;
use crate::mapindex::MapIndex;
use crate::maplimits::*;
use crate::mapstore::{self, Stored};
use crate::mapwire::{LeaveWhy, Msg, Reason, ReqId, SLOTS, SlotProgress, SlotState, decode};
use crate::pudvalidate;

/// How often the host re-checks the selected file on disk while the lobby map is unchanged.
pub const SERVE_STAT_INTERVAL_MS: u64 = 1_000;
/// A player waits this long for the hash index before treating a map as missing.
pub const INDEX_WAIT_MS: u64 = 6_000;
/// Without a screen to show the prompt, a player declines after this long.
pub const NO_PROMPT_DECLINE_MS: u64 = 1_500;
/// Between attempts at a map the host refused for now, and how many attempts.
pub const RETRY_MS: u64 = REQUEST_MIN_INTERVAL_MS + 500;
pub const MAX_RETRIES: i32 = 12;

/// What reading the selected map for serving found.
pub enum ServeRead {
    Ok(Vec<u8>),
    Unreadable,
    /// It resolves outside the game's Maps folders: never served.
    OutsideMaps(String),
}

/// Everything the roles need from the game and the network.
pub trait Env {
    /// The map name the lobby is showing (up to 64 bytes), empty when none.
    fn lobby_map_name(&self) -> String;
    /// The game path of the file the game has selected.
    fn selected_map_path(&self) -> Option<String>;
    /// Size and modification time of a game path.
    fn stat(&self, game_path: &str) -> Option<(u64, u64)>;
    fn read_servable(&self, game_path: &str) -> ServeRead;
    fn file_has_sha(&self, game_path: &str, sha: &[u8; 32]) -> bool;
    /// Makes the game select this file for the lobby.
    fn select_by_path(&mut self, game_path: &str) -> bool;
    /// Tells the game's map registry about a folder of new maps.
    fn register_directory(&mut self, dir: &Path);
    fn to_game_path(&self, path: &Path) -> Option<String>;
    fn peer_runs_mod(&self, slot: u32) -> bool;
    fn peer_shares_maps(&self, slot: u32) -> bool;
    fn countdown_running(&self) -> bool;
    fn send_to(&mut self, slot: u8, bytes: &[u8]);
    fn send_all_modded(&mut self, bytes: &[u8]);
    fn kick(&mut self, slot: u8);
    /// A game popup is open over the lobby (the prompt is not shown then).
    fn popup_open(&self) -> bool {
        false
    }
    /// The name of the player at a net slot, for messages.
    fn player_name(&self, _net_slot: u32) -> Option<String> {
        None
    }
    fn make_req_id(&mut self) -> Option<ReqId>;
    fn log(&mut self, msg: &str);
}

// ---- host ----------------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Default)]
struct ServeKey {
    name: String,
    path: String,
    size: u64,
    mtime: u64,
}

#[derive(Default)]
pub struct HostRole {
    host: DownloadHost,
    served_name: String,
    served_sha: [u8; 32],
    attempted_name: String,
    attempted_key: ServeKey,
    last_stat_ms: u64,
    peer_noted: [bool; SLOTS],
}

impl HostRole {
    pub fn new() -> HostRole {
        HostRole::default()
    }

    pub fn reset(&mut self) {
        self.host.reset();
        self.served_name.clear();
        self.attempted_name.clear();
        self.attempted_key = ServeKey::default();
        self.peer_noted = [false; SLOTS];
    }

    fn forget_served(&mut self) {
        self.host.clear_servable_map();
        self.served_name.clear();
    }

    /// Follows the lobby's map: serve it if the file is readable, inside the Maps folders and a
    /// valid map; stop serving it otherwise (wc2r-plus `UpdateServableMap`).
    fn update_servable_map(&mut self, env: &mut dyn Env, now: u64) {
        let name = env.lobby_map_name();
        if name.is_empty() {
            if !self.served_name.is_empty() {
                self.forget_served();
            }
            self.attempted_name.clear();
            return;
        }
        if name == self.attempted_name
            && now.saturating_sub(self.last_stat_ms) < SERVE_STAT_INTERVAL_MS
        {
            return;
        }
        self.last_stat_ms = now;
        let path = env.selected_map_path().unwrap_or_default();
        let (size, mtime) = env.stat(&path).unwrap_or((0, 0));
        let key = ServeKey {
            name: name.clone(),
            path: path.clone(),
            size,
            mtime,
        };
        if name == self.attempted_name && key == self.attempted_key {
            return;
        }
        let same_name = name == self.attempted_name;
        self.attempted_name = name.clone();
        self.attempted_key = key;

        let read = if path.is_empty() {
            ServeRead::Unreadable
        } else {
            env.read_servable(&path)
        };
        let bytes = match read {
            ServeRead::Ok(b) => b,
            ServeRead::OutsideMaps(final_path) => {
                env.log(&format!(
                    "selected map '{name}' resolves to '{final_path}', outside the game's Maps folders -- not serving"
                ));
                return self.forget_served();
            }
            ServeRead::Unreadable => {
                env.log(&format!(
                    "selected map '{name}' path '{path}' unreadable -- not serving"
                ));
                return self.forget_served();
            }
        };
        if let Some(v) = pudvalidate::validate(&bytes) {
            env.log(&format!(
                "selected map '{name}' failed validation ({}) -- not serving",
                v.fault.describe()
            ));
            return self.forget_served();
        }
        let sha = sha256(&bytes);
        if same_name && self.served_name == name && sha == self.served_sha {
            return;
        }
        if same_name && self.served_name == name {
            env.log(&format!(
                "'{name}' changed on disk -- re-validated, open transfers cancelled"
            ));
        }
        let len = bytes.len();
        if self.host.set_servable_map(name.as_bytes(), bytes) {
            self.served_sha = sha;
            self.served_name = name.clone();
            env.log(&format!("serving '{name}' ({len} bytes, validated)"));
        }
    }

    /// Players who announced the mod are known to the host machine; the others are not served.
    fn track_peers(&mut self, env: &dyn Env, now: u64) {
        for s in 1..SLOTS {
            let modded = env.peer_runs_mod(s as u32);
            if modded && !self.peer_noted[s] {
                self.peer_noted[s] = true;
                self.host
                    .note_peer_joined(s, now, env.peer_shares_maps(s as u32));
            } else if !modded && self.peer_noted[s] {
                self.peer_noted[s] = false;
                self.host.note_peer_left(s);
            }
        }
    }

    /// Once per lobby frame while hosting.
    pub fn tick(&mut self, env: &mut dyn Env, now: u64, serving: bool) {
        self.host.set_serving(serving);
        if serving {
            self.update_servable_map(env, now);
        } else if !self.attempted_name.is_empty() {
            self.forget_served();
            self.attempted_name.clear();
        }
        self.track_peers(env, now);
        self.host.set_countdown(env.countdown_running());
        self.host.tick(now);
        for out in self.host.take_outbox() {
            match out.to {
                Target::Slot(s) => env.send_to(s, &out.bytes),
                Target::AllModded => env.send_all_modded(&out.bytes),
            }
        }
        for slot in self.host.take_disconnects() {
            if slot != 0 && (slot as usize) < SLOTS {
                env.log(&format!(
                    "kicking net slot {slot} (stalled, declined or no answer)"
                ));
                env.kick(slot);
            }
        }
    }

    pub fn on_message(&mut self, now: u64, sender: usize, msg: &Msg) {
        self.host.on_client_message(now, sender, msg);
    }

    pub fn slot_progress(&self, slot: usize) -> SlotProgress {
        SlotProgress {
            state: self.host.slot_state(slot),
            percent: self.host.slot_percent(slot),
        }
    }

    pub fn any_pending(&self, now: u64) -> bool {
        self.host.any_pending(now)
    }

    pub fn pending_slot(&self, now: u64) -> Option<usize> {
        self.host.pending_slot(now)
    }
}

// ---- player --------------------------------------------------------------------------------

#[derive(Default)]
struct Run {
    map: String,
    started: bool,
    unsafe_name: bool,
    no_request_id: bool,
    decided: bool,
    mismatch: bool,
    selected_checked: bool,
    stored: bool,
    fail_logged: bool,
    /// Which run a prompt answer belongs to.
    generation: u32,
    retry_at: u64,
    retries: i32,
    failed_at: u64,
}

pub struct ClientRole {
    client: DownloadClient,
    run: Run,
    generation: u32,
    /// 0 none, 1 download, 2 leave: the user's answer to the prompt.
    answer: i32,
    prompt_seen_ms: u64,
    held_miss_name: String,
    held_since: u64,
    lookup_miss_name: String,
    progress: [SlotProgress; SLOTS],
    index: MapIndex,
    download_root: PathBuf,
}

impl ClientRole {
    pub fn new(index: MapIndex, download_root: PathBuf) -> ClientRole {
        ClientRole {
            client: DownloadClient::new(),
            run: Run::default(),
            generation: 0,
            answer: 0,
            prompt_seen_ms: 0,
            held_miss_name: String::new(),
            held_since: 0,
            lookup_miss_name: String::new(),
            progress: [SlotProgress::default(); SLOTS],
            index,
            download_root,
        }
    }

    pub fn set_download_root(&mut self, root: PathBuf) {
        self.download_root = root;
    }

    pub fn reset(&mut self) {
        self.client = DownloadClient::new();
        self.run = Run::default();
        self.answer = 0;
        self.progress = [SlotProgress::default(); SLOTS];
    }

    pub fn state(&self) -> ClientState {
        self.client.state()
    }

    fn will_retry(&self) -> bool {
        self.client.retryable() && self.run.retries < MAX_RETRIES
    }

    fn flush(&mut self, env: &mut dyn Env) {
        for bytes in self.client.take_outbox() {
            env.send_to(0, &bytes);
        }
    }

    fn select_and_clear_miss(&mut self, env: &mut dyn Env, game_path: &str) -> bool {
        if !env.select_by_path(game_path) {
            return false;
        }
        let lobby = env.lobby_map_name();
        if self.held_miss_name == lobby {
            self.held_miss_name.clear();
        }
        if self.lookup_miss_name == lobby {
            self.lookup_miss_name.clear();
        }
        self.run.mismatch = false;
        true
    }

    fn start_run(&mut self, env: &mut dyn Env, now: u64, map: &str) {
        if matches!(
            self.client.state(),
            ClientState::Offered | ClientState::Receiving | ClientState::Requested
        ) {
            self.client.abort(Reason::NotCurrentMap);
            self.flush(env);
        }
        let same_map = self.run.map == map;
        let retry_at = if same_map { self.run.retry_at } else { 0 };
        let retries = if same_map { self.run.retries + 1 } else { 0 };
        self.client = DownloadClient::new();
        self.generation += 1;
        self.run = Run {
            map: map.to_owned(),
            generation: self.generation,
            retry_at,
            retries,
            ..Run::default()
        };
        self.answer = 0;

        let Some(req) = env.make_req_id() else {
            env.log(&format!("no random request id -- not asking about '{map}'"));
            self.run.no_request_id = true;
            return;
        };
        if !self.client.start(now, req, map.as_bytes()) {
            self.run.unsafe_name = true;
            env.log(&format!(
                "'{map}' is not a name we can request -- stock lookup only"
            ));
            return;
        }
        self.run.started = true;
        env.log(&format!("asking the host about '{map}'"));
    }

    /// An offer is waiting for an answer: do we already have this map, or do we want it?
    fn decide_offer(&mut self, env: &mut dyn Env, now: u64, ask: bool) {
        let sha = *self.client.offered_sha();
        let lobby = env.lobby_map_name();
        if !self.run.mismatch {
            if !self.run.selected_checked {
                self.run.selected_checked = true;
                let selected = env.selected_map_path().unwrap_or_default();
                if self.lookup_miss_name != lobby
                    && !selected.is_empty()
                    && env.file_has_sha(&selected, &sha)
                {
                    self.client.on_already_have();
                    self.run.decided = true;
                    env.log(&format!("'{lobby}' already selected with the host's hash"));
                    return;
                }
            }
            if !self.index.ready()
                && now.saturating_sub(self.client.offered_at_ms()) < INDEX_WAIT_MS
            {
                return;
            }
            if let Some(local) = self.index.find_by_sha(&sha)
                && let Some(game_path) = env.to_game_path(&local)
                && env.file_has_sha(&game_path, &sha)
                && self.select_and_clear_miss(env, &game_path)
            {
                self.client.on_already_have();
                self.run.decided = true;
                env.log(&format!(
                    "'{lobby}' found locally by hash at '{game_path}' -- selected"
                ));
                return;
            }
            self.run.mismatch = true;
            env.log(&format!(
                "no local file has the host's '{lobby}' ({} bytes){}",
                self.client.offered_size(),
                if self.lookup_miss_name == lobby {
                    ""
                } else {
                    " -- the same-named local map is DIFFERENT"
                }
            ));
        }
        if !ask {
            self.client.on_consent(now, true);
            self.run.decided = true;
            env.log(&format!("downloading '{lobby}' (ask is off)"));
            return;
        }
        let prompt_shown =
            self.prompt_seen_ms != 0 && now.saturating_sub(self.prompt_seen_ms) < 1000;
        if self.answer != 0 {
            let yes = self.answer == 1;
            self.client.on_consent(now, yes);
            self.run.decided = true;
            env.log(&format!(
                "player chose {} for '{lobby}'",
                if yes { "DOWNLOAD" } else { "LEAVE" }
            ));
        } else if !prompt_shown
            && now.saturating_sub(self.client.offered_at_ms()) >= NO_PROMPT_DECLINE_MS
        {
            self.client.abort(Reason::Timeout);
            self.run.decided = true;
            env.log(&format!(
                "no screen can show the prompt -- not downloading '{lobby}'"
            ));
        }
    }

    /// The download finished and verified: write it, register it, select it.
    fn store_completed(&mut self, env: &mut dyn Env) {
        self.run.stored = true;
        let result = mapstore::store_map(
            &self.download_root,
            self.client.name(),
            self.client.bytes(),
            self.client.sha(),
        );
        match &result {
            Ok(stored) => {
                let (dir, full) = (stored.dir().to_path_buf(), stored.full().to_path_buf());
                let how = match stored {
                    Stored::Written { .. } => "written",
                    Stored::AlreadyPresent { .. } => "already there",
                };
                env.log(&format!(
                    "download of '{}' complete ({} bytes), {how} at '{}'",
                    String::from_utf8_lossy(self.client.name()),
                    self.client.bytes().len(),
                    full.display()
                ));
                self.index.add(full.clone(), *self.client.sha());
                env.register_directory(&dir);
                let selected = env
                    .to_game_path(&full)
                    .is_some_and(|p| self.select_and_clear_miss(env, &p));
                env.log(&format!(
                    "selected the downloaded map by path -- {}",
                    if selected { "ok" } else { "FAILED" }
                ));
                if selected {
                    return;
                }
            }
            Err(e) => env.log(&format!("could not store the downloaded map: {e:?}")),
        }
        self.client.on_store_failed();
        self.flush(env);
    }

    /// Once per lobby frame as a guest, when the host announced map sharing.
    pub fn tick(&mut self, env: &mut dyn Env, now: u64, ask: bool) {
        let lobby = env.lobby_map_name();
        if lobby.is_empty() || !env.peer_shares_maps(0) {
            return;
        }
        let new_map = self.run.map != lobby;
        let retry = !new_map && !self.run.started && self.will_retry() && now >= self.run.retry_at;
        if new_map || retry {
            self.start_run(env, now, &lobby);
        }
        if !self.run.started {
            return;
        }
        self.client.tick(now);
        if self.client.awaiting_consent() && !self.run.decided {
            self.decide_offer(env, now, ask);
        }
        self.flush(env);

        if self.client.state() == ClientState::Done && !self.run.stored {
            self.store_completed(env);
        } else if self.client.state() == ClientState::Failed && !self.run.fail_logged {
            self.run.fail_logged = true;
            self.run.failed_at = now;
            if self.will_retry() {
                self.run.retry_at = now + RETRY_MS;
                self.run.fail_logged = false;
                self.run.started = false;
            }
            env.log(&format!(
                "'{}' ended, reason {:?}",
                self.run.map,
                self.client.reason()
            ));
        }
    }

    /// A message from the host (net slot 0).
    pub fn on_host_message(&mut self, now: u64, sender: usize, msg: &Msg) {
        if let Msg::Progress { slots } = msg {
            if sender == 0 {
                self.progress = *slots;
            }
            return;
        }
        self.client.on_host_message(now, sender, msg);
    }

    pub fn slot_progress(&self, slot: usize) -> SlotProgress {
        self.progress.get(slot).copied().unwrap_or_default()
    }

    /// The game looked a map up by name (`SelectMapByRegistryName`). A miss is held back so the
    /// download can finish before the game's own "missing map" handling runs.
    pub fn note_lookup(&mut self, name: &str, found: bool, now: u64) {
        if !found {
            self.lookup_miss_name = name.to_owned();
            self.held_miss_name = name.to_owned();
            self.held_since = now;
            return;
        }
        if self.held_miss_name == name {
            self.held_miss_name.clear();
        }
        if self.lookup_miss_name == name {
            self.lookup_miss_name.clear();
        }
    }

    /// The map the lobby shows is missing here: the lookup failed, or the host's file differs
    /// from the same-named local one.
    pub fn map_missing(&self, lobby_map: &str) -> bool {
        !lobby_map.is_empty()
            && (self.lookup_miss_name == lobby_map
                || (self.run.map == lobby_map && self.run.mismatch))
    }

    /// Whether a held miss should now be handed to the game's own "map missing" handling: the
    /// download cannot or will not happen. `enabled` is the map-sharing setting.
    pub fn should_show_stock_missing(&mut self, env: &dyn Env, now: u64, enabled: bool) -> bool {
        const HOST_CAP_WAIT_MS: u64 = 5_000;
        const FAILED_DELAY_MS: u64 = 1_000;
        if self.held_miss_name.is_empty() {
            return false;
        }
        if self.held_miss_name != env.lobby_map_name() {
            self.held_miss_name.clear();
            return false;
        }
        let this_run = self.run.map == self.held_miss_name;
        let host_serves = env.peer_shares_maps(0);
        let give_up = if !enabled {
            !(!env.peer_runs_mod(0) && now.saturating_sub(self.held_since) < HOST_CAP_WAIT_MS)
        } else if !host_serves {
            env.peer_runs_mod(0) || now.saturating_sub(self.held_since) >= HOST_CAP_WAIT_MS
        } else {
            this_run
                && (self.run.no_request_id
                    || self.run.unsafe_name
                    || (self.client.state() == ClientState::Failed
                        && !self.will_retry()
                        && self.client.reason() != Reason::Declined
                        && self.run.failed_at != 0
                        && now.saturating_sub(self.run.failed_at) >= FAILED_DELAY_MS))
        };
        if give_up {
            self.held_miss_name.clear();
        }
        give_up
    }
}

// ---- the prompt ----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    None,
    /// Something is going on and nothing needs the user.
    Waiting,
    /// The host offered a map the player lacks: ask.
    Asking,
    Downloading,
}

/// What the lobby should show about the map download right now (wc2r-plus `DownloadPrompt`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub map_name: String,
    pub size_bytes: u32,
    pub percent: u8,
    pub ms_left: u64,
    /// The Download button works only after a moment, so a stray click cannot answer.
    pub clickable: bool,
    pub generation: u32,
}

impl Prompt {
    fn none() -> Prompt {
        Prompt {
            kind: PromptKind::None,
            map_name: String::new(),
            size_bytes: 0,
            percent: 0,
            ms_left: 0,
            clickable: false,
            generation: 0,
        }
    }
}

/// The Download button is greyed for this long after the offer arrives.
pub const PROMPT_CLICK_DELAY_MS: u64 = 1_000;

impl ClientRole {
    /// Called by whatever draws the prompt, every frame it is drawn: that is also how the role
    /// knows a screen is showing it (so it does not decline for lack of one).
    pub fn prompt(&mut self, env: &dyn Env, now: u64, enabled: bool, ask: bool) -> Prompt {
        self.prompt_seen_ms = now.max(1);
        let lobby = env.lobby_map_name();
        if !self.map_missing(&lobby) || env.popup_open() {
            return Prompt::none();
        }
        let mut p = Prompt {
            map_name: lobby.clone(),
            generation: self.run.generation,
            ..Prompt::none()
        };
        if !enabled {
            return p;
        }
        let this_run = self.run.map == lobby && self.run.started;
        if !this_run || self.client.state() == ClientState::Requested {
            p.kind = PromptKind::Waiting;
            return p;
        }
        match self.client.state() {
            ClientState::Offered => {
                p.size_bytes = self.client.offered_size();
                let shown = now.saturating_sub(self.client.offered_at_ms());
                p.ms_left = CONSENT_WAIT_MS.saturating_sub(shown);
                if self.run.decided || !ask || !self.run.mismatch {
                    p.kind = PromptKind::Waiting;
                } else {
                    p.kind = PromptKind::Asking;
                    p.clickable = shown >= PROMPT_CLICK_DELAY_MS;
                }
            }
            ClientState::Receiving => {
                p.kind = PromptKind::Downloading;
                p.size_bytes = self.client.offered_size();
                p.percent = self.client.percent();
            }
            ClientState::Done => {
                p.kind = PromptKind::Downloading;
                p.percent = 100;
            }
            // A decline is final and quiet; anything else is waiting for a retry or the stock
            // message.
            ClientState::Failed if self.client.reason() == Reason::Declined => {}
            _ => p.kind = PromptKind::Waiting,
        }
        p
    }

    /// The user's click: `download` true answers yes (after the click delay), false answers no
    /// and declines the offer. Only for the offer the prompt was drawn for.
    pub fn answer_prompt(&mut self, env: &mut dyn Env, generation: u32, download: bool, now: u64) {
        if generation != self.run.generation
            || self.run.decided
            || self.answer != 0
            || self.client.state() != ClientState::Offered
        {
            return;
        }
        if download {
            if now.saturating_sub(self.client.offered_at_ms()) >= PROMPT_CLICK_DELAY_MS {
                self.answer = 1;
            }
            return;
        }
        self.answer = 2;
        self.run.decided = true;
        self.client.on_consent(now, false);
        self.flush(env);
    }
}

// ---- what happened to other players' downloads ---------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadEventKind {
    Declined,
    Stalled,
    NoAnswer,
    LeftNoMap,
}

/// A download that ended badly for the player at `slot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadEvent {
    pub slot: usize,
    pub kind: DownloadEventKind,
    /// For `LeftNoMap`: why they left, when the host said.
    pub why: Option<LeaveWhy>,
}

fn leave_why(code: u8) -> Option<LeaveWhy> {
    match code {
        1 => Some(LeaveWhy::HostCannotSend),
        2 => Some(LeaveWhy::DownloadsOff),
        3 => Some(LeaveWhy::BadName),
        4 => Some(LeaveWhy::DownloadFailed),
        _ => None,
    }
}

/// The sentence for an event (wc2r-plus `CollectDownloadNotices`).
pub fn event_text(e: &DownloadEvent, who: &str) -> String {
    match e.kind {
        DownloadEventKind::Declined => format!("{who} declined map download and was disconnected"),
        DownloadEventKind::Stalled => format!("{who} disconnected while downloading map"),
        DownloadEventKind::NoAnswer => {
            format!("{who} did not respond to map download and was disconnected")
        }
        DownloadEventKind::LeftNoMap => match e.why {
            Some(LeaveWhy::HostCannotSend) => {
                format!("{who} does not have the map, and map sharing is disabled")
            }
            Some(LeaveWhy::DownloadsOff) => {
                format!("{who} does not have the map, and has map downloading disabled")
            }
            Some(LeaveWhy::BadName) => {
                format!("{who} could not download the map: its name cannot be requested")
            }
            Some(LeaveWhy::DownloadFailed) => {
                format!("{who} could not download the map: the download failed")
            }
            None => format!("{who} does not have the map"),
        },
    }
}

/// Watches the per-slot download states (the host's own, or what the host broadcast) and reports
/// the transitions that mean a player's download ended badly.
#[derive(Default)]
pub struct EventTracker {
    last: [SlotState; SLOTS],
}

impl EventTracker {
    pub fn reset(&mut self) {
        self.last = [SlotState::None; SLOTS];
    }

    #[allow(clippy::needless_range_loop)]
    pub fn observe(&mut self, progress: &[SlotProgress; SLOTS]) -> Vec<DownloadEvent> {
        let mut out = Vec::new();
        for slot in 1..SLOTS {
            let (was, now) = (self.last[slot], progress[slot].state);
            self.last[slot] = now;
            if now == was {
                continue;
            }
            if now == SlotState::LeftNoMap {
                out.push(DownloadEvent {
                    slot,
                    kind: DownloadEventKind::LeftNoMap,
                    why: leave_why(progress[slot].percent),
                });
                continue;
            }
            if !matches!(was, SlotState::Offered | SlotState::Uploading) {
                continue;
            }
            let kind = match now {
                SlotState::NoMap => DownloadEventKind::Declined,
                SlotState::NoAnswer => DownloadEventKind::NoAnswer,
                SlotState::Stalled => DownloadEventKind::Stalled,
                _ => continue,
            };
            out.push(DownloadEvent {
                slot,
                kind,
                why: None,
            });
        }
        out
    }
}

/// Decodes a message body from the wire and routes it: a host to its role, a guest to its.
pub fn route(
    is_host: bool,
    now: u64,
    sender: usize,
    body: &[u8],
    host: &mut HostRole,
    client: &mut ClientRole,
) {
    let Ok(msg) = decode(body) else {
        return;
    };
    if is_host {
        host.on_message(now, sender, &msg);
    } else if sender == 0 {
        client.on_host_message(now, sender, &msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapwire::encode;
    use crate::pudvalidate::tests::sample_map;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Default)]
    struct Fake {
        lobby: String,
        selected: Option<String>,
        files: HashMap<String, Vec<u8>>,
        outside: Vec<String>,
        modded: [bool; SLOTS],
        shares: [bool; SLOTS],
        countdown: bool,
        sent: Vec<(u8, Vec<u8>)>,
        all: Vec<Vec<u8>>,
        kicked: Vec<u8>,
        selects: Vec<String>,
        registered: Vec<PathBuf>,
        logs: Vec<String>,
        next_req: u8,
        mtime: u64,
        popup: bool,
    }

    impl Env for Fake {
        fn lobby_map_name(&self) -> String {
            self.lobby.clone()
        }
        fn selected_map_path(&self) -> Option<String> {
            self.selected.clone()
        }
        fn stat(&self, p: &str) -> Option<(u64, u64)> {
            self.files.get(p).map(|b| (b.len() as u64, self.mtime))
        }
        fn read_servable(&self, p: &str) -> ServeRead {
            if self.outside.iter().any(|o| o == p) {
                return ServeRead::OutsideMaps(format!("C:\\elsewhere\\{p}"));
            }
            match self.files.get(p) {
                Some(b) => ServeRead::Ok(b.clone()),
                None => ServeRead::Unreadable,
            }
        }
        fn file_has_sha(&self, p: &str, sha: &[u8; 32]) -> bool {
            self.files.get(p).is_some_and(|b| sha256(b) == *sha)
        }
        fn select_by_path(&mut self, p: &str) -> bool {
            self.selects.push(p.to_owned());
            true
        }
        fn register_directory(&mut self, d: &Path) {
            self.registered.push(d.to_path_buf());
        }
        fn to_game_path(&self, p: &Path) -> Option<String> {
            Some(p.to_string_lossy().into_owned())
        }
        fn peer_runs_mod(&self, s: u32) -> bool {
            self.modded[s as usize]
        }
        fn peer_shares_maps(&self, s: u32) -> bool {
            self.shares[s as usize]
        }
        fn countdown_running(&self) -> bool {
            self.countdown
        }
        fn send_to(&mut self, s: u8, b: &[u8]) {
            self.sent.push((s, b.to_vec()));
        }
        fn send_all_modded(&mut self, b: &[u8]) {
            self.all.push(b.to_vec());
        }
        fn kick(&mut self, s: u8) {
            self.kicked.push(s);
        }
        fn make_req_id(&mut self) -> Option<ReqId> {
            self.next_req += 1;
            (self.next_req != 255).then_some([self.next_req; 16])
        }
        fn log(&mut self, m: &str) {
            self.logs.push(m.to_owned());
        }
        fn popup_open(&self) -> bool {
            self.popup
        }
    }

    fn logged(env: &Fake, needle: &str) -> bool {
        env.logs.iter().any(|l| l.contains(needle))
    }

    fn temp(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().join(format!(
            "w2r-roles-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("Maps")).unwrap();
        base
    }

    // ---- host ----

    fn serving_env() -> Fake {
        let mut env = Fake::default();
        env.lobby = "ridge.pud".into();
        env.selected = Some("Maps\\ridge.pud".into());
        env.files.insert("Maps\\ridge.pud".into(), sample_map());
        env.modded[1] = true;
        env.shares[1] = true;
        env
    }

    #[test]
    fn the_host_serves_the_lobbys_map_once_it_validates_and_offers_it_to_a_modded_player() {
        let mut env = serving_env();
        let mut host = HostRole::new();
        host.tick(&mut env, 0, true);
        assert!(logged(&env, "serving 'ridge.pud'"));
        let request = encode(&Msg::Request {
            req: [4; 16],
            name: b"ridge.pud",
        })
        .unwrap();
        host.on_message(100, 1, &decode(&request[5..]).unwrap());
        host.tick(&mut env, 200, true);
        let offered = env
            .sent
            .iter()
            .filter(|(s, b)| *s == 1 && matches!(decode(&b[5..]), Ok(Msg::Offer { .. })))
            .count();
        assert_eq!(offered, 1);
        assert!(!env.all.is_empty(), "progress went to everyone");
        assert!(host.any_pending(200));
    }

    #[test]
    fn nothing_is_served_that_is_unreadable_outside_maps_or_invalid() {
        for case in 0..3 {
            let mut env = serving_env();
            match case {
                0 => {
                    env.files.clear();
                }
                1 => env.outside.push("Maps\\ridge.pud".into()),
                _ => {
                    env.files.insert("Maps\\ridge.pud".into(), vec![0x41; 3000]);
                }
            }
            let mut host = HostRole::new();
            host.tick(&mut env, 0, true);
            assert!(!logged(&env, "serving '"), "case {case}: {:?}", env.logs);
            let request = encode(&Msg::Request {
                req: [4; 16],
                name: b"ridge.pud",
            })
            .unwrap();
            host.on_message(100, 1, &decode(&request[5..]).unwrap());
            host.tick(&mut env, 200, true);
            let denied = env.sent.iter().any(|(_, b)| {
                matches!(
                    decode(&b[5..]),
                    Ok(Msg::Deny {
                        reason: Reason::MapUnservable,
                        ..
                    })
                )
            });
            assert!(denied, "case {case}");
        }
    }

    #[test]
    fn a_changed_file_is_revalidated_and_a_new_lobby_map_replaces_the_old() {
        let mut env = serving_env();
        let mut host = HostRole::new();
        host.tick(&mut env, 0, true);
        env.logs.clear();
        host.tick(&mut env, 500, true);
        assert!(env.logs.is_empty(), "nothing changed, nothing logged");
        // The file changes on disk (size and time).
        let mut other = sample_map();
        other.push(0);
        env.mtime = 99;
        env.files.insert("Maps\\ridge.pud".into(), other);
        host.tick(&mut env, 1500, true);
        // Trailing garbage makes it an invalid map, which stops serving.
        assert!(logged(&env, "failed validation"));
        env.files.insert("Maps\\ridge.pud".into(), sample_map());
        env.mtime = 100;
        host.tick(&mut env, 3000, true);
        assert!(logged(&env, "serving 'ridge.pud'"));
        // The lobby map changes.
        env.lobby = "next.pud".into();
        env.selected = Some("Maps\\next.pud".into());
        env.files.insert("Maps\\next.pud".into(), sample_map());
        host.tick(&mut env, 4500, true);
        assert!(logged(&env, "serving 'next.pud'"));
        // Serving turned off, or no lobby map: stops.
        host.tick(&mut env, 6000, false);
        let request = encode(&Msg::Request {
            req: [4; 16],
            name: b"next.pud",
        })
        .unwrap();
        host.on_message(6100, 1, &decode(&request[5..]).unwrap());
        host.tick(&mut env, 6200, false);
        assert!(env.sent.iter().any(|(_, b)| matches!(
            decode(&b[5..]),
            Ok(Msg::Deny {
                reason: Reason::ServingOff,
                ..
            })
        )));
    }

    #[test]
    fn a_decline_kicks_through_the_game_and_the_countdown_is_passed_on() {
        let mut env = serving_env();
        let mut host = HostRole::new();
        host.tick(&mut env, 0, true);
        let req = [4; 16];
        let request = encode(&Msg::Request {
            req,
            name: b"ridge.pud",
        })
        .unwrap();
        host.on_message(100, 1, &decode(&request[5..]).unwrap());
        let cancel = encode(&Msg::Cancel {
            req,
            reason: Reason::Declined,
        })
        .unwrap();
        host.on_message(200, 1, &decode(&cancel[5..]).unwrap());
        host.tick(&mut env, 300, true);
        assert_eq!(env.kicked, [1]);
        env.countdown = true;
        host.tick(&mut env, 9_900, true); // the countdown is read once per frame
        let request = encode(&Msg::Request {
            req: [5; 16],
            name: b"ridge.pud",
        })
        .unwrap();
        host.on_message(10_000, 1, &decode(&request[5..]).unwrap());
        host.tick(&mut env, 10_100, true);
        assert!(env.sent.iter().any(|(_, b)| matches!(
            decode(&b[5..]),
            Ok(Msg::Deny {
                reason: Reason::Countdown,
                ..
            })
        )));
    }

    #[test]
    fn players_without_the_mod_are_never_served_and_leaving_forgets_them() {
        let mut env = serving_env();
        env.modded[1] = false;
        let mut host = HostRole::new();
        host.tick(&mut env, 0, true);
        let request = encode(&Msg::Request {
            req: [4; 16],
            name: b"ridge.pud",
        })
        .unwrap();
        host.on_message(100, 1, &decode(&request[5..]).unwrap());
        host.tick(&mut env, 200, true);
        assert!(env.sent.is_empty(), "an unknown slot is not answered");
        env.modded[1] = true;
        host.tick(&mut env, 300, true);
        env.modded[1] = false;
        host.tick(&mut env, 400, true);
        assert_eq!(host.slot_progress(1).percent, 0);
    }

    // ---- player ----

    fn guest_env(host_map: &[u8]) -> Fake {
        let mut env = Fake::default();
        env.lobby = "ridge.pud".into();
        env.shares[0] = true;
        env.modded[0] = true;
        let _ = host_map;
        env
    }

    fn deliver_host_replies(
        host: &mut HostRole,
        hostenv: &mut Fake,
        guest: &mut ClientRole,
        guestenv: &mut Fake,
        now: u64,
    ) {
        for (slot, bytes) in guestenv.sent.drain(..) {
            assert_eq!(slot, 0);
            route(true, now, 1, &bytes[5..], host, guest);
        }
        host.tick(hostenv, now, true);
        for (slot, bytes) in hostenv.sent.drain(..) {
            assert_eq!(slot, 1);
            route(false, now, 0, &bytes[5..], host, guest);
        }
        for bytes in hostenv.all.drain(..) {
            route(false, now, 0, &bytes[5..], host, guest);
        }
    }

    #[test]
    fn a_guest_without_the_map_downloads_stores_and_selects_it() {
        let map = sample_map();
        let base = temp("dl");
        let root = base.join("Maps").join("Download");
        let index = MapIndex::new();
        index.start_build(base.join("Maps"), None);
        let mut guest = ClientRole::new(index, root.clone());
        let mut genv = guest_env(&map);
        let mut host = HostRole::new();
        let mut henv = serving_env();
        let mut t = 0;
        while t < 20_000
            && !(logged(&genv, "selected the downloaded")
                || genv.logs.iter().any(|l| l.contains("FAILED")))
        {
            guest.tick(&mut genv, t, false);
            deliver_host_replies(&mut host, &mut henv, &mut guest, &mut genv, t);
            t += 50;
        }
        assert!(
            logged(&genv, "downloading 'ridge.pud' (ask is off)"),
            "{:?}",
            genv.logs
        );
        assert!(
            logged(&genv, "selected the downloaded map by path -- ok"),
            "{:?}",
            genv.logs
        );
        let stored = root.join("V1").join("ridge.pud");
        assert_eq!(std::fs::read(&stored).unwrap(), map);
        assert_eq!(genv.selects.len(), 1);
        assert_eq!(genv.registered, [root.join("V1")]);
        assert_eq!(guest.state(), ClientState::Done);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_guest_that_already_has_the_map_by_content_says_so_and_selects_it() {
        let map = sample_map();
        let base = temp("have");
        std::fs::write(base.join("Maps").join("other-name.pud"), &map).unwrap();
        let index = MapIndex::new();
        index.start_build(base.join("Maps"), None);
        let start = std::time::Instant::now();
        while !index.ready() && start.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let local = base.join("Maps").join("other-name.pud");
        let mut guest = ClientRole::new(index, base.join("Maps").join("Download"));
        let mut genv = guest_env(&map);
        genv.files
            .insert(local.to_string_lossy().into_owned(), map.clone());
        let mut host = HostRole::new();
        let mut henv = serving_env();
        for t in (0..6000).step_by(50) {
            guest.tick(&mut genv, t, true);
            deliver_host_replies(&mut host, &mut henv, &mut guest, &mut genv, t);
        }
        assert!(logged(&genv, "found locally by hash"), "{:?}", genv.logs);
        assert_eq!(genv.selects, [local.to_string_lossy().into_owned()]);
        assert_eq!(guest.state(), ClientState::Present);
        assert!(
            !base.join("Maps").join("Download").exists(),
            "nothing was downloaded"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn asked_to_ask_and_with_no_prompt_to_show_the_guest_declines_after_a_moment() {
        let map = sample_map();
        let base = temp("ask");
        let index = MapIndex::new();
        index.start_build(base.join("Maps"), None);
        let mut guest = ClientRole::new(index, base.join("Maps").join("Download"));
        let mut genv = guest_env(&map);
        let mut host = HostRole::new();
        let mut henv = serving_env();
        for t in (0..6000).step_by(50) {
            guest.tick(&mut genv, t, true);
            deliver_host_replies(&mut host, &mut henv, &mut guest, &mut genv, t);
        }
        assert!(
            logged(&genv, "no screen can show the prompt"),
            "{:?}",
            genv.logs
        );
        assert_eq!(guest.state(), ClientState::Failed);
        assert!(!base.join("Maps").join("Download").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_busy_host_is_asked_again_up_to_the_retry_limit() {
        let base = temp("retry");
        let mut guest = ClientRole::new(MapIndex::new(), base.join("Maps").join("Download"));
        let mut genv = guest_env(&[]);
        guest.tick(&mut genv, 0, false);
        let first = genv.sent.len();
        assert_eq!(first, 1, "the first request");
        // The host says busy.
        let deny = encode(&Msg::Deny {
            req: [1; 16],
            reason: Reason::Busy,
        })
        .unwrap();
        guest.on_host_message(10, 0, &decode(&deny[5..]).unwrap());
        guest.tick(&mut genv, 20, false);
        assert_eq!(guest.state(), ClientState::Failed);
        guest.tick(&mut genv, 20 + RETRY_MS - 1, false);
        assert_eq!(genv.sent.len(), first, "not yet");
        guest.tick(&mut genv, 20 + RETRY_MS, false);
        assert_eq!(genv.sent.len(), first + 1, "asked again");
        assert!(logged(&genv, "asking the host"));
        // A refusal that waiting cannot fix is not retried.
        let deny = encode(&Msg::Deny {
            req: [2; 16],
            reason: Reason::ServingOff,
        })
        .unwrap();
        guest.on_host_message(6000, 0, &decode(&deny[5..]).unwrap());
        guest.tick(&mut genv, 6001, false);
        let n = genv.sent.len();
        guest.tick(&mut genv, 100_000, false);
        assert_eq!(genv.sent.len(), n);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_host_that_does_not_share_is_not_asked() {
        let base = temp("noshare");
        let mut guest = ClientRole::new(MapIndex::new(), base.join("d"));
        let mut genv = guest_env(&[]);
        genv.shares[0] = false;
        guest.tick(&mut genv, 0, false);
        assert!(genv.sent.is_empty());
        genv.shares[0] = true;
        genv.lobby.clear();
        guest.tick(&mut genv, 10, false);
        assert!(genv.sent.is_empty(), "no lobby map yet");
        genv.lobby = "x\\y.pud".into();
        guest.tick(&mut genv, 20, false);
        assert!(
            logged(&genv, "not a name we can request"),
            "{:?}",
            genv.logs
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_lookup_is_held_then_released_when_the_download_cannot_happen() {
        let base = temp("held");
        let mut guest = ClientRole::new(MapIndex::new(), base.join("d"));
        let mut genv = guest_env(&[]);
        guest.note_lookup("ridge.pud", false, 0);
        assert!(guest.map_missing("ridge.pud"));
        assert!(!guest.map_missing("other.pud"));
        // The host shares and the run has not failed: keep holding.
        guest.tick(&mut genv, 0, false);
        assert!(!guest.should_show_stock_missing(&genv, 100, true));
        // The host stops sharing: the stock handling takes over.
        genv.shares[0] = false;
        assert!(guest.should_show_stock_missing(&genv, 200, true));
        assert!(
            !guest.should_show_stock_missing(&genv, 300, true),
            "released once"
        );
        // Map sharing off locally: released at once for a host that runs the mod.
        guest.note_lookup("ridge.pud", false, 400);
        assert!(guest.should_show_stock_missing(&genv, 400, false));
        // The lookup later succeeds: nothing is held.
        guest.note_lookup("ridge.pud", false, 500);
        guest.note_lookup("ridge.pud", true, 600);
        assert!(!guest.should_show_stock_missing(&genv, 700, false));
        assert!(!guest.map_missing("ridge.pud"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_new_lobby_forgets_the_progress_and_the_run() {
        let base = temp("reset");
        let mut guest = ClientRole::new(MapIndex::new(), base.join("d"));
        let mut genv = guest_env(&[]);
        guest.tick(&mut genv, 0, false);
        let mut slots = [SlotProgress::default(); SLOTS];
        slots[3].percent = 40;
        guest.on_host_message(5, 0, &Msg::Progress { slots });
        assert_eq!(guest.slot_progress(3).percent, 40);
        guest.on_host_message(
            5,
            2,
            &Msg::Progress {
                slots: [SlotProgress::default(); SLOTS],
            },
        );
        assert_eq!(
            guest.slot_progress(3).percent,
            40,
            "only the host reports progress"
        );
        guest.reset();
        assert_eq!(guest.slot_progress(3).percent, 0);
        assert_eq!(guest.state(), ClientState::Idle);
        let _ = std::fs::remove_dir_all(&base);
    }

    // ---- prompt and events ----

    fn offered_guest(base: &Path) -> (ClientRole, Fake, HostRole, Fake) {
        let map = sample_map();
        let index = MapIndex::new();
        index.start_build(base.join("Maps"), None);
        let guest = ClientRole::new(index, base.join("Maps").join("Download"));
        (guest, guest_env(&map), HostRole::new(), serving_env())
    }

    #[test]
    fn the_prompt_asks_after_an_offer_for_a_map_that_is_missing_here() {
        let base = temp("prompt");
        let (mut guest, mut genv, mut host, mut henv) = offered_guest(&base);
        genv.popup = false;
        // No lookup miss yet: nothing to show.
        guest.tick(&mut genv, 0, true);
        assert_eq!(guest.prompt(&genv, 10, true, true).kind, PromptKind::None);
        guest.note_lookup("ridge.pud", false, 20);
        assert_eq!(
            guest.prompt(&genv, 30, true, true).kind,
            PromptKind::Waiting,
            "requested"
        );
        assert_eq!(
            guest.prompt(&genv, 30, false, true).kind,
            PromptKind::None,
            "sharing off, nothing"
        );
        // Run until the offer arrives and is waiting on the user.
        let mut t = 0;
        let mut asked = None;
        while t < 8000 && asked.is_none() {
            guest.tick(&mut genv, t, true);
            deliver_host_replies(&mut host, &mut henv, &mut guest, &mut genv, t);
            let p = guest.prompt(&genv, t, true, true);
            if p.kind == PromptKind::Asking {
                asked = Some((t, p));
            }
            t += 50;
        }
        let (t0, p) = asked.expect("an Asking prompt appeared");
        assert_eq!(p.map_name, "ridge.pud");
        assert_eq!(p.size_bytes as usize, sample_map().len());
        assert!(!p.clickable, "greyed for the first second");
        assert!(p.ms_left > 0 && p.ms_left <= CONSENT_WAIT_MS);
        // A click too early does nothing; after the delay it answers.
        guest.answer_prompt(&mut genv, p.generation, true, t0);
        guest.tick(&mut genv, t0, true);
        assert_eq!(guest.state(), ClientState::Offered);
        let later = t0 + PROMPT_CLICK_DELAY_MS;
        let p = guest.prompt(&genv, later, true, true);
        assert!(p.clickable);
        guest.answer_prompt(&mut genv, p.generation, true, later);
        guest.tick(&mut genv, later, true);
        assert_eq!(guest.state(), ClientState::Receiving, "the download began");
        assert_eq!(
            guest.prompt(&genv, later + 10, true, true).kind,
            PromptKind::Downloading
        );
        assert!(logged(&genv, "player chose DOWNLOAD"), "{:?}", genv.logs);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn declining_tells_the_host_and_a_stale_or_repeated_answer_is_ignored() {
        let base = temp("decline");
        let (mut guest, mut genv, mut host, mut henv) = offered_guest(&base);
        guest.note_lookup("ridge.pud", false, 0);
        let mut t = 0;
        let mut answer_gen = None;
        while t < 8000 && answer_gen.is_none() {
            guest.tick(&mut genv, t, true);
            deliver_host_replies(&mut host, &mut henv, &mut guest, &mut genv, t);
            let p = guest.prompt(&genv, t, true, true);
            if p.kind == PromptKind::Asking {
                answer_gen = Some(p.generation);
            }
            t += 50;
        }
        let answer_gen = answer_gen.expect("asked");
        guest.answer_prompt(&mut genv, answer_gen + 1, false, t);
        assert_eq!(guest.state(), ClientState::Offered, "another run's prompt");
        guest.answer_prompt(&mut genv, answer_gen, false, t);
        assert_eq!(
            (guest.state(), guest.prompt(&genv, t, true, true).kind),
            (ClientState::Failed, PromptKind::None)
        );
        genv.sent.clear();
        guest.answer_prompt(&mut genv, answer_gen, true, t + 5000);
        assert!(genv.sent.is_empty(), "answered once");
        // The host removed the declined player.
        for (_, bytes) in guest_outgoing(&mut genv) {
            route(true, t, 1, &bytes[5..], &mut host, &mut guest);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    fn guest_outgoing(env: &mut Fake) -> Vec<(u8, Vec<u8>)> {
        std::mem::take(&mut env.sent)
    }

    #[test]
    fn a_popup_over_the_lobby_hides_the_prompt() {
        let base = temp("popup");
        let mut guest = ClientRole::new(MapIndex::new(), base.join("d"));
        let mut genv = guest_env(&[]);
        guest.note_lookup("ridge.pud", false, 0);
        guest.tick(&mut genv, 0, true);
        genv.popup = true;
        assert_eq!(guest.prompt(&genv, 10, true, true).kind, PromptKind::None);
        genv.popup = false;
        assert_eq!(
            guest.prompt(&genv, 20, true, true).kind,
            PromptKind::Waiting
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn download_events_name_what_happened_to_whom() {
        let mut tracker = EventTracker::default();
        let mut p = [SlotProgress::default(); SLOTS];
        p[2].state = SlotState::Offered;
        p[3].state = SlotState::Uploading;
        p[4].state = SlotState::Offered;
        p[5].state = SlotState::Uploading;
        assert!(tracker.observe(&p).is_empty(), "starting is not an ending");
        p[2].state = SlotState::NoMap;
        p[3].state = SlotState::Stalled;
        p[4].state = SlotState::NoAnswer;
        p[5].state = SlotState::Done;
        p[6].state = SlotState::LeftNoMap;
        p[6].percent = 3;
        let events = tracker.observe(&p);
        let kinds: Vec<_> = events.iter().map(|e| (e.slot, e.kind)).collect();
        assert_eq!(
            kinds,
            [
                (2, DownloadEventKind::Declined),
                (3, DownloadEventKind::Stalled),
                (4, DownloadEventKind::NoAnswer),
                (6, DownloadEventKind::LeftNoMap),
            ]
        );
        assert_eq!(events[3].why, Some(LeaveWhy::BadName));
        assert!(
            tracker.observe(&p).is_empty(),
            "each ending is reported once"
        );
        // A state that was never open cannot be "declined".
        let mut q = [SlotProgress::default(); SLOTS];
        q[1].state = SlotState::NoMap;
        assert!(EventTracker::default().observe(&q).is_empty());
        // Slot 0 is the host and never reported.
        let mut r = [SlotProgress::default(); SLOTS];
        r[0].state = SlotState::LeftNoMap;
        assert!(EventTracker::default().observe(&r).is_empty());
        tracker.reset();
        assert_eq!(
            tracker.observe(&p).len(),
            1,
            "after a reset the leave is new again"
        );
    }

    #[test]
    fn event_sentences() {
        let e = |kind, why| DownloadEvent { slot: 2, kind, why };
        assert_eq!(
            event_text(&e(DownloadEventKind::Declined, None), "Ann"),
            "Ann declined map download and was disconnected"
        );
        assert_eq!(
            event_text(&e(DownloadEventKind::Stalled, None), "Ann"),
            "Ann disconnected while downloading map"
        );
        assert_eq!(
            event_text(&e(DownloadEventKind::NoAnswer, None), "Ann"),
            "Ann did not respond to map download and was disconnected"
        );
        for (why, text) in [
            (
                Some(LeaveWhy::HostCannotSend),
                "Ann does not have the map, and map sharing is disabled",
            ),
            (
                Some(LeaveWhy::DownloadsOff),
                "Ann does not have the map, and has map downloading disabled",
            ),
            (
                Some(LeaveWhy::BadName),
                "Ann could not download the map: its name cannot be requested",
            ),
            (
                Some(LeaveWhy::DownloadFailed),
                "Ann could not download the map: the download failed",
            ),
            (None, "Ann does not have the map"),
        ] {
            assert_eq!(
                event_text(&e(DownloadEventKind::LeftNoMap, why), "Ann"),
                text
            );
        }
    }
}
