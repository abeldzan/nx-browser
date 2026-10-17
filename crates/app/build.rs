// Embeds the camera/microphone usage strings and the ATS web-content exemption from Info.plist in the binary so
// `cargo run` can use them too: without them macOS and WebKit refuse capture before any prompt, and http sites. Only those keys: an embedded bundle id would
// make LaunchServices treat a `cargo run` process as NxBrowser.app and activate it instead of launching the app.
fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    let source = std::fs::read_to_string("Info.plist").expect("crates/app/Info.plist");
    let keys: String = source.lines().filter(|line| line.contains("UsageDescription") || line.contains("NSAppTransportSecurity")).map(|line| format!("{line}\n")).collect();
    assert!(!keys.is_empty(), "Info.plist has no usage descriptions");
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n{keys}</dict>\n</plist>\n"
    );
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("Info.plist");
    std::fs::write(&out, plist).unwrap();
    println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}", out.display());
}
