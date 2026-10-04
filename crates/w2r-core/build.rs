fn main() {
    // The integration tests build the fixture plugin DLLs for the same target they run on.
    println!(
        "cargo:rustc-env=W2R_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    println!("cargo:rerun-if-changed=build.rs");
}
