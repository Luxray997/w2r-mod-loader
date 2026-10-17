fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon/w2r-mod-loader.ico");
    // Give the exe its icon (Explorer, taskbar, shortcuts); the window icon is set at run time.
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon/w2r-mod-loader.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the exe icon: {e}");
        }
    }
}
