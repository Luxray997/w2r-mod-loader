//! Load policy end to end: a temporary host root with `state.json` and plugin folders made from
//! the fixture DLLs, through checks 1 to 10.

#![cfg(windows)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::OnceLock;

use w2r_core::host::Log;
use w2r_core::host::Services;
use w2r_core::json::{Number, Value};
use w2r_core::loader::sha256_of;
use w2r_core::manifest::Build;
use w2r_core::plugins::load_all;

#[derive(Default)]
struct Lines(RefCell<Vec<String>>);

impl Log for Lines {
    fn line(&self, level: u32, plugin: &str, msg: &str) {
        self.0
            .borrow_mut()
            .push(format!("[{level}] {plugin}: {msg}"));
    }
}

impl Lines {
    fn has(&self, needle: &str) -> bool {
        self.0.borrow().iter().any(|l| l.contains(needle))
    }
    fn dump(&self) -> String {
        self.0.borrow().join("\n")
    }
}

fn fixtures() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let target_dir = root.join("target").join("plugins");
        let status = Command::new(env!("CARGO"))
            .args(["build", "-q", "-p", "w2r-test-plugins", "--examples"])
            .args(["--target", env!("W2R_TARGET")])
            .env("CARGO_TARGET_DIR", &target_dir)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building the fixture plugins failed");
        target_dir
            .join(env!("W2R_TARGET"))
            .join("debug")
            .join("examples")
    })
}

fn s(v: &str) -> Value {
    Value::String(v.to_owned())
}

fn n(v: u64) -> Value {
    Value::Number(Number {
        text: v.to_string(),
    })
}

