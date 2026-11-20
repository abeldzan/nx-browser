//! Terminal tabs: a tab whose URL is `nxbrowser://terminal` shows a shell instead of a web page.

mod agent;
mod emu;
mod input;
mod kitty;
mod pty;
mod view;

use browser_core::Settings;
use browser_engine_macos::TermSurface;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
pub use agent::{AgentState, Status};
pub use view::TerminalView;

pub const TERMINAL_URL: &str = "nxbrowser://terminal";

pub fn is_terminal(url: &str) -> bool {
    url == TERMINAL_URL || url.starts_with("nxbrowser://terminal?")
}

/// The URL a terminal tab is saved under, so a restored tab starts in the same folder.
pub fn terminal_url(cwd: &std::path::Path) -> String {
    format!("{TERMINAL_URL}?cwd={}", cwd.display())
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

thread_local! {
    /// The app's current settings, for terminals and for `paint`'s interface font size.
    static PREFS: RefCell<Option<Settings>> = const { RefCell::new(None) };
    /// The theme's foreground, background and accent (sRGB with alpha), set by the windows.
    static COLORS: std::cell::Cell<[[f64; 4]; 3]> = const { std::cell::Cell::new([[0.9, 0.9, 0.9, 1.], [0.1, 0.1, 0.1, 1.], [0.2, 0.5, 1., 1.]]) };
    /// Live terminals by tab id. App-wide, so a tab torn off into another OS window keeps its shell.
    static TERMINALS: RefCell<HashMap<String, Rc<RefCell<TerminalView>>>> = RefCell::default();
}

/// The app's current settings, kept here for terminals and read by `paint` for the interface font size.
pub(crate) fn prefs() -> Settings {
    PREFS.with_borrow(|prefs| prefs.clone()).unwrap_or_default()
}

/// Hands changed settings to every open terminal (font, size, cursor and scrollback apply right away).
pub fn set_prefs(settings: &Settings) {
    if PREFS.with_borrow(|prefs| prefs.as_ref() == Some(settings)) {
        return;
    }
    PREFS.set(Some(settings.clone()));
    for view in all() {
        view.borrow_mut().apply_prefs(settings);
    }
}

/// The theme's foreground, background and accent, for drawing terminals.
fn colors() -> [[f64; 4]; 3] {
    COLORS.get()
}

/// Repaints every terminal in the theme's foreground, background and accent when they change.
pub fn set_colors(colors: [[f64; 4]; 3]) {
    if COLORS.replace(colors) != colors {
        for view in all() {
            view.borrow().redraw();
        }
    }
}

fn all() -> Vec<Rc<RefCell<TerminalView>>> {
    TERMINALS.with_borrow(|terminals| terminals.values().cloned().collect())
}

/// The tab's terminal, starting its shell on first use.
pub fn terminal_for(tab_id: &str, url: &str) -> Rc<RefCell<TerminalView>> {
    if let Some(view) = get(tab_id) {
        return view;
    }
    let view = Rc::new(RefCell::new(TerminalView::new(tab_id, &cwd_of(url), &prefs())));
    TERMINALS.with_borrow_mut(|terminals| terminals.insert(tab_id.to_string(), view.clone()));
    view
}

/// The tab's terminal screen for the engine to place, starting its shell on first use.
pub fn screen_for(tab_id: &str, url: &str) -> Option<Rc<TermSurface>> {
    terminal_for(tab_id, url).borrow().surface.clone()
}

pub fn get(tab_id: &str) -> Option<Rc<RefCell<TerminalView>>> {
    TERMINALS.with_borrow(|terminals| terminals.get(tab_id).cloned())
}

/// Runs `f` on the tab's terminal, unless it is gone or already in use further up the stack.
fn with<T>(tab_id: &str, f: impl FnOnce(&mut TerminalView) -> T) -> Option<T> {
    let view = get(tab_id)?;
    let mut view = view.try_borrow_mut().ok()?;
    Some(f(&mut view))
}

/// Feeds shell output to the tab's terminal on the main thread.
fn feed(tab_id: &str, bytes: Vec<u8>) {
    if get(tab_id).is_some_and(|view| view.try_borrow_mut().is_err()) {
        // Busy further up the stack: output must not be dropped, so it waits for the next turn.
        let id = tab_id.to_string();
        dispatch2::DispatchQueue::main().exec_async(move || feed(&id, bytes));
        return;
    }
    with(tab_id, |view| view.feed(&bytes));
}

/// The coding agent running in the tab's terminal and its status.
pub fn agent(tab_id: &str) -> Option<AgentState> {
    get(tab_id)?.borrow().agent()
}

/// Ends shells whose tab is gone or now shows a web page (the PTY is killed when the view drops).
pub fn retain(mut keep: impl FnMut(&str) -> bool) {
    TERMINALS.with_borrow_mut(|terminals| terminals.retain(|id, _| keep(id)));
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
