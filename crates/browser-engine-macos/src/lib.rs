mod default_browser;
mod dock;
mod drag;
mod extensions;
mod popover;
mod screenshot;
mod updates;
mod vault;
// Delegate method names mirror their Objective-C selectors.
#[allow(non_snake_case)]
mod webkit;

pub use default_browser::make_default_browser;
pub use dock::install_dock_reopen;
pub use drag::drag_file;
pub use popover::{Edge, Initial, Ink, Key, Node, Style, Weight, Wrap};
pub use updates::on_check_for_updates;
pub use vault::{authenticate, vault_key};
pub use extensions::{Extensions, SCHEME as EXTENSION_SCHEME};
pub use webkit::{LiveTab, MacEngine};
