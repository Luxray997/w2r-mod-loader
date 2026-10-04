//! Loads the built `w2r_core.dll` the way the game loads `version.dll`: `DllMain` wires the
//! forwarders and starts the host, which logs and runs the build guard (against this test
//! process, so the guard must fail cleanly). On x86 the 17 version.dll exports must be present
//! under their undecorated names and actually forward.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(target_arch = "x86")]
use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Builds the host cdylib for this test's target in the shared fixture target dir.
fn host_dll() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let target_dir = root.join("target").join("plugins");
    let status = Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "w2r-core", "--lib"])
        .args(["--target", env!("W2R_TARGET")])
        .env("CARGO_TARGET_DIR", &target_dir)
        .current_dir(&root)
        .status()
        .expect("run cargo");
    assert!(status.success(), "building w2r-core failed");
    target_dir
        .join(env!("W2R_TARGET"))
        .join("debug")
        .join("w2r_core.dll")
}

fn wait_for(path: &Path, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.contains(needle) {
                return text;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?} in {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn loading_the_host_dll_starts_it_and_the_build_guard_refuses_this_process() {
    let root = std::env::temp_dir().join(format!("w2r-proxy-test-{}", std::process::id()));
    // Set before DllMain runs; the startup thread reads it.
    unsafe { std::env::set_var("W2R_ROOT", &root) };

    let dll = host_dll();
    let module = unsafe { LoadLibraryW(wide(dll.to_str().unwrap()).as_ptr()) };
    assert!(!module.is_null(), "LoadLibraryW({}) failed", dll.display());

    let log = root.join("logs").join("w2r.log");
    let text = wait_for(&log, "the host is inactive this session");
    assert!(text.contains("w2rpm host"), "{text}");
    assert!(text.contains("attached"), "{text}");
    assert!(
        text.contains("expecting Warcraft II Remastered v1.0.2.2818"),
        "{text}"
    );
    assert!(text.contains("build-guard: FAILED ("), "{text}");
    assert!(!text.contains("proxy: could not resolve"), "{text}");
    for sub in ["plugins", "config", "cache", "logs"] {
        assert!(root.join(sub).is_dir(), "{sub}");
    }

    #[cfg(target_arch = "x86")]
    {
        let names = include_str!("../version_exports.txt");
        for name in names.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let cname = format!("{name}\0");
            let f = unsafe { GetProcAddress(module, cname.as_ptr()) };
            assert!(f.is_some(), "export {name} missing from w2r_core.dll");
        }
        type SizeW = unsafe extern "system" fn(*const u16, *mut u32) -> u32;
        let f =
            unsafe { GetProcAddress(module, c"GetFileVersionInfoSizeW".as_ptr().cast()) }.unwrap();
        let size_w: SizeW = unsafe { core::mem::transmute(f) };
        let kernel32 = wide(&format!(
            "{}\\System32\\kernel32.dll",
            std::env::var("SystemRoot").unwrap()
        ));
        let mut handle = 0u32;
        assert!(
            unsafe { size_w(kernel32.as_ptr(), &mut handle) } > 0,
            "forwarded call returned 0"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
