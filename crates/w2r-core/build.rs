use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
    // The integration tests build fixture DLLs (and the host itself) for the target they run on.
    println!(
        "cargo:rustc-env=W2R_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    // The commit the host was built from, for the title screen; empty without git. `-dirty`
    // marks uncommitted changes to tracked files.
    let hash = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_default();
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let label = match (hash.is_empty(), dirty) {
        (true, _) => String::new(),
        (false, true) => format!("{hash}-dirty"),
        (false, false) => hash,
    };
    println!("cargo:rustc-env=W2R_GIT_HASH={label}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
