use crate::native::{button_if, heading, setting, Key, Node, Pop, Style};
use crate::window::BrowserWindow;
use browser_core::Settings;
use browser_engine::{Binding, Shortcut, SHORTCUTS};

/// The keys `binding` runs on: the user's own, else its default; empty when unbound.
pub(crate) fn keys<'a>(settings: &'a Settings, binding: &'static Binding) -> &'a str {
    settings.shortcuts.get(binding.id).map_or(binding.keys, String::as_str)
}

/// Every bound shortcut, as the engine's key monitor matches them.
pub(crate) fn keymap(settings: &Settings) -> Vec<(String, Shortcut)> {
    SHORTCUTS.iter().map(|binding| (keys(settings, binding).to_string(), binding.action)).filter(|(keys, _)| !keys.is_empty()).collect()
}

/// Modifiers in the key monitor's spelling and order.
const MODIFIERS: [&str; 5] = ["fn-", "ctrl-", "alt-", "cmd-", "shift-"];

/// `cmd-shift-t` as which of `MODIFIERS` it holds and its key; None without a key.
fn parse(keys: &str) -> Option<([bool; 5], &str)> {
    let (mut on, mut rest) = ([false; 5], keys);
    while let Some(at) = MODIFIERS.iter().position(|modifier| rest.len() > modifier.len() && rest.starts_with(modifier)) {
        on[at] = true;
        rest = &rest[MODIFIERS[at].len()..];
    }
    (!rest.is_empty()).then_some((on, rest))
}

/// `keys` in the key monitor's spelling: modifiers in `MODIFIERS` order, then the key.
fn canonical(keys: &str) -> Option<String> {
    let (on, key) = parse(keys)?;
    Some(MODIFIERS.iter().zip(on).filter_map(|(modifier, on)| on.then_some(*modifier)).chain([key]).collect())
}

/// `cmd-shift-t` as macOS writes it: ⇧⌘T.
pub(crate) fn label(keys: &str) -> String {
    let Some(([_, control, alt, platform, shift], key)) = parse(keys) else {
        return String::new();
    };
    let mut out = [(control, "⌃"), (alt, "⌥"), (shift, "⇧"), (platform, "⌘")]
        .into_iter()
        .filter_map(|(on, glyph)| on.then_some(glyph))
        .collect::<String>();
    out.push_str(&match key {
        "left" => "←".into(),
        "right" => "→".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "tab" => "⇥".into(),
        "enter" => "↩".into(),
        "backspace" => "⌫".into(),
        "space" => "Space".into(),
        key => key.to_uppercase(),
    });
    out
}

/// The live keys of `action`, for hints; empty when unbound.
pub(crate) fn hint(settings: &Settings, action: Shortcut) -> String {
    SHORTCUTS.iter().find(|binding| binding.action == action).map(|binding| label(keys(settings, binding))).unwrap_or_default()
}

/// Whether settings search `query` (already lowercased) names `binding` by its label, group or default keys.
pub(crate) fn matches(binding: &Binding, query: &str) -> bool {
    query.is_empty()
        || binding.label.to_lowercase().contains(query)
        || binding.group.to_lowercase().contains(query)
        || label(binding.keys).to_lowercase().contains(query)
}

/// Stores `keys` for `binding`, dropping the override when it matches the default.
fn set_keys(settings: &mut Settings, binding: &'static Binding, keys: String) {
    if keys == binding.keys {
        settings.shortcuts.remove(binding.id);
    } else {
        settings.shortcuts.insert(binding.id.into(), keys);
    }
}

