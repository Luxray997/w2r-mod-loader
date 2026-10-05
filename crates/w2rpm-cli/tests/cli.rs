//! End to end: the real `w2rpm` binary against a signed test repository (the Phase 4 exit).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use w2rpm_lib::devrepo::{self, Plugin};

struct Env {
    repo: PathBuf,
    root: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let base = std::env::temp_dir().join(format!("w2rpm-cli-{name}-{}", std::process::id()));
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

    fn w2rpm_with(&self, args: &[&str], stdin: &str, key: &Path) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_w2rpm"));
        c.env_remove("W2R_ROOT")
            .env_remove("W2RPM_INDEX_URL")
            .env_remove("W2RPM_KEY_FILE")
            .arg("--root")
            .arg(&self.root)
            .arg("--index-dir")
            .arg(&self.repo)
            .arg("--key")
            .arg(key)
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

    fn w2rpm(&self, args: &[&str]) -> Output {
        self.w2rpm_with(args, "", &self.repo.join("test.pub"))
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

    assert!(ok(e.w2rpm(&["list"])).contains("no plugins installed"));
    assert!(ok(e.w2rpm(&["search", "example"])).contains("example-plugin 1.0.0"));

    let out = ok(e.w2rpm(&["install", "example-plugin", "--yes"]));
    assert!(
        out.contains("[new] game.read") && out.contains("installed example-plugin 1.0.0"),
        "{out}"
    );
    let listed = ok(e.w2rpm(&["list"]));
    assert!(
        listed.contains("example-plugin 1.0.0 enabled") && listed.contains("game.read"),
        "{listed}"
    );
    assert!(ok(e.w2rpm(&["verify"])).contains("ok"));

    // A newer version that asks for one more capability.
    e.publish(
        2,
        &[
            plugin("1.0.0", &["game.read"], b"dll-1"),
            plugin("1.1.0", &["game.read", "chat.write"], b"dll-2"),
        ],
    );
    ok(e.w2rpm(&["refresh"]));
    assert!(ok(e.w2rpm(&["list"])).contains("update available: 1.1.0"));
    let out = ok(e.w2rpm(&["update", "--yes"]));
    assert!(
        out.contains("[new] chat.write") && out.contains("updated example-plugin 1.0.0 -> 1.1.0"),
        "{out}"
    );
    assert!(ok(e.w2rpm(&["verify"])).contains("ok"));

    assert!(ok(e.w2rpm(&["rollback", "example-plugin"])).contains("example-plugin is now 1.0.0"));
    assert!(ok(e.w2rpm(&["list"])).contains("example-plugin 1.0.0"));
    ok(e.w2rpm(&["disable", "example-plugin"]));
    assert!(ok(e.w2rpm(&["list"])).contains("disabled"));
    ok(e.w2rpm(&["enable", "example-plugin"]));

    // Out-of-band change to the DLL: verify must say so and exit non-zero.
    fs::write(
        e.root.join("plugins/example-plugin/example_plugin.dll"),
        b"swapped",
    )
    .unwrap();
    assert!(fail(e.w2rpm(&["verify"])).contains("differs from the pinned hash"));

    ok(e.w2rpm(&["remove", "example-plugin"]));
    assert!(ok(e.w2rpm(&["list"])).contains("no plugins installed"));
}

#[test]
fn the_capability_question_is_asked_and_hooks_raw_is_never_silent() {
    let e = Env::new("approval");
    e.publish(1, &[plugin("1.0.0", &["hooks.raw"], b"dll")]);
    let out = fail(e.w2rpm(&["install", "example-plugin", "--yes"]));
    assert!(out.contains("--allow-hooks-raw"), "{out}");
    assert!(!e.root.join("plugins/example-plugin").exists());

    let out = fail(e.w2rpm_with(
        &["install", "example-plugin"],
        "n\n",
        &e.repo.join("test.pub"),
    ));
    assert!(
        out.contains("DANGEROUS") && out.contains("declined"),
        "{out}"
    );
    assert!(!e.root.join("plugins/example-plugin").exists());

    ok(e.w2rpm_with(
        &["install", "example-plugin"],
        "y\n",
        &e.repo.join("test.pub"),
    ));
    assert!(
        e.root
            .join("plugins/example-plugin/example_plugin.dll")
            .exists()
    );
}

#[test]
fn an_index_signed_by_another_key_is_refused() {
    let e = Env::new("wrongkey");
    e.publish(1, &[plugin("1.0.0", &[], b"dll")]);
    // The signer's own key with one character of the key bytes changed: well-formed, wrong key.
    let good = fs::read_to_string(e.repo.join("test.pub")).unwrap();
    let (comment, body) = good.split_once('\n').unwrap();
    let mut body: Vec<u8> = body.trim_end().bytes().collect();
    body[45] = if body[45] == b'A' { b'B' } else { b'A' };
    let other = e.repo.join("other.pub");
    fs::write(
        &other,
        format!("{comment}\n{}\n", String::from_utf8(body).unwrap()),
    )
    .unwrap();
    let out = fail(e.w2rpm_with(&["refresh"], "", &other));
    assert!(out.contains("bad_signature"), "{out}");
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
    let out = ok(e.w2rpm(&["install", "example-plugin", "--yes"]));
    assert!(out.contains("Built for game builds: 1.0.2.2818"), "{out}");
    assert!(
        out.contains("WARNING: depends on some-library which is not installed")
            && out.contains("w2rpm install some-library"),
        "{out}"
    );
    // The warning does not block; once the library is in, the next install is quiet.
    let out = ok(e.w2rpm(&["install", "some-library", "--yes"]));
    assert!(!out.contains("WARNING"), "{out}");
    let out = ok(e.w2rpm(&["install", "example-plugin", "--yes"]));
    assert!(!out.contains("WARNING"), "{out}");
}

#[test]
fn a_pending_key_rotation_is_announced_and_a_second_key_is_accepted() {
    let e = Env::new("rotate");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // A well-formed key the manager does not trust, as the announced next key.
    let pubfile = {
        e.publish(1, &[]);
        fs::read_to_string(e.repo.join("test.pub")).unwrap()
    };
    let body = pubfile.lines().nth(1).unwrap();
    let mut next: Vec<u8> = body.bytes().collect();
    next[45] = if next[45] == b'A' { b'B' } else { b'A' };
    let next = String::from_utf8(next).unwrap();
    devrepo::build_with(
        &e.repo,
        now,
        2,
        &[plugin("1.0.0", &[], b"dll")],
        Some(&next),
    )
    .unwrap();
    let out = e.w2rpm(&["refresh"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("announces a new signing key"),
        "{}",
        text(&out)
    );

    // Trusting the announced key silences it (two --key flags, either may have signed).
    let next_file = e.repo.join("next.pub");
    fs::write(&next_file, format!("untrusted comment: next\n{next}\n")).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_w2rpm"));
    let out = c
        .env_remove("W2R_ROOT")
        .args(["--root"])
        .arg(&e.root)
        .arg("--index-dir")
        .arg(&e.repo)
        .arg("--key")
        .arg(&next_file)
        .arg("--key")
        .arg(e.repo.join("test.pub"))
        .arg("refresh")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(!text(&out).contains("announces"), "{}", text(&out));
}
