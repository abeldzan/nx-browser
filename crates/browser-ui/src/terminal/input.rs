//! Keyboard and mouse input as the bytes a TTY expects.
//!
//! Both encoders read the emulator's mode flags (application cursor,
//! mouse reporting, SGR encoding) instead of patching bytes afterwards, the
//! way Zed's terminal mappings do.

use alacritty_terminal::term::TermMode;
use gpui::{KeyDownEvent, Modifiers, MouseButton};

/// Map a GPUI keystroke to the bytes a TTY expects. Returns None for
/// chords the app still owns (`Cmd+Q`, `Cmd+,`) and for copy/paste,
/// which the terminal view handles itself.
pub fn key_bytes(event: &KeyDownEvent, mode: TermMode) -> Option<Vec<u8>> {
    let key = event.keystroke.key.as_str();
    let m = &event.keystroke.modifiers;
    if m.platform {
        if m.shift || m.control || m.alt || m.function {
            return None;
        }
        return match key {
            "left" => Some(vec![0x01]),
            "right" => Some(vec![0x05]),
            _ => None,
        };
    }
    let single = key.chars().count() == 1;
    if m.control && m.shift && single {
        return None;
    }
    // xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    let param = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let bracketed = mode.contains(TermMode::BRACKETED_PASTE);
    let esc = |bytes: &[u8]| {
        let mut out = vec![0x1b];
        out.extend_from_slice(bytes);
        out
    };

    let bytes = match key {
        "up" | "down" | "right" | "left" | "home" | "end" => {
            let letter = match key {
                "up" => 'A',
                "down" => 'B',
                "right" => 'C',
                "left" => 'D',
                "home" => 'H',
                _ => 'F',
            };
            // Alt+arrow moves by word in readline, the macOS convention.
            if m.alt && !m.shift && !m.control && key == "left" {
                return Some(b"\x1bb".to_vec());
            }
            if m.alt && !m.shift && !m.control && key == "right" {
                return Some(b"\x1bf".to_vec());
            }
            if param > 1 {
                format!("\x1b[1;{param}{letter}").into_bytes()
            } else if app_cursor {
                format!("\x1bO{letter}").into_bytes()
            } else {
                format!("\x1b[{letter}").into_bytes()
            }
        }
        "pageup" => tilde(5, param),
        "pagedown" => tilde(6, param),
        "delete" => tilde(3, param),
        "insert" => tilde(2, param),
        "f1" | "f2" | "f3" | "f4" => {
            let letter = match key {
                "f1" => 'P',
                "f2" => 'Q',
                "f3" => 'R',
                _ => 'S',
            };
            if param > 1 {
                format!("\x1b[1;{param}{letter}").into_bytes()
            } else {
                format!("\x1bO{letter}").into_bytes()
            }
        }
        _ if function_key_code(key).is_some() => tilde(function_key_code(key)?, param),
        "enter" if m.shift => super::view::newline_bytes(bracketed),
        "enter" if m.alt => b"\x1b\r".to_vec(),
        "enter" => b"\r".to_vec(),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" if m.alt => b"\x1b\t".to_vec(),
        "tab" => b"\t".to_vec(),
        "backspace" if m.control => vec![0x08],
        "backspace" if m.alt => esc(&[0x7f]),
        "backspace" => vec![0x7f],
        "escape" => vec![0x1b],
        "space" if m.control => vec![0x00],
        "space" if m.alt => esc(b" "),
        "space" => b" ".to_vec(),
        _ if single => {
            let c = key.chars().next()?;
            if m.control {
                let code = control_code(c)?;
                if m.alt {
                    esc(&[code])
                } else {
                    vec![code]
                }
            } else if m.alt {
                let typed = event.keystroke.key_char.as_deref().unwrap_or(key);
                esc(typed.as_bytes())
            } else {
                event.keystroke.key_char.clone()?.into_bytes()
            }
        }
        _ => {
            // Composed or non-ASCII input (é, ü, CJK) arrives as key_char.
            if m.control || m.alt {
                return None;
            }
            let text = event.keystroke.key_char.clone().filter(|t| !t.is_empty())?;
            text.into_bytes()
        }
    };
    Some(bytes)
}