impl BrowserWindow {
    pub(crate) fn shortcuts_settings(&self, settings: &Settings, query: &str, pop: &mut Pop) -> Vec<Node> {
        let reset_all = pop.act(|this| this.set_pref(|s| s.shortcuts.clear()));
        let mut page = vec![setting(
            "Change a shortcut",
            Some("Click a shortcut, then press the new keys. Delete clears it; Esc or click again to cancel."),
            button_if(!settings.shortcuts.is_empty(), "Reset all", reset_all),
        )];
        // The section can match by its own keywords; then list every binding rather than none.
        let query = if SHORTCUTS.iter().any(|binding| matches(binding, query)) { query } else { "" };
        let mut group = "";
        for binding in SHORTCUTS.iter().filter(|binding| matches(binding, query)) {
            if binding.group != group {
                group = binding.group;
                page.push(heading(group));
            }
            let keys = keys(settings, binding);
            let recording = self.recording == Some(binding.id);
            let shown = if recording { "Press keys… (Esc)".to_string() } else if keys.is_empty() { "None".into() } else { label(keys) };
            let record = pop.act(move |this| {
                this.settings_note = None;
                if this.recording == Some(binding.id) {
                    this.end_recording();
                } else {
                    this.recording = Some(binding.id);
                    // Every key goes to the recorder, shortcuts included, until it has one.
                    this.engine.record_keys(true);
                }
            });
            let mut controls = Vec::new();
            if settings.shortcuts.contains_key(binding.id) {
                let reset = pop.act(move |this| {
                    this.set_pref(|s| {
                        s.shortcuts.remove(binding.id);
                    })
                });
                controls.push(Node::Tool { symbol: "arrow.uturn.backward", tip: "Reset to default", action: reset, on: false });
            }
            controls.push(Node::Button { title: shown, action: record, style: if recording { Style::Primary } else { Style::Plain }, key: Key::None, enabled: true });
            page.push(setting(binding.label, None, Node::Row { gap: 6., children: controls }));
        }
        page
    }

    /// Stops waiting for keys and turns the app shortcuts back on.
    pub(crate) fn end_recording(&mut self) {
        if self.recording.take().is_some() {
            self.engine.record_keys(false);
            self.apply_settings();
        }
    }

    /// Keys pressed while recording, as `EngineEvent::RecordedKey` reports them.
    pub(crate) fn record_key(&mut self, keys: &str) {
        let Some(binding) = self.recording.and_then(|id| SHORTCUTS.iter().find(|binding| binding.id == id)) else {
            return;
        };
        match keys {
            "escape" => {}
            "backspace" => {
                self.recording = None;
                self.set_pref(|s| set_keys(s, binding, String::new()));
            }
            // Without ⌘ or ⌃ it would swallow typing; the key monitor never matches such keys.
            "" => {
                self.settings_note = Some("Use ⌘ or ⌃ with a key".into());
                return self.notify();
            }
            keys => {
                // In the spelling the defaults and the key monitor use.
                let keys = canonical(keys).unwrap_or_else(|| keys.to_string());
                let taken = {
                    let settings = &self.model.lock().settings;
                    SHORTCUTS.iter().find(|other| other.id != binding.id && self::keys(settings, other) == keys)
                };
                self.recording = None;
                self.set_pref(|s| {
                    if let Some(other) = taken {
                        set_keys(s, other, String::new());
                    }
                    set_keys(s, binding, keys);
                });
                self.settings_note = taken.map(|other| format!("Removed from “{}”", other.label));
            }
        }
        self.end_recording();
        self.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_never_collide() {
        let mut seen = std::collections::HashSet::new();
        for binding in SHORTCUTS.iter().filter(|binding| !binding.keys.is_empty()) {
            // The key monitor matches one spelling, so defaults must already be in it.
            assert_eq!(canonical(binding.keys).as_deref(), Some(binding.keys));
            assert!(seen.insert(binding.keys), "{} bound twice", binding.keys);
        }
    }

    #[test]
    fn overrides_rebind_and_unbind() {
        let new_tab = SHORTCUTS.iter().find(|binding| binding.id == "new_tab").unwrap();
        let close = SHORTCUTS.iter().find(|binding| binding.id == "close_tab").unwrap();
        let mut settings = Settings::default();
        set_keys(&mut settings, new_tab, "ctrl-cmd-t".into());
        set_keys(&mut settings, close, String::new());
        let map = keymap(&settings);
        assert!(map.contains(&("ctrl-cmd-t".into(), Shortcut::NewTab)));
        assert!(!map.iter().any(|(keys, action)| keys == "cmd-t" || *action == Shortcut::CloseTab));
        assert_eq!(hint(&settings, Shortcut::NewTab), "⌃⌘T");
        assert_eq!(label("cmd-shift-["), "⇧⌘[");
        assert_eq!(label("cmd--"), "⌘-");
        assert_eq!(canonical("shift-cmd-t").as_deref(), Some("cmd-shift-t"));
        // Setting a default back drops the override.
        set_keys(&mut settings, new_tab, "cmd-t".into());
        assert!(!settings.shortcuts.contains_key("new_tab"));
    }
}
