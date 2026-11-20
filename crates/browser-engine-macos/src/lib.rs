mod default_browser;
mod drag;
mod extensions;
mod frame;
mod popover;
mod screenshot;
mod shell;
// Input-method method names mirror their Objective-C selectors.
#[allow(non_snake_case)]
mod term;
mod updates;
mod vault;
// Delegate method names mirror their Objective-C selectors.
#[allow(non_snake_case)]
mod webkit;

pub use default_browser::make_default_browser;
pub use drag::drag_file;
pub use popover::{Edge, Float, Initial, Ink, Key, Node, Place, Style, Weight, Wrap};
pub use updates::on_check_for_updates;
pub use vault::{authenticate, vault_key};
pub use extensions::{Extensions, SCHEME as EXTENSION_SCHEME};
pub use frame::FrameStyle;
pub use shell::{activate, choose_path, key_window, quit, run, save_path, AppHost, ShellWindow, WindowHost};
pub use term::{cell_size, clipboard_text, set_clipboard_text, Keystroke, Modifiers, MouseButton, TermHost, TermImage, TermInput, TermPaint, TermSpan, TermSurface};
pub use webkit::{LiveTab, MacEngine};
