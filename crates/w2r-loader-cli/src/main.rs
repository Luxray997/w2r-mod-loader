//! `w2r-loader`: the command-line interface of the W2R Mod Loader. All logic is in `w2r-loader-lib`; this file parses
//! arguments, asks the capability question and prints results (docs/manager.md).

use clap::{Args, Parser, Subcommand};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use w2r_formats::semver::Version;
use w2r_loader_lib::index::{Index, VersionEntry};
use w2r_loader_lib::session::{self, Session};
use w2r_loader_lib::store::{self, Root};
use w2r_loader_lib::{Error, catalog, install};

#[derive(Parser)]
#[command(
    name = "w2r-loader",
    version,
    about = "Plugin manager for Warcraft II Remastered"
)]
struct Cli {
    /// The w2r folder (default: %USERPROFILE%\Saved Games\Warcraft2Remastered\w2r).
    #[arg(long, env = "W2R_ROOT", global = true)]
    root: Option<PathBuf>,
    /// Where the index is published (`index.json`).
    #[arg(long, env = "W2R_LOADER_INDEX_URL", global = true)]
    index_url: Option<String>,
    /// Development: serve the index and packages from a folder instead of the network.
    #[arg(long, global = true, hide = true)]
    index_dir: Option<PathBuf>,
    /// The game's x86 folder (the one holding Warcraft II.exe), for `core`. Remembered once it works.
    #[arg(long, env = "W2R_GAME_DIR", global = true)]
    game_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
struct Approval {
    /// Accept the capability list without asking (not `hooks.raw`, see --allow-hooks-raw).
    #[arg(long, short)]
    yes: bool,
    /// With --yes, also accept the dangerous `hooks.raw` capability.
    #[arg(long)]
    allow_hooks_raw: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show installed plugins.
    List,
    /// Search the index by id, name or description.
    Search {
        term: Option<String>,
    },
    /// Fetch and check the index now.
    Refresh,
    /// Install a plugin: `id` for the latest version or `id@1.2.3`.
    Install {
        spec: String,
        #[command(flatten)]
        approval: Approval,
    },
    /// Update one plugin, or all of them.
    Update {
        id: Option<String>,
        #[command(flatten)]
        approval: Approval,
    },
    /// Remove a plugin (its data folder is kept unless --purge).
    Remove {
        id: String,
        #[arg(long)]
        purge: bool,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    /// Swap back to the version the last install replaced (run again to swap forward).
    Rollback {
        id: String,
    },
    /// Re-check installed files against the pins in state.json.
    Verify,
    /// The core mod (version.dll in the game folder): show, install or update, roll back.
    Core {
        #[command(subcommand)]
        action: CoreAction,
    },
    /// Your results so far, from the signed reports in the outbox folder (nothing is uploaded).
    Stats {
        /// How many of the latest matches to list.
        #[arg(long, default_value_t = 10)]
        recent: usize,
    },
    /// Recorded games: list them, or queue one to play back in the next custom game.
    Replays {
        /// Queue this replay (its file name, or any part of it that is unique) for the next game.
        #[arg(long)]
        play: Option<String>,
    },
    /// Share your match results with the stats service (off until you turn it on).
    Share {
        #[command(subcommand)]
        action: ShareAction,
    },
    /// Skill ratings worked out from the reports in the outbox (humans only, two teams).
    Ratings {
        /// Also rate computer players by name. For trying the maths on single-player games.
        #[arg(long)]
        with_computers: bool,
    },
}

#[derive(Subcommand)]
enum CoreAction {
    /// Show the installed core and whether a newer one is published.
    Status,
    /// Install the newest core, or update to it. The game must be closed.
    Update {
        /// Also replace another mod's version.dll. It is moved to version.dll.pre-w2rpm (never
        /// deleted) so you can put it back.
        #[arg(long)]
        replace_other: bool,
    },
    /// Swap back to the core the last update replaced (run again to swap forward).
    Rollback,
}

#[derive(Subcommand)]
enum ShareAction {
    /// Show whether sharing is on and where reports go.
    Status,
    /// Turn sharing on. Reports in the outbox are sent when you run `share upload`.
    On,
    /// Turn sharing off.
    Off,
    /// Set the stats service address (https://...).
    Url { url: String },
    /// Send the reports the service has not seen yet.
    Upload,
}

fn context(cli: &Cli) -> Result<Session, Error> {
    let root = session::resolve_root(cli.root.as_deref())?;
    Session::new(root, cli.index_url.clone(), cli.index_dir.clone())
}

fn index(ctx: &Session) -> Result<Index, Error> {
    ctx.index()
}

fn refresh(ctx: &Session) -> Result<Index, Error> {
    ctx.refresh()
}

use w2r_loader_lib::session::capability_note;

/// Shows what is being installed and asks. Returns the capabilities the user approved.
fn confirm(
    entry: &VersionEntry,
    pending: &[String],
    warnings: &[String],
    a: &Approval,
) -> Result<Vec<String>, Error> {
    let m = &entry.manifest;
    println!("{} {} by {}", m.name, entry.version, m.author);
    println!("  {}", m.description);
    println!("  Plugins are native code and run inside the game with your permissions.");
    if m.affects_simulation {
        println!(
            "  NOTE: this plugin changes the simulation; players need it to match in multiplayer."
        );
    }
    println!(
        "  Built for game builds: {}",
        m.game_builds
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for w in warnings {
        println!("  WARNING: {w}");
    }
    for c in &m.capabilities {
        let tag = if pending.contains(c) {
            "new"
        } else {
            "approved"
        };
        println!("  [{tag}] {c}: {}", capability_note(c));
    }
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    let raw = pending.iter().any(|c| c == "hooks.raw");
    if a.yes {
        if raw && !a.allow_hooks_raw {
            return Err(Error::new(
                "approval_needed",
                "hooks.raw needs --allow-hooks-raw as well as --yes",
            ));
        }
        return Ok(pending.to_vec());
    }
    print!("Approve {} and install? [y/N] ", pending.join(", "));
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    if matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(pending.to_vec())
    } else {
        Err(Error::new("declined", "nothing was installed"))
    }
}

fn install_entry(
    ctx: &Session,
    idx: &Index,
    entry: &VersionEntry,
    a: &Approval,
) -> Result<(), Error> {
    let pre = ctx.preflight(idx, entry)?;
    let approved = confirm(entry, &pre.pending, &pre.warnings, a)?;
    let out = ctx.install(entry, &approved)?;
    let id = &entry.manifest.id;
    match out {
        install::Outcome::Installed => println!("installed {id} {}", entry.version),
        install::Outcome::Updated { from } => println!("updated {id} {from} -> {}", entry.version),
        install::Outcome::Reinstalled => println!("reinstalled {id} {}", entry.version),
    }
    Ok(())
}

fn parse_spec(spec: &str) -> Result<(&str, Option<Version>), Error> {
    match spec.split_once('@') {
        None => Ok((spec, None)),
        Some((id, v)) => {
            let v = Version::parse(v)
                .ok_or_else(|| Error::new("bad_version", format!("{v:?} is not a version")))?;
            Ok((id, Some(v)))
        }
    }
}

fn core(cli: &Cli, action: &CoreAction) -> Result<(), Error> {
    use w2r_loader_lib::coremod::{self, Installed};
    let ctx = context(cli)?;
    let game = coremod::resolve_game_dir(&ctx.root, cli.game_dir.as_deref())?;
    let show = |i: &Installed| match i {
        Installed::Missing => "not installed".to_owned(),
        Installed::Foreign => "another mod's version.dll (not touched; `core update --replace-other` replaces it and keeps a backup)".to_owned(),
        Installed::Ours(Some(v)) => v.to_string(),
        Installed::Ours(None) => "installed (older build, version unknown)".to_owned(),
    };
    match action {
        CoreAction::Status => {
            println!("game folder: {}", game.display());
            let now = coremod::installed(&game)?;
            println!("installed:   {}", show(&now));
            match ctx.core_refresh().ok().or_else(|| coremod::load_cached(&ctx.root)) {
                Some(info) => {
                    println!("published:   {}", info.version);
                    if coremod::update_available(&now, &info, &ctx.manager) {
                        println!("an update is available: run `w2r-loader core update`");
                    }
                }
                None => println!("published:   unknown (could not read core.json)"),
            }
        }
        CoreAction::Update { replace_other } => {
            let now = coremod::installed(&game)?;
            let info = ctx.core_refresh()?;
            let replacing = *replace_other && now == Installed::Foreign;
            if !replacing && !coremod::update_available(&now, &info, &ctx.manager) {
                if matches!(now, Installed::Ours(_)) {
                    println!("the core is up to date ({})", show(&now));
                    return Ok(());
                }
                return Err(Error::new("not_ours", show(&now)));
            }
            println!("core {} (game builds: {})", info.version, info.game_builds.join(", "));
            println!("  runs inside the game with your permissions; replaces {}", show(&now));
            let out = ctx
                .core_install(&game, &info, *replace_other, &mut |_, _| {})
                .map_err(|e| if e.code == "io" { session::hint_io(e) } else { e })?;
            coremod::save_game_dir(&ctx.root, &game)?;
            if let Some(b) = &out.backed_up {
                println!("the other mod's version.dll was kept as {}", b.display());
            }
            println!("core is now {}", out.to.map_or("?".to_owned(), |v| v.to_string()));
        }
        CoreAction::Rollback => {
            let out = coremod::rollback(&ctx.root, &game)?;
            println!("core is now {}", out.to.map_or("unknown".to_owned(), |v| v.to_string()));
        }
    }
    Ok(())
}

fn run(cli: &Cli) -> Result<bool, Error> {
    match &cli.cmd {
        Cmd::Remove { id, purge } => {
            let root = context_root(cli)?;
            install::remove(&root, id, *purge).map_err(session::hint_io)?;
            println!("removed {id}");
        }
        Cmd::Enable { id } | Cmd::Disable { id } => {
            let on = matches!(cli.cmd, Cmd::Enable { .. });
            install::set_enabled(&context_root(cli)?, id, on)?;
            println!("{} {id}", if on { "enabled" } else { "disabled" });
        }
        Cmd::Rollback { id } => {
            let v = install::rollback(&context_root(cli)?, id).map_err(session::hint_io)?;
            println!("{id} is now {v}");
        }
        Cmd::Verify => {
            let findings = store::verify(&context_root(cli)?)?;
            for f in &findings {
                println!("{}: {}", f.id, f.problem);
            }
            if findings.is_empty() {
                println!("ok");
            }
            return Ok(findings.is_empty());
        }
        Cmd::Core { action } => core(cli, action)?,
        Cmd::Stats { recent } => return stats(cli, *recent),
        Cmd::Replays { play } => return replays(cli, play.as_deref()),
        Cmd::Share { action } => share(cli, action)?,
        Cmd::Ratings { with_computers } => return ratings(cli, *with_computers),
        Cmd::List => list(cli)?,
        Cmd::Refresh => {
            let ctx = context(cli)?;
            let idx = refresh(&ctx)?;
            println!("index serial {}: {} plugins", idx.serial, idx.plugins.len());
        }
        Cmd::Search { term } => {
            let ctx = context(cli)?;
            let idx = index(&ctx)?;
            for (p, v) in catalog::search(&idx, term.as_deref().unwrap_or("")) {
                println!("{} {} - {}", p.id, v.version, v.manifest.description);
            }
        }
        Cmd::Install { spec, approval } => {
            let ctx = context(cli)?;
            let (id, want) = parse_spec(spec)?;
            let idx = index(&ctx)?;
            let p = idx
                .plugin(id)
                .ok_or_else(|| Error::new("unknown_plugin", id.to_owned()))?;
            let entry = match &want {
                Some(v) => catalog::exact(p, v),
                None => catalog::latest(p, &ctx.manager),
            }
            .ok_or_else(|| Error::new("no_version", format!("no installable version of {id}")))?;
            install_entry(&ctx, &idx, entry, approval)?;
        }
        Cmd::Update { id, approval } => {
            let ctx = context(cli)?;
            let idx = index(&ctx)?;
            let state = ctx.root.load_state()?;
            let mut todo = catalog::updates(&state, &idx, &ctx.manager);
            if let Some(id) = id {
                if state.get(id).is_none() {
                    return Err(Error::new("not_installed", id.clone()));
                }
                todo.retain(|(s, _)| &s.id == id);
            }
            if todo.is_empty() {
                println!("everything is up to date");
            }
            for (_, entry) in todo {
                install_entry(&ctx, &idx, entry, approval)?;
            }
        }
    }
    Ok(true)
}

/// Commands that never touch the network need only the root.
fn context_root(cli: &Cli) -> Result<Root, Error> {
    session::resolve_root(cli.root.as_deref())
}

fn list(cli: &Cli) -> Result<(), Error> {
    let root = context_root(cli)?;
    let state = root.load_state()?;
    // Update markers come from the cached index only; `list` does not use the network.
    let updates = context(cli)
        .ok()
        .and_then(|c| {
            let idx = c.cached_index().ok()?;
            Some(
                c.updates(&idx)
                    .ok()?
                    .into_iter()
                    .map(|(id, v)| (id, v.version.to_string()))
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
    if state.plugins.is_empty() {
        println!("no plugins installed");
    }
    for p in &state.plugins {
        let on = if p.enabled { "enabled" } else { "disabled" };
        let caps = if p.approved_capabilities.is_empty() {
            "none".into()
        } else {
            p.approved_capabilities.join(", ")
        };
        let up = updates
            .iter()
            .find(|(id, _)| *id == p.id)
            .map(|(_, v)| format!("  (update available: {v})"))
            .unwrap_or_default();
        let by = root
            .installed_author(&p.id)
            .unwrap_or_else(|| "unknown".into());
        println!(
            "{} {} by {by} {on}  capabilities: {caps}{up}",
            p.id, p.version
        );
    }
    Ok(())
}

fn share(cli: &Cli, action: &ShareAction) -> Result<(), Error> {
    use w2r_loader_lib::fetch::Https;
    use w2r_loader_lib::share::{self, Settings};

    let root = context_root(cli)?;
    let mut settings = Settings::load(&root.0)?;
    match action {
        ShareAction::Status => {
            println!("sharing: {}", if settings.share_match_data { "on" } else { "off" });
            let service = settings
                .endpoint()
                .map_or_else(|_| "(not set)".to_string(), |e| e.replace("/v1/reports", ""));
            println!("service: {service}");
            println!("Sent: your signed match reports (map, players' BattleTags, teams, race, settings, result, turns)
      and your replay files, which include in-game chat.");
        }
        ShareAction::On => {
            settings.share_match_data = true;
            settings.save(&root.0)?;
            println!("sharing is on; run `w2r-loader share upload` to send reports");
        }
        ShareAction::Off => {
            settings.share_match_data = false;
            settings.save(&root.0)?;
            println!("sharing is off");
        }
        ShareAction::Url { url } => {
            settings.url = url.clone();
            settings.endpoint()?;
            settings.save(&root.0)?;
            println!("service set to {url}");
        }
        ShareAction::Upload => {
            if !settings.share_match_data {
                println!("sharing is off; turn it on with `w2r-loader share on`");
                return Ok(());
            }
            let s = share::upload(&root.0, &settings, &Https::default())?;
            println!(
                "{} sent, {} refused, {} skipped (not against humans, or a player without a BattleTag), {} waiting",
                s.sent, s.rejected, s.skipped, s.pending
            );
            let identity = w2r_stats::identity::load_or_create(
                &root.0.join("config").join("identity.json"),
                || None,
            );
            match identity {
                Ok(identity) => {
                    let r = w2r_replay::upload::upload(
                        &root.0,
                        &settings,
                        &identity,
                        &Https::default(),
                    )?;
                    println!(
                        "replays: {} sent, {} refused, {} skipped, {} waiting",
                        r.sent, r.rejected, r.skipped, r.waiting
                    );
                }
                Err(_) => println!("replays: no installation key yet (play a game first)"),
            }
        }
    }
    Ok(())
}

fn stats(cli: &Cli, recent: usize) -> Result<bool, Error> {
    use w2r_stats::history::{self, Match, Tally};
    use w2r_stats::report::Outcome;

    let root = context_root(cli)?;
    let dir = root.0.join("outbox");
    let loaded = history::load(&dir);
    for (path, why) in &loaded.rejected {
        println!("not counted: {} ({why})", path.display());
    }
    if loaded.matches.is_empty() {
        println!("no matches reported yet ({})", dir.display());
        return Ok(loaded.rejected.is_empty());
    }
    let line = |t: &Tally| {
        format!(
            "{:>3} games  {:>3} won  {:>3} lost  {:>3} left  {:>3} unfinished",
            t.games, t.wins, t.losses, t.left, t.unfinished
        )
    };
    let summary = history::summarize(&loaded.matches);
    println!("All       {}", line(&summary.total));
    println!(
        "
By opponent"
    );
    for (name, t) in &summary.by_opponent {
        println!("  {name:<32} {}", line(t));
    }
    println!(
        "
By map"
    );
    for (name, t) in &summary.by_map {
        println!("  {name:<32} {}", line(t));
    }
    println!(
        "
Latest"
    );
    for m in loaded.matches.iter().rev().take(recent) {
        let outcome = match m.result.as_ref().map(|r| &r.outcome) {
            Some(Outcome::Win) => "won".to_string(),
            Some(Outcome::Loss) => "lost".to_string(),
            Some(Outcome::Left) => "left".to_string(),
            Some(Outcome::Other(c)) => format!("code {c}"),
            None => "unfinished".to_string(),
        };
        let turns = m
            .result
            .as_ref()
            .map_or(String::new(), |r| format!("{} turns", r.turns));
        let against: Vec<String> = m.opponents().map(Match::label).collect();
        println!(
            "  {}  {:<10} {:<20} vs {}  {turns}",
            m.started,
            outcome,
            m.map,
            against.join(", ")
        );
    }
    Ok(loaded.rejected.is_empty())
}

fn replays(cli: &Cli, play: Option<&str>) -> Result<bool, Error> {
    use w2r_replay::Replay;

    let root = context_root(cli)?;
    let dir = root.0.join("replays");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".w2rr"))
        .collect();
    // The names start with the date, so this is oldest first.
    files.sort();
    if let Some(want) = play {
        let hits: Vec<&String> = files.iter().filter(|n| n.contains(want)).collect();
        let name = match hits.as_slice() {
            [one] => (*one).clone(),
            [] => return Err(Error::new("no_replay", format!("no replay matches {want:?}"))),
            many => {
                let list: Vec<&str> = many.iter().map(|n| n.as_str()).collect();
                return Err(Error::new(
                    "ambiguous_replay",
                    format!("{want:?} matches {} replays: {}", many.len(), list.join(", ")),
                ));
            }
        };
        let raw = std::fs::read(dir.join(&name))?;
        let replay = Replay::parse(&raw).map_err(|e| Error::new("bad_replay", e.to_string()))?;
        if replay.header.lobby.is_empty() {
            return Err(Error::new(
                "not_playable",
                "that replay was recorded before playback was supported (it has no start state)"
                    .to_owned(),
            ));
        }
        std::fs::write(root.0.join("replay-play.txt"), format!("{name}
"))?;
        println!("queued {name}");
        println!(
            "Start a custom game against the computer on {:?} (any races), with the same plugins as then; it plays the replay instead.",
            replay.header.map
        );
        return Ok(true);
    }
    if files.is_empty() {
        println!("no replays yet ({})", dir.display());
        return Ok(true);
    }
    for name in files.iter().rev() {
        let line = match std::fs::read(dir.join(name))
            .ok()
            .and_then(|raw| Replay::parse(&raw).ok())
        {
            Some(r) => {
                let local = usize::from(r.header.local).min(7);
                let outcome = match r.footer.map(|f| f.results[local]) {
                    Some(3) => "won",
                    Some(2) => "lost",
                    Some(_) => "left",
                    None => "cut off",
                };
                let players: Vec<String> = r.players().iter().map(|p| p.name.clone()).collect();
                format!(
                    "{outcome:<8} {:<16} {:>6} turns  {}{}",
                    r.header.map,
                    r.last_turn() + 1,
                    players.join(" v "),
                    if r.header.lobby.is_empty() { "  (not playable)" } else { "" }
                )
            }
            None => "unreadable".to_owned(),
        };
        println!("{name}  {line}");
    }
    Ok(true)
}

fn ratings(cli: &Cli, with_computers: bool) -> Result<bool, Error> {
    use w2r_stats::{history, rating};

    let root = context_root(cli)?;
    let loaded = history::load(&root.0.join("outbox"));
    let ladder = rating::ladder(&loaded.matches, with_computers);
    if ladder.players.is_empty() {
        println!(
            "no rated games yet (a rated game is a finished win or loss between two teams of humans)"
        );
    } else {
        let mut rows: Vec<_> = ladder.players.iter().collect();
        rows.sort_by(|a, b| {
            b.1.rating
                .conservative()
                .total_cmp(&a.1.rating.conservative())
        });
        println!(
            "{:<34} {:>6} {:>6} {:>7} {:>6}",
            "player", "skill", "mu", "sigma", "games"
        );
        for (name, e) in rows {
            println!(
                "{name:<34} {:>6.1} {:>6.1} {:>7.2} {:>6}",
                e.rating.conservative(),
                e.rating.mu,
                e.rating.sigma,
                e.games
            );
        }
    }
    for (started, why) in &ladder.skipped {
        println!("not rated: match at {started} ({why})");
    }
    Ok(true)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
