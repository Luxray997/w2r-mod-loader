//! `w2rpm`: the command-line plugin manager. All logic is in `w2rpm-lib`; this file parses
//! arguments, asks the capability question and prints results (docs/manager.md).

use clap::{Args, Parser, Subcommand};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use w2r_formats::semver::Version;
use w2rpm_lib::fetch::{self, DirFetcher, Fetcher, Https, Source};
use w2rpm_lib::index::{Index, VersionEntry};
use w2rpm_lib::minisign::PublicKey;
use w2rpm_lib::store::{self, Root};
use w2rpm_lib::{Error, catalog, check, install};

#[derive(Parser)]
#[command(
    name = "w2rpm",
    version,
    about = "Plugin manager for Warcraft II Remastered"
)]
struct Cli {
    /// The w2r folder (default: %USERPROFILE%\Saved Games\Warcraft2Remastered\w2r).
    #[arg(long, env = "W2R_ROOT", global = true)]
    root: Option<PathBuf>,
    /// Where the signed index is published (`index.json`; the signature is `index.json.sig`).
    #[arg(long, env = "W2RPM_INDEX_URL", global = true)]
    index_url: Option<String>,
    /// A minisign public key (.pub file) the index may be signed with. Repeat it, or separate
    /// files with `;` in W2RPM_KEY_FILE, to trust more than one (around a key rotation).
    #[arg(
        long = "key",
        env = "W2RPM_KEY_FILE",
        global = true,
        value_delimiter = ';'
    )]
    keys: Vec<PathBuf>,
    /// Development: serve the index and packages from a folder instead of the network.
    #[arg(long, global = true, hide = true)]
    index_dir: Option<PathBuf>,
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
    /// Fetch and verify the index now.
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
}

struct Ctx {
    root: Root,
    fetcher: Box<dyn Fetcher>,
    source: Source,
    now: i64,
    manager: Version,
}

fn default_root() -> Result<PathBuf, Error> {
    let home = std::env::var_os("USERPROFILE")
        .ok_or_else(|| Error::new("no_root", "USERPROFILE is not set; pass --root"))?;
    Ok(PathBuf::from(home)
        .join("Saved Games")
        .join("Warcraft2Remastered")
        .join("w2r"))
}

fn context(cli: &Cli) -> Result<Ctx, Error> {
    let root = match &cli.root {
        Some(p) if p.is_absolute() => p.clone(),
        Some(p) => {
            return Err(Error::new(
                "no_root",
                format!("--root must be absolute: {}", p.display()),
            ));
        }
        None => default_root()?,
    };
    if cli.keys.is_empty() {
        return Err(Error::new(
            "no_key",
            "no release key is built in yet; pass --key FILE (a minisign .pub file)",
        ));
    }
    let mut keys = Vec::new();
    for key_file in &cli.keys {
        let key_text = std::fs::read_to_string(key_file)
            .map_err(|e| Error::new("no_key", format!("{}: {e}", key_file.display())))?;
        keys.push(PublicKey::parse_file(&key_text).ok_or_else(|| {
            Error::new(
                "no_key",
                format!("{} is not a minisign public key", key_file.display()),
            )
        })?);
    }
    let (fetcher, index_url): (Box<dyn Fetcher>, String) = match (&cli.index_dir, &cli.index_url) {
        (Some(dir), url) => (
            Box::new(DirFetcher(dir.clone())),
            url.clone()
                .unwrap_or_else(|| "https://dev.invalid/index.json".to_owned()),
        ),
        (None, Some(url)) => (Box::new(Https::default()), url.clone()),
        (None, None) => return Err(Error::new("no_index_url", "pass --index-url URL")),
    };
    // No clock means no expiry check: refuse rather than treat it as 1970.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::new("no_clock", "the system clock is before 1970"))?
        .as_secs() as i64;
    let manager = Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is SemVer");
    Ok(Ctx {
        root: Root(root),
        fetcher,
        source: Source { index_url, keys },
        now,
        manager,
    })
}

impl Ctx {
    /// The cached index when it still verifies, else a fresh one.
    fn index(&self) -> Result<Index, Error> {
        let idx = match fetch::load_cached(&self.root, &self.source.keys, self.now) {
            Ok(i) => i,
            Err(_) => return self.refresh(),
        };
        self.note_rotation(&idx);
        Ok(idx)
    }