fn tilde(code: u8, param: u8) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[{code};{param}~").into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

/// CSI number for F5 and up (F1-F4 use SS3 letters).
fn function_key_code(key: &str) -> Option<u8> {
    let n: u8 = key.strip_prefix('f')?.parse().ok()?;
    Some(match n {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        13 => 25,
        14 => 26,
        15 => 28,
        16 => 29,
        17 => 31,
        18 => 32,
        19 => 33,
        20 => 34,
        _ => return None,
    })
}

/// The C0 code for Ctrl+char, if the chord has one.
fn control_code(c: char) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    match lower {
        'a'..='z' => Some(lower as u8 - b'a' + 1),
        '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '/' | '7' => Some(0x1f),
        _ => None,
    }
}

/// What the pointer did, for mouse reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    /// Motion with a button held.
    Drag(MouseButton),
    /// Motion with no button.
    Move,
    WheelUp,
    WheelDown,
}

/// Whether the program asked for mouse events of this kind.
pub fn reports_mouse(mode: TermMode, action: MouseAction) -> bool {
    match action {
        MouseAction::Press(_)
        | MouseAction::Release(_)
        | MouseAction::WheelUp
        | MouseAction::WheelDown => mode.intersects(TermMode::MOUSE_MODE),
        MouseAction::Drag(_) => mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
        MouseAction::Move => mode.contains(TermMode::MOUSE_MOTION),
    }
}

