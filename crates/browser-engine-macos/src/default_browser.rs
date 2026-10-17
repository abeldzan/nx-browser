use objc2_app_kit::NSWorkspace;
use objc2_foundation::{ns_string, NSBundle};

/// Asks macOS to make this app the handler for web links; the system confirms with the user.
pub fn make_default_browser() -> anyhow::Result<()> {
    let bundle = NSBundle::mainBundle();
    // `cargo run` has no bundle (see crates/app/build.rs), so LaunchServices has nothing to register.
    anyhow::ensure!(bundle.bundleIdentifier().is_some(), "Only the bundled NxBrowser.app can be the default browser");
    let workspace = NSWorkspace::sharedWorkspace();
    for scheme in [ns_string!("http"), ns_string!("https")] {
        workspace.setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(&bundle.bundleURL(), scheme, None);
    }
    Ok(())
}
