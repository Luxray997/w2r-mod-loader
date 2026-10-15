//! End to end: the real `w2r-loader` binary against a test repository (the Phase 4 exit).

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use w2r_loader_lib::devrepo::{self, Plugin};

struct Env {
    repo: PathBuf,
    root: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let base = std::env::temp_dir().join(format!("w2r-loader-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        Env {
            repo: base.join("repo"),
            root: base.join("root"),
        }
    }

    fn publish(&self, serial: u64, plugins: &[Plugin]) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        devrepo::build(&self.repo, now, serial, plugins).unwrap();
    }

    fn cli_with(&self, args: &[&str], stdin: &str) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_w2r-loader"));
        c.env_remove("W2R_ROOT")
            .env_remove("W2RPM_INDEX_URL")
            .arg("--root")
            .arg(&self.root)
            .arg("--index-dir")
            .arg(&self.repo)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.cli_with(args, "")
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn ok(o: Output) -> String {
    assert!(o.status.success(), "failed: {}", text(&o));
    text(&o)
}

fn fail(o: Output) -> String {
    assert!(!o.status.success(), "should have failed: {}", text(&o));
    text(&o)
}

fn plugin<'a>(version: &'a str, caps: &'a [&'a str], dll: &'a [u8]) -> Plugin<'a> {
    Plugin {
        id: "example-plugin",
        version,
        capabilities: caps,
        dll,
        dependencies: &[],
    }
}

#[test]
fn install_list_verify_update_rollback_remove() {
    let e = Env::new("flow");
    e.publish(1, &[plugin("1.0.0", &["game.read"], b"dll-1")]);

    assert!(ok(e.cli(&["list"])).contains("no plugins installed"));
    assert!(ok(e.cli(&["search", "example"])).contains("example-plugin 1.0.0"));

    let out = ok(e.cli(&["install", "example-plugin", "--yes"]));
    assert!(
        out.contains("[new] game.read") && out.contains("installed example-plugin 1.0.0"),
        "{out}"
    );
    let listed = ok(e.cli(&["list"]));
    assert!(
        listed.contains("example-plugin 1.0.0 by test enabled") && listed.contains("game.read"),
        "{listed}"
    );
    assert!(ok(e.cli(&["verify"])).contains("ok"));

    // A newer version that asks for one more capability.
    e.publish(
        2,
        &[
            plugin("1.0.0", &["game.read"], b"dll-1"),
            plugin("1.1.0", &["game.read", "chat.write"], b"dll-2"),
        ],
    );
    ok(e.cli(&["refresh"]));
    assert!(ok(e.cli(&["list"])).contains("update available: 1.1.0"));
    let out = ok(e.cli(&["update", "--yes"]));
    assert!(
        out.contains("[new] chat.write") && out.contains("updated example-plugin 1.0.0 -> 1.1.0"),
        "{out}"
    );
    assert!(ok(e.cli(&["verify"])).contains("ok"));

    assert!(ok(e.cli(&["rollback", "example-plugin"])).contains("example-plugin is now 1.0.0"));
    assert!(ok(e.cli(&["list"])).contains("example-plugin 1.0.0"));
    ok(e.cli(&["disable", "example-plugin"]));
    assert!(ok(e.cli(&["list"])).contains("disabled"));
    ok(e.cli(&["enable", "example-plugin"]));

    // Out-of-band change to the DLL: verify must say so and exit non-zero.
    fs::write(
        e.root.join("plugins/example-plugin/example_plugin.dll"),
        b"swapped",
    )
    .unwrap();
    assert!(fail(e.cli(&["verify"])).contains("differs from the pinned hash"));

    ok(e.cli(&["remove", "example-plugin"]));
    assert!(ok(e.cli(&["list"])).contains("no plugins installed"));
}

#[test]
fn the_capability_question_is_asked_and_hooks_raw_is_never_silent() {
    let e = Env::new("approval");
    e.publish(1, &[plugin("1.0.0", &["hooks.raw"], b"dll")]);
    let out = fail(e.cli(&["install", "example-plugin", "--yes"]));
    assert!(out.contains("--allow-hooks-raw"), "{out}");
    assert!(!e.root.join("plugins/example-plugin").exists());

    let out = fail(e.cli_with(&["install", "example-plugin"], "n\n"));
    assert!(
        out.contains("DANGEROUS") && out.contains("declined"),
        "{out}"
    );
    assert!(!e.root.join("plugins/example-plugin").exists());

    ok(e.cli_with(&["install", "example-plugin"], "y\n"));
    assert!(
        e.root
            .join("plugins/example-plugin/example_plugin.dll")
            .exists()
    );
}