/// Encode a mouse event at 0-based `(row, col)`. None when the program
/// did not ask for it or the position does not fit the active encoding.
pub fn mouse_bytes(
    action: MouseAction,
    row: u16,
    col: u16,
    modifiers: &Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if !reports_mouse(mode, action) {
        return None;
    }
    let sgr = mode.contains(TermMode::SGR_MOUSE);
    let button = |button: MouseButton| match button {
        MouseButton::Left => 0u16,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        _ => 0,
    };
    let (mut code, release) = match action {
        MouseAction::Press(b) => (button(b), false),
        // Legacy encodings cannot say which button was released.
        MouseAction::Release(b) => (if sgr { button(b) } else { 3 }, true),
        MouseAction::Drag(b) => (button(b) + 32, false),
        MouseAction::Move => (3 + 32, false),
        MouseAction::WheelUp => (64, false),
        MouseAction::WheelDown => (65, false),
    };
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.control {
        code += 16;
    }
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    if sgr {
        let end = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    let mut out = b"\x1b[M".to_vec();
    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    for value in [u32::from(code) + 32, x + 32, y + 32] {
        if utf8 {
            let c = char::from_u32(value)?;
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        } else if value <= 255 {
            out.push(value as u8);
        } else {
            return None;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke::parse(name).unwrap(),
            is_held: false,
            prefer_character_input: false,
        }
    }

    fn bytes(name: &str) -> Option<Vec<u8>> {
        key_bytes(&key(name), TermMode::empty())
    }

    #[test]
    fn control_keys_are_binary() {
        assert_eq!(bytes("ctrl-c"), Some(vec![3]));
        assert_eq!(bytes("ctrl-d"), Some(vec![4]));
        assert_eq!(bytes("ctrl-["), Some(vec![0x1b]));
        assert_eq!(bytes("ctrl-\\"), Some(vec![0x1c]));
        assert_eq!(bytes("ctrl-space"), Some(vec![0]));
        assert_eq!(bytes("ctrl-alt-b"), Some(vec![0x1b, 2]));
        assert_eq!(bytes("cmd-v"), None);
        assert_eq!(bytes("enter"), Some(b"\r".to_vec()));
        assert_eq!(bytes("backspace"), Some(vec![0x7f]));
        assert_eq!(bytes("shift-tab"), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn shift_enter_inserts_a_newline_without_submitting() {
        assert_eq!(bytes("shift-enter").as_deref(), Some(&b"\x1b\r"[..]));
        assert_eq!(
            key_bytes(&key("shift-enter"), TermMode::BRACKETED_PASTE).as_deref(),
            Some(&b"\x1b[200~\n\x1b[201~"[..])
        );
    }

    #[test]
    fn word_and_line_movement_follow_mac_conventions() {
        assert_eq!(bytes("alt-left").as_deref(), Some(&b"\x1bb"[..]));
        assert_eq!(bytes("alt-right").as_deref(), Some(&b"\x1bf"[..]));
        assert_eq!(bytes("cmd-left"), Some(vec![0x01]));
        assert_eq!(bytes("cmd-right"), Some(vec![0x05]));
    }

    #[test]
    fn cursor_keys_follow_application_mode_and_modifiers() {
        assert_eq!(bytes("up"), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            key_bytes(&key("up"), TermMode::APP_CURSOR),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_bytes(&key("home"), TermMode::APP_CURSOR),
            Some(b"\x1bOH".to_vec())
        );
        assert_eq!(bytes("ctrl-right"), Some(b"\x1b[1;5C".to_vec()));
        assert_eq!(bytes("shift-left"), Some(b"\x1b[1;2D".to_vec()));
        assert_eq!(bytes("ctrl-shift-up"), Some(b"\x1b[1;6A".to_vec()));
        assert_eq!(bytes("pageup"), Some(b"\x1b[5~".to_vec()));
        assert_eq!(bytes("ctrl-delete"), Some(b"\x1b[3;5~".to_vec()));
    }

    #[test]
    fn function_keys_use_xterm_sequences() {
        assert_eq!(bytes("f1"), Some(b"\x1bOP".to_vec()));
        assert_eq!(bytes("f4"), Some(b"\x1bOS".to_vec()));
        assert_eq!(bytes("f5"), Some(b"\x1b[15~".to_vec()));
        assert_eq!(bytes("f12"), Some(b"\x1b[24~".to_vec()));
        assert_eq!(bytes("shift-f5"), Some(b"\x1b[15;2~".to_vec()));
        assert_eq!(bytes("ctrl-f1"), Some(b"\x1b[1;5P".to_vec()));
    }

    #[test]
    fn alt_prefixes_escape_and_keeps_non_ascii_whole() {
        assert_eq!(bytes("alt-b"), Some(b"\x1bb".to_vec()));
        let mut event = key("alt-a");
        event.keystroke.key_char = Some("å".into());
        assert_eq!(
            key_bytes(&event, TermMode::empty()),
            Some("\u{1b}å".as_bytes().to_vec())
        );
        let mut composed = key("a");
        composed.keystroke.key = "é".into();
        composed.keystroke.key_char = Some("é".into());
        assert_eq!(
            key_bytes(&composed, TermMode::empty()),
            Some("é".as_bytes().to_vec())
        );
    }

    #[test]
    fn mouse_reports_need_a_mouse_mode() {
        let mods = Modifiers::default();
        let press = MouseAction::Press(MouseButton::Left);
        assert_eq!(mouse_bytes(press, 0, 0, &mods, TermMode::empty()), None);
        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            mouse_bytes(press, 4, 9, &mods, sgr),
            Some(b"\x1b[<0;10;5M".to_vec())
        );
        assert_eq!(
            mouse_bytes(MouseAction::Release(MouseButton::Left), 4, 9, &mods, sgr),
            Some(b"\x1b[<0;10;5m".to_vec())
        );
        assert_eq!(
            mouse_bytes(MouseAction::WheelUp, 1, 1, &mods, sgr),
            Some(b"\x1b[<64;2;2M".to_vec())
        );
        // Click-only mode never reports motion.
        assert_eq!(
            mouse_bytes(MouseAction::Drag(MouseButton::Left), 1, 1, &mods, sgr),
            None
        );
        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(
            mouse_bytes(MouseAction::Drag(MouseButton::Left), 1, 1, &mods, drag),
            Some(b"\x1b[<32;2;2M".to_vec())
        );
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(
            mouse_bytes(press, 0, 0, &ctrl, sgr),
            Some(b"\x1b[<16;1;1M".to_vec())
        );
    }

    #[test]
    fn legacy_mouse_encoding_has_a_coordinate_limit() {
        let mods = Modifiers::default();
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let press = MouseAction::Press(MouseButton::Left);
        assert_eq!(
            mouse_bytes(press, 0, 0, &mods, mode),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            mouse_bytes(MouseAction::Release(MouseButton::Right), 0, 0, &mods, mode),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
        assert_eq!(mouse_bytes(press, 0, 300, &mods, mode), None);
        let utf8 = mode | TermMode::UTF8_MOUSE;
        assert!(mouse_bytes(press, 0, 300, &mods, utf8).is_some());
    }
}
