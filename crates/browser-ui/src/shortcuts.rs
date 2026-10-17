use crate::settings::row;
use crate::ui::{action_button, Kind};
use crate::window::{BrowserWindow, Paint};
use browser_core::Settings;
use browser_engine::WebEngine;
use browser_engine::{Binding, Shortcut, SHORTCUTS};
use gpui::{div, prelude::*, px, ClickEvent, Context, FontWeight, KeyDownEvent, Keystroke, SharedString};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::icons::Icons;
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::traits::control_sized::ControlSized;

/// The keys `binding` runs on: the user's own, else its default; empty when unbound.
pub(crate) fn keys<'a>(settings: &'a Settings, binding: &'static Binding) -> &'a str {
    settings.shortcuts.get(binding.id).map_or(binding.keys, String::as_str)
}

/// Every bound shortcut, as the engine's key monitor matches them.
pub(crate) fn keymap(settings: &Settings) -> Vec<(String, Shortcut)> {
    SHORTCUTS.iter().map(|binding| (keys(settings, binding).to_string(), binding.action)).filter(|(keys, _)| !keys.is_empty()).collect()
}

/// `cmd-shift-t` as macOS writes it: ⇧⌘T.
pub(crate) fn label(keys: &str) -> String {
    let Ok(keystroke) = Keystroke::parse(keys) else {
        return String::new();
    };
    let m = keystroke.modifiers;
    let mut out = [(m.control, "⌃"), (m.alt, "⌥"), (m.shift, "⇧"), (m.platform, "⌘")]
        .into_iter()
        .filter_map(|(on, glyph)| on.then_some(glyph))
        .collect::<String>();
    out.push_str(&match keystroke.key.as_str() {
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
    pub(crate) fn shortcuts_settings(&self, settings: &Settings, query: &str, paint: Paint, cx: &mut Context<Self>) -> gpui::Div {
        let mut page = v_stack()
            .w_full()
            .track_focus(&self.shortcut_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.record_key(event, cx)))
            .child(row(
                "Change a shortcut",
                Some("Click a shortcut, then press the new keys. Delete clears it; Esc or click again to cancel."),
                action_button("shortcuts-reset", "Reset all", Kind::Secondary, !settings.shortcuts.is_empty(), cx, |this, cx| {
                    this.set_pref(cx, |s| s.shortcuts.clear());
                }),
                paint,
            ));
        // The section can match by its own keywords; then list every binding rather than none.
        let query = if SHORTCUTS.iter().any(|binding| matches(binding, query)) { query } else { "" };
        let mut group = "";
        for binding in SHORTCUTS.iter().filter(|binding| matches(binding, query)) {
            if binding.group != group {
                group = binding.group;
                page = page.child(div().pt(px(18.)).pb(px(2.)).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(paint.muted).child(group));
            }
            let keys = keys(settings, binding);
            let recording = self.recording == Some(binding.id);
            let shown = if recording { "Press keys… (Esc)".to_string() } else if keys.is_empty() { "None".into() } else { label(keys) };
            let pill = div()
                .id(SharedString::from(format!("shortcut-{}", binding.id)))
                .h(px(28.))
                .min_w(px(96.))
                .px(px(10.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.))
                .cursor_pointer()
                .border_1()
                .border_color(if recording { paint.accent } else { paint.border })
                .bg(if recording { paint.accent_bg } else { gpui::transparent_black() })
                .text_color(if keys.is_empty() && !recording { paint.muted } else { paint.fg })
                .child(shown)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.settings_note = None;
                    if this.recording == Some(binding.id) {
                        this.end_recording();
                    } else {
                        this.recording = Some(binding.id);
                        this.engine.set_shortcuts(Vec::new());
                        // Take key focus from the page, or Escape never reaches the recorder.
                        this.engine.focus_chrome();
                        window.focus(&this.shortcut_focus, cx);
                    }
                    cx.notify();
                }));
            let cancel = recording.then(|| {
                icon_button(SharedString::from(format!("shortcut-cancel-{}", binding.id)), Icons::cross_2())
                    .small()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.end_recording();
                        cx.notify();
                    }))
            });
            let reset = settings.shortcuts.contains_key(binding.id).then(|| {
                icon_button(SharedString::from(format!("shortcut-reset-{}", binding.id)), Icons::reset())
                    .small()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_pref(cx, |s| { s.shortcuts.remove(binding.id); })))
            });
            page = page.child(row(binding.label, None, h_stack().gap(px(6.)).items_center().children(reset).children(cancel).child(pill), paint));
        }
        page
    }

    /// Stops waiting for keys and turns the app shortcuts back on.
    pub(crate) fn end_recording(&mut self) {
        if self.recording.take().is_some() {
            self.apply_settings();
        }
    }

    fn record_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(binding) = self.recording.and_then(|id| SHORTCUTS.iter().find(|binding| binding.id == id)) else {
            return;
        };
        // Escape here cancels the recording, not the settings dialog.
        cx.stop_propagation();
        let mut keystroke = event.keystroke.clone();
        let m = keystroke.modifiers;
        match keystroke.key.as_str() {
            "escape" => {}
            "backspace" | "delete" if !m.modified() => {
                self.recording = None;
                self.set_pref(cx, |s| set_keys(s, binding, String::new()));
            }
            // Without ⌘ or ⌃ it would swallow typing; the key monitor never matches such keys.
            _ if !m.control && !m.platform => {
                self.settings_note = Some("Use ⌘ or ⌃ with a key".into());
                cx.notify();
                return;
            }
            _ => {
                // macOS flags arrows and F-keys as fn; the key monitor ignores it too.
                keystroke.modifiers.function = false;
                let keys = keystroke.unparse();
                let taken = {
                    let settings = &self.model.lock().settings;
                    SHORTCUTS.iter().find(|other| other.id != binding.id && self::keys(settings, other) == keys)
                };
                self.recording = None;
                self.set_pref(cx, |s| {
                    if let Some(other) = taken {
                        set_keys(s, other, String::new());
                    }
                    set_keys(s, binding, keys);
                });
                self.settings_note = taken.map(|other| format!("Removed from “{}”", other.label).into());
            }
        }
        self.end_recording();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_never_collide() {
        let mut seen = std::collections::HashSet::new();
        for binding in SHORTCUTS.iter().filter(|binding| !binding.keys.is_empty()) {
            let parsed = Keystroke::parse(binding.keys).unwrap_or_else(|_| panic!("{} does not parse", binding.keys));
            // The key monitor matches gpui's own spelling, so defaults must already be in it.
            assert_eq!(parsed.unparse(), binding.keys);
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
        // Setting a default back drops the override.
        set_keys(&mut settings, new_tab, "cmd-t".into());
        assert!(!settings.shortcuts.contains_key("new_tab"));
    }
}