#[test]
fn a_malformed_index_is_refused_and_not_cached() {
    let e = Env::new("badindex");
    e.publish(1, &[plugin("1.0.0", &[], b"dll")]);
    fs::write(e.repo.join("index.json"), b"{").unwrap();
    let out = fail(e.cli(&["refresh"]));
    assert!(out.contains("not_json"), "{out}");
    assert!(!e.root.join("cache/index.json").exists());
}

#[test]
fn install_warns_about_game_builds_and_missing_dependencies() {
    let e = Env::new("warn");
    let mut p = plugin("1.0.0", &[], b"dll");
    p.dependencies = &[("some-library", ">=1.0.0")];
    let lib = Plugin {
        id: "some-library",
        version: "1.2.0",
        capabilities: &[],
        dll: b"lib",
        dependencies: &[],
    };
    e.publish(1, &[p, lib]);
    let out = ok(e.cli(&["install", "example-plugin", "--yes"]));
    assert!(out.contains("Built for game builds: 1.0.2.2818"), "{out}");
    assert!(
        out.contains("WARNING: depends on some-library which is not installed")
            && out.contains("w2r-loader install some-library"),
        "{out}"
    );
    // The warning does not block; once the library is in, the next install is quiet.
    let out = ok(e.cli(&["install", "some-library", "--yes"]));
    assert!(!out.contains("WARNING"), "{out}");
    let out = ok(e.cli(&["install", "example-plugin", "--yes"]));
    assert!(!out.contains("WARNING"), "{out}");
}

fn fake_core(version: &str) -> Vec<u8> {
    let mut b = b"MZ..".to_vec();
    b.extend_from_slice(b"=== w2rpm host attached W2R-CORE-VERSION:");
    b.extend_from_slice(version.as_bytes());
    b.extend_from_slice(b";");
    b
}

fn publish_core(env: &Env, serial: u64, version: &str) {
    let dll = fake_core(version);
    fs::create_dir_all(&env.repo).unwrap();
    fs::write(env.repo.join("version.dll"), &dll).unwrap();
    fs::write(
        env.repo.join("core.json"),
        format!(
            r#"{{"schema":1,"serial":{serial},"version":"{version}","url":"https://dev.invalid/version.dll","sha256":"{}","size":{},"min_manager":"0.1.0","game_builds":["1.0.2.2818"]}}"#,
            w2r_loader_lib::package::sha256_hex(&dll),
            dll.len()
        ),
    )
    .unwrap();
}

#[test]
fn core_installs_updates_rolls_back_and_leaves_another_mod_alone() {
    let env = Env::new("core");
    let game = env.root.parent().unwrap().join("game");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join("Warcraft II.exe"), b"exe").unwrap();
    let g = game.to_str().unwrap();

    publish_core(&env, 1, "0.1.0");
    let out = ok(env.cli(&["--game-dir", g, "core", "update"]));
    assert!(out.contains("core is now 0.1.0"), "{out}");
    assert_eq!(fs::read(game.join("version.dll")).unwrap(), fake_core("0.1.0"));

    // Same version again: nothing to do. The folder was remembered, so no --game-dir.
    assert!(ok(env.cli(&["core", "update"])).contains("up to date"));

    publish_core(&env, 2, "0.2.0");
    assert!(ok(env.cli(&["core", "status"])).contains("an update is available"));
    assert!(ok(env.cli(&["core", "update"])).contains("core is now 0.2.0"));
    assert_eq!(fs::read(game.join("version.dll")).unwrap(), fake_core("0.2.0"));

    assert!(ok(env.cli(&["core", "rollback"])).contains("core is now 0.1.0"));
    assert_eq!(fs::read(game.join("version.dll")).unwrap(), fake_core("0.1.0"));

    // An older core.json is a replay: refused, nothing changes.
    publish_core(&env, 1, "0.3.0");
    assert!(fail(env.cli(&["core", "update"])).contains("rollback"));
    assert_eq!(fs::read(game.join("version.dll")).unwrap(), fake_core("0.1.0"));

    // Someone else's version.dll is never replaced.
    fs::write(game.join("version.dll"), b"MZ wc2r-plus").unwrap();
    publish_core(&env, 5, "0.4.0");
    assert!(fail(env.cli(&["core", "update"])).contains("not_ours"));
    assert_eq!(fs::read(game.join("version.dll")).unwrap(), b"MZ wc2r-plus");
}
