fn main() {
    // The integration tests build fixture DLLs (and the host itself) for the target they run on.
    println!(
        "cargo:rustc-env=W2R_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    println!("cargo:rerun-if-changed=build.rs");
}