    fn refresh(&self) -> Result<Index, Error> {
        let idx = fetch::refresh(&self.root, self.fetcher.as_ref(), &self.source, self.now)?;
        self.note_rotation(&idx);
        Ok(idx)
    }

    /// The publisher announced a new signing key this manager does not trust yet.
    fn note_rotation(&self, idx: &Index) {
        if idx.rotation_pending(&self.source.keys) {
            eprintln!(
                "warning: the index announces a new signing key; update w2rpm (or add the new \
                 key with --key) before the publisher switches to it"
            );
        }
    }
}

fn capability_note(c: &str) -> &'static str {
    match c {
        "game.read" => "read game state",
        "chat.read" => "see chat text and player names from others",
        "chat.write" => "send chat messages as you",
        "net.send" => "send plugin messages to other players",
        "hooks.raw" => "DANGEROUS: patch the game's code directly",
        _ => "",
    }
}

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

fn install_entry(ctx: &Ctx, idx: &Index, entry: &VersionEntry, a: &Approval) -> Result<(), Error> {
    let state = ctx.root.load_state()?;
    let pending = install::pending_approvals(&state, entry);
    let warnings = check::warnings(&state, Some(idx), entry, &ctx.manager);
    let approved = confirm(entry, &pending, &warnings, a)?;
    let bytes = fetch::download(ctx.fetcher.as_ref(), entry)?;
    let out =
        install::install(&ctx.root, entry, &bytes, &approved, &ctx.manager).map_err(hint_io)?;
    let id = &entry.manifest.id;
    match out {
        install::Outcome::Installed => println!("installed {id} {}", entry.version),
        install::Outcome::Updated { from } => println!("updated {id} {from} -> {}", entry.version),
        install::Outcome::Reinstalled => println!("reinstalled {id} {}", entry.version),
    }
    Ok(())
}

/// Folder moves fail while the game holds a plugin DLL open.
fn hint_io(e: Error) -> Error {
    if e.code == "io" {
        Error::new(
            "io",
            format!("{} (is the game running? close it and try again)", e.detail),
        )
    } else {
        e
    }
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

fn run(cli: &Cli) -> Result<bool, Error> {
    match &cli.cmd {
        Cmd::Remove { id, purge } => {
            let root = context_root(cli)?;
            install::remove(&root, id, *purge).map_err(hint_io)?;
            println!("removed {id}");
        }
        Cmd::Enable { id } | Cmd::Disable { id } => {
            let on = matches!(cli.cmd, Cmd::Enable { .. });
            install::set_enabled(&context_root(cli)?, id, on)?;
            println!("{} {id}", if on { "enabled" } else { "disabled" });
        }
        Cmd::Rollback { id } => {
            let v = install::rollback(&context_root(cli)?, id).map_err(hint_io)?;
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
        Cmd::List => list(cli)?,
        Cmd::Refresh => {
            let ctx = context(cli)?;
            let idx = ctx.refresh()?;
            println!("index serial {}: {} plugins", idx.serial, idx.plugins.len());
        }
        Cmd::Search { term } => {
            let ctx = context(cli)?;
            let idx = ctx.index()?;
            for (p, v) in catalog::search(&idx, term.as_deref().unwrap_or("")) {
                println!("{} {} - {}", p.id, v.version, v.manifest.description);
            }
        }
        Cmd::Install { spec, approval } => {
            let ctx = context(cli)?;
            let (id, want) = parse_spec(spec)?;
            let idx = ctx.index()?;
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
            let idx = ctx.index()?;
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
    match &cli.root {
        Some(p) if p.is_absolute() => Ok(Root(p.clone())),
        Some(p) => Err(Error::new(
            "no_root",
            format!("--root must be absolute: {}", p.display()),
        )),
        None => Ok(Root(default_root()?)),
    }
}

fn list(cli: &Cli) -> Result<(), Error> {
    let root = context_root(cli)?;
    let state = root.load_state()?;
    // Update markers come from the cached index only; `list` does not use the network.
    let updates = context(cli)
        .ok()
        .and_then(|c| {
            fetch::load_cached(&c.root, &c.source.keys, c.now)
                .ok()
                .map(|i| (c, i))
        })
        .map(|(c, i)| {
            catalog::updates(&state, &i, &c.manager)
                .into_iter()
                .map(|(s, v)| (s.id.clone(), v.version.to_string()))
                .collect::<Vec<_>>()
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
        println!("{} {} {on}  capabilities: {caps}{up}", p.id, p.version);
    }
    Ok(())
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