fn arr(items: Vec<Value>) -> Value {
    Value::Array(items)
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn set(v: &mut Value, key: &str, new: Value) {
    if let Value::Object(pairs) = v {
        match pairs.iter_mut().find(|(k, _)| k == key) {
            Some((_, old)) => *old = new,
            None => pairs.push((key.to_owned(), new)),
        }
    }
}

fn manifest(id: &str, entry: &str) -> Value {
    obj(vec![
        ("schema", n(1)),
        ("id", s(id)),
        ("name", s("Fixture")),
        ("version", s("1.0.0")),
        ("author", s("tests")),
        ("description", s("A fixture.")),
        ("entry", s(entry)),
        ("abi", n(1)),
        ("game_builds", arr(vec![s("1.0.2.2818")])),
        ("capabilities", arr(vec![])),
        ("affects_simulation", Value::Bool(false)),
        ("dependencies", arr(vec![])),
    ])
}

fn dep(id: &str, req: &str) -> Value {
    obj(vec![("id", s(id)), ("version_req", s(req))])
}

/// One plugin folder plus its state.json entry.
struct Plugin {
    id: &'static str,
    fixture: &'static str,
    /// `None` leaves plugin.json out; `Some(bytes)` writes those bytes verbatim.
    manifest: Option<Vec<u8>>,
    enabled: bool,
    approved: Vec<&'static str>,
    /// `Some` overrides the pin (otherwise the real hash).
    pin: Option<String>,
}

impl Plugin {
    fn new(id: &'static str, fixture: &'static str, manifest: Value) -> Plugin {
        Plugin {
            id,
            fixture,
            manifest: Some(manifest.to_json().into_bytes()),
            enabled: true,
            approved: Vec::new(),
            pin: None,
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Builds a host root under temp with the given plugins and returns it.
fn root_with(tag: &str, plugins: &[Plugin]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("w2r-load-all-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for sub in ["plugins", "config", "cache", "logs"] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
    }
    let mut entries: Vec<(String, Value)> = Vec::new();
    for p in plugins {
        let folder = root.join("plugins").join(p.id);
        std::fs::create_dir_all(&folder).unwrap();
        let dll_name = format!("{}.dll", p.fixture);
        let dll = folder.join(&dll_name);
        std::fs::copy(fixtures().join(&dll_name), &dll).unwrap();
        if let Some(bytes) = &p.manifest {
            std::fs::write(folder.join("plugin.json"), bytes).unwrap();
        }
        let pin = p
            .pin
            .clone()
            .unwrap_or_else(|| hex(&sha256_of(&dll).unwrap()));
        let approved = p.approved.iter().map(|c| s(c)).collect();
        entries.push((
            p.id.to_owned(),
            obj(vec![
                ("version", s("1.0.0")),
                ("enabled", Value::Bool(p.enabled)),
                ("sha256", s(&pin)),
                ("approved_capabilities", arr(approved)),
            ]),
        ));
    }
    let state = obj(vec![("schema", n(1)), ("plugins", Value::Object(entries))]);
    std::fs::write(root.join("config").join("state.json"), state.to_json()).unwrap();
    root
}

fn running() -> Build {
    Build::parse("1.0.2.2818").unwrap()
}

#[test]
fn an_enabled_valid_plugin_loads_with_its_approved_capabilities() {
    let mut m = manifest("hello-plugin", "hello_plugin.dll");
    set(&mut m, "capabilities", arr(vec![s("chat.write")]));
    let mut hello = Plugin::new("hello-plugin", "hello_plugin", m);
    hello.approved = vec!["chat.write", "game.read"];
    let off = Plugin {
        enabled: false,
        ..Plugin::new(
            "off-plugin",
            "crash_plugin",
            manifest("off-plugin", "crash_plugin.dll"),
        )
    };
    let root = root_with("ok", &[hello, off]);

    let log = Rc::new(Lines::default());
    let (reg, _) = load_all(&root, running(), log.clone(), None, Services::none());
    assert_eq!(reg.len(), 1, "{}", log.dump());
    let slot = reg.find("hello-plugin").unwrap();
    assert!(slot.is_active());
    assert!(slot.has_cap("chat.write"));
    assert!(slot.has_cap("game.read"));
    assert!(!slot.has_cap("hooks.raw"));
    assert!(
        log.has("[1] hello-plugin: hello from hello_plugin"),
        "{}",
        log.dump()
    );
    assert!(log.has("1 of 1 enabled plugins loaded"), "{}", log.dump());
    assert!(reg.find("off-plugin").is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn each_check_refuses_with_its_code_and_leaves_no_slot() {
    let mut old_build = manifest("old-build", "hello_plugin.dll");
    set(
        &mut old_build,
        "game_builds",
        arr(vec![s("1.0.0.1-1.0.2.2817")]),
    );
    let mut new_abi = manifest("new-abi", "hello_plugin.dll");
    set(&mut new_abi, "abi", n(2));
    let mut needs_caps = manifest("needs-caps", "hello_plugin.dll");
    set(&mut needs_caps, "capabilities", arr(vec![s("hooks.raw")]));
    let mut dep_missing = manifest("dep-missing", "hello_plugin.dll");
    set(
        &mut dep_missing,
        "dependencies",
        arr(vec![dep("nope", ">=1.0.0")]),
    );

    let plugins = vec![
        Plugin {
            manifest: None,
            ..Plugin::new(
                "no-manifest",
                "hello_plugin",
                manifest("no-manifest", "hello_plugin.dll"),
            )
        },
        Plugin {
            manifest: Some(b"{ not json".to_vec()),
            ..Plugin::new("bad-manifest", "hello_plugin", manifest("x", "x.dll"))
        },
        Plugin::new(
            "wrong-id",
            "hello_plugin",
            manifest("other-id", "hello_plugin.dll"),
        ),
        Plugin::new("old-build", "hello_plugin", old_build),
        Plugin::new("new-abi", "hello_plugin", new_abi),
        Plugin {
            pin: Some("00".repeat(32)),
            ..Plugin::new(
                "bad-pin",
                "hello_plugin",
                manifest("bad-pin", "hello_plugin.dll"),
            )
        },
        Plugin::new("needs-caps", "hello_plugin", needs_caps),
        Plugin::new("dep-missing", "hello_plugin", dep_missing),
        Plugin::new(
            "refuses",
            "load_fails",
            manifest("refuses", "load_fails.dll"),
        ),
        Plugin::new(
            "no-entry",
            "null_entry",
            manifest("no-entry", "null_entry.dll"),
        ),
    ];
    let root = root_with("refusals", &plugins);
    let log = Rc::new(Lines::default());
    let (reg, _) = load_all(&root, running(), log.clone(), None, Services::none());

    for (id, code) in [
        ("no-manifest", "E_STATE: plugin.json is missing"),
        ("bad-manifest", "E_MANIFEST: not_json"),
        ("wrong-id", "E_STATE: manifest id other-id differs"),
        ("old-build", "E_BUILD: game build 1.0.2.2818"),
        ("new-abi", "E_ABI: needs ABI 2"),
        ("bad-pin", "E_HASH: sha256 of hello_plugin.dll differs"),
        ("needs-caps", "E_CAPS: capability hooks.raw"),
        ("dep-missing", "E_DEP: dependency nope is not loaded"),
        ("refuses", "disabled: E_LOAD: on_load returned -42"),
        (
            "no-entry",
            "disabled: E_ENTRY: w2r_plugin_main returned null",
        ),
    ] {
        assert!(
            log.has(&format!("{id}: {code}")),
            "{id}: expected {code:?} in\n{}",
            log.dump()
        );
    }
    // Refused before check 9: no slot. Refused at 9 or 10: a disabled slot.
    assert_eq!(reg.len(), 2, "{}", log.dump());
    assert!(!reg.find("refuses").unwrap().is_active());
    assert!(!reg.find("no-entry").unwrap().is_active());
    assert!(log.has("0 of 10 enabled plugins loaded"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dependencies_load_first_and_version_and_cycles_are_enforced() {
    let mut needs_hello = manifest("needs-hello", "hello_plugin.dll");
    set(
        &mut needs_hello,
        "dependencies",
        arr(vec![dep("hello-plugin", ">=1.0.0,<2.0.0")]),
    );
    let mut wants_newer = manifest("wants-newer", "hello_plugin.dll");
    set(
        &mut wants_newer,
        "dependencies",
        arr(vec![dep("hello-plugin", ">=9.0.0")]),
    );
    let mut cycle_a = manifest("cycle-a", "hello_plugin.dll");
    set(
        &mut cycle_a,
        "dependencies",
        arr(vec![dep("cycle-b", ">=1.0.0")]),
    );
    let mut cycle_b = manifest("cycle-b", "hello_plugin.dll");
    set(
        &mut cycle_b,
        "dependencies",
        arr(vec![dep("cycle-a", ">=1.0.0")]),
    );
    let mut after_crasher = manifest("after-crasher", "hello_plugin.dll");
    set(
        &mut after_crasher,
        "dependencies",
        arr(vec![dep("crasher", ">=1.0.0")]),
    );

    // Listed so that id order alone would load dependents first.
    let plugins = vec![
        Plugin::new("after-crasher", "hello_plugin", after_crasher),
        Plugin::new("cycle-a", "hello_plugin", cycle_a),
        Plugin::new("cycle-b", "hello_plugin", cycle_b),
        Plugin::new(
            "crasher",
            "load_fails",
            manifest("crasher", "load_fails.dll"),
        ),
        Plugin::new(
            "hello-plugin",
            "hello_plugin",
            manifest("hello-plugin", "hello_plugin.dll"),
        ),
        Plugin::new("needs-hello", "hello_plugin", needs_hello),
        Plugin::new("wants-newer", "hello_plugin", wants_newer),
    ];
    let root = root_with("deps", &plugins);
    let log = Rc::new(Lines::default());
    let (reg, _) = load_all(&root, running(), log.clone(), None, Services::none());

    let hello = reg.find("hello-plugin").expect("hello loaded");
    let needs = reg.find("needs-hello").expect("dependent loaded");
    assert!(hello.is_active() && needs.is_active(), "{}", log.dump());
    assert!(hello.index < needs.index, "dependency loads first");
    assert!(reg.find("wants-newer").is_none());
    assert!(
        log.has("wants-newer: E_DEP: dependency hello-plugin"),
        "{}",
        log.dump()
    );
    assert!(log.has("cycle-a: E_DEP: part of a dependency cycle"));
    assert!(log.has("cycle-b: E_DEP: part of a dependency cycle"));
    // crasher reaches check 10 and fails, so after-crasher fails check 8 at its turn.
    assert!(!reg.find("crasher").unwrap().is_active());
    assert!(reg.find("after-crasher").is_none());
    assert!(
        log.has("after-crasher: E_DEP: dependency crasher is not loaded"),
        "{}",
        log.dump()
    );
    assert!(log.has("2 of 7 enabled plugins loaded"), "{}", log.dump());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_or_bad_state_file_loads_nothing() {
    let root = std::env::temp_dir().join(format!("w2r-load-all-{}-nostate", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config")).unwrap();
    let log = Rc::new(Lines::default());
    let (reg, _) = load_all(&root, running(), log.clone(), None, Services::none());
    assert!(reg.is_empty());
    assert!(log.has("no config\\state.json"));

    std::fs::write(
        root.join("config").join("state.json"),
        b"{\"schema\": 2, \"plugins\": {}}",
    )
    .unwrap();
    let log = Rc::new(Lines::default());
    let (reg, _) = load_all(&root, running(), log.clone(), None, Services::none());
    assert!(reg.is_empty());
    assert!(log.has("state.json refused (bad_schema)"));
    let _ = std::fs::remove_dir_all(&root);
}
