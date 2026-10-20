//! Terminal tabs: a tab whose URL is `nxbrowser://terminal` shows a shell instead of a web page.

mod emu;
mod input;
mod kitty;
mod pty;
mod view;

use browser_core::Settings;
use gpui::{App, AppContext, Entity, Global};
use std::collections::HashMap;
use std::path::PathBuf;
pub use view::TerminalView;

pub const TERMINAL_URL: &str = "nxbrowser://terminal";

pub fn is_terminal(url: &str) -> bool {
    url == TERMINAL_URL || url.starts_with("nxbrowser://terminal?")
}

/// The URL a terminal tab is saved under, so a restored tab starts in the same folder.
pub fn terminal_url(cwd: &std::path::Path) -> String {
    format!("{TERMINAL_URL}?cwd={}", cwd.display())
}

/// The folder a terminal tab is in, as `~/dev/app`.
pub fn location(url: &str) -> String {
    view::short_path(&cwd_of(url))
}

fn cwd_of(url: &str) -> PathBuf {
    url.split_once("?cwd=").map(|(_, dir)| PathBuf::from(dir)).or_else(|| std::env::var_os("HOME").map(PathBuf::from)).unwrap_or_else(|| "/".into())
}

/// Installed monospace font families, sorted. The scan takes over a second, so it runs once,
/// warmed off the main thread at launch (see `lib.rs`).
pub fn monospace_fonts() -> &'static [String] {
    &families().1
}

/// Every installed font family, sorted; from the same scan as `monospace_fonts`.
pub fn all_fonts() -> &'static [String] {
    &families().0
}

/// (all, monospace) installed families.
fn families() -> &'static (Vec<String>, Vec<String>) {
    static FONTS: std::sync::OnceLock<(Vec<String>, Vec<String>)> = std::sync::OnceLock::new();
    FONTS.get_or_init(|| {
        use core_text::font_descriptor::{SymbolicTraitAccessors, TraitAccessors};
        // shortcut: core-text 21's get_descriptors over-retains the array (a one-time leak of the list); fine for a single call.
        let descriptors = core_text::font_collection::create_for_all_families().get_descriptors();
        let (mut all, mut mono) = (Vec::new(), vec!["Menlo".to_string()]);
        for descriptor in descriptors.iter().flat_map(|list| list.iter().collect::<Vec<_>>()) {
            let name = descriptor.family_name();
            if name.starts_with('.') {
                continue;
            }
            if descriptor.traits().symbolic_traits().is_monospace() {
                mono.push(name.clone());
            }
            all.push(name);
        }
        for names in [&mut all, &mut mono] {
            names.sort_by_key(|name| name.to_lowercase());
            names.dedup();
        }
        (all, mono)
    })
}

/// The user's terminal settings, readable from any window's terminals.
#[derive(Default)]
struct Prefs(Settings);

impl Global for Prefs {}

/// The app's current settings, kept here for terminals and read by `paint` for the interface font size.
pub(crate) fn prefs(cx: &App) -> &Settings {
    cx.try_global::<Prefs>().map_or_else(|| default_settings(), |prefs| &prefs.0)
}

fn default_settings() -> &'static Settings {
    static DEFAULT: std::sync::OnceLock<Settings> = std::sync::OnceLock::new();
    DEFAULT.get_or_init(Settings::default)
}

/// Hands changed settings to every open terminal (font, size, cursor and scrollback apply right away).
pub fn set_prefs(settings: &Settings, cx: &mut App) {
    if cx.try_global::<Prefs>().is_some_and(|prefs| prefs.0 == *settings) {
        return;
    }
    cx.set_global(Prefs(settings.clone()));
    let views = cx.try_global::<Terminals>().map(|terminals| terminals.0.values().cloned().collect::<Vec<_>>()).unwrap_or_default();
    for view in views {
        view.update(cx, |view, cx| {
            view.apply_prefs(settings);
            cx.notify();
        });
    }
}

/// Live terminals by tab id. App-wide, so a tab torn off into another OS window keeps its shell.
#[derive(Default)]
struct Terminals(HashMap<String, Entity<TerminalView>>);

impl Global for Terminals {}

/// The tab's terminal, starting its shell on first use.
pub fn terminal_for(tab_id: &str, url: &str, cx: &mut App) -> Entity<TerminalView> {
    if let Some(view) = get(tab_id, cx) {
        return view;
    }
    let cwd = cwd_of(url);
    let settings = prefs(cx).clone();
    let view = cx.new(|cx| TerminalView::new(&cwd, &settings, cx));
    cx.default_global::<Terminals>().0.insert(tab_id.to_string(), view.clone());
    view
}

pub fn get(tab_id: &str, cx: &App) -> Option<Entity<TerminalView>> {
    cx.try_global::<Terminals>().and_then(|terminals| terminals.0.get(tab_id).cloned())
}

/// Ends shells whose tab is gone or now shows a web page (the PTY is killed when the view drops).
pub fn retain(mut keep: impl FnMut(&str) -> bool, cx: &mut App) {
    if let Some(terminals) = cx.try_global::<Terminals>() {
        if terminals.0.keys().all(|id| keep(id)) {
            return;
        }
    }
    cx.default_global::<Terminals>().0.retain(|id, _| keep(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_lists_share_one_scan() {
        let (all, mono) = (all_fonts(), monospace_fonts());
        assert!(all.iter().any(|name| name == "Helvetica") && mono.iter().any(|name| name == "Menlo"));
        assert!(!mono.iter().any(|name| name == "Helvetica") && mono.iter().all(|name| all.contains(name)));
        assert!(all.windows(2).all(|pair| pair[0].to_lowercase() < pair[1].to_lowercase()));
    }

    #[test]
    fn font_list_has_monospace_families_only() {
        let fonts = monospace_fonts();
        assert!(fonts.iter().any(|name| name == "Menlo") && fonts.iter().any(|name| name == "Monaco"), "{fonts:?}");
        assert!(!fonts.iter().any(|name| name == "Helvetica"), "{fonts:?}");
    }

    #[test]
    fn urls_carry_the_folder() {
        let url = terminal_url(std::path::Path::new("/tmp/a b"));
        assert!(is_terminal(&url) && is_terminal(TERMINAL_URL) && !is_terminal("nxbrowser://home"));
        assert_eq!(cwd_of(&url), PathBuf::from("/tmp/a b"));
    }
}
