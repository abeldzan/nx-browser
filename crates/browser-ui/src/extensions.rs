use crate::native::{button, empty, field, rgb, sheet, text, title, Ink, Key, Node, Pop, Style, Weight, Wrap};
use crate::window::Popover;
use crate::window::{site_color, BrowserWindow};
use browser_extensions::store_id;
use gpui::Context;

/// The install field in the extensions popover.
pub(crate) const EXT_FIELD: u32 = 0;

const STORE_URL: &str = "https://chromewebstore.google.com/category/extensions";

impl BrowserWindow {
    /// Extension manager, a sheet opened from the dock's puzzle button.
    pub(crate) fn extensions_sheet(&self, pop: &mut Pop) -> Node {
        let extensions = self.model.lock().extensions.clone();
        let on = extensions.iter().filter(|extension| extension.enabled).count();
        let summary = match extensions.len() {
            0 => "Nothing installed".to_string(),
            total => format!("{total} installed · {on} on"),
        };
        let mut children = vec![Node::Column {
            gap: 2.,
            pad: (16., 12.),
            children: vec![text("Extensions", 15., Weight::Semibold, Ink::Primary), text(summary, 12., Weight::Regular, Ink::Secondary)],
        }];
        if let Some(id) = self.store_page_id() {
            let add = pop.act(move |this, _| this.install_from_store(id.clone()));
            children.push(Node::Column {
                gap: 0.,
                pad: (12., 0.),
                children: vec![Node::Card {
                    tint: Ink::Accent,
                    child: Box::new(Node::Column {
                        gap: 0.,
                        pad: (10., 10.),
                        children: vec![Node::Row {
                            gap: 10.,
                            children: vec![
                                Node::Glyph { name: "puzzlepiece.extension", size: 14., ink: Ink::Accent },
                                title("This page is a Chrome Web Store listing.", 12., Weight::Regular, Ink::Primary),
                                Node::Spacer,
                                button("Add to NxBrowser", add, Style::Primary, Key::None),
                            ],
                        }],
                    }),
                }],
            });
        }
        for (prompt, id, request) in self.ext_prompts.clone() {
            let name = extensions.iter().find(|extension| extension.id == id).map_or(id.clone(), |extension| extension.name.clone());
            let icon = self.ext_meta.get(&id).and_then(|meta| meta.icon.clone());
            let (deny, allow) = (pop.act(move |this, _| this.answer_prompt(prompt, false)), pop.act(move |this, _| this.answer_prompt(prompt, true)));
            children.push(Node::Column {
                gap: 0.,
                pad: (12., 4.),
                children: vec![Node::Card {
                    tint: Ink::Secondary,
                    child: Box::new(Node::Column {
                        gap: 10.,
                        pad: (12., 12.),
                        children: vec![
                            Node::Row { gap: 10., children: vec![extension_image(&name, icon, 20.), title(format!("{name} wants {request}"), 12., Weight::Regular, Ink::Primary)] },
                            Node::Row {
                                gap: 8.,
                                children: vec![
                                    Node::Spacer,
                                    button("Deny", deny, Style::Plain, Key::None),
                                    button("Allow", allow, Style::Primary, Key::None),
                                ],
                            },
                        ],
                    }),
                }],
            });
        }
        let mut rows = Vec::new();
        for id in &self.installing {
            rows.push(Node::Item {
                height: 44.,
                click: None,
                drag: None,
                children: vec![
                    Node::Faded(Box::new(extension_image(id, None, 28.))),
                    Node::Column { gap: 2., pad: (0., 0.), children: vec![text("Installing…", 13., Weight::Regular, Ink::Primary), title(id.clone(), 11., Weight::Regular, Ink::Secondary)] },
                ],
            });
        }
        for extension in &extensions {
            rows.push(self.extension_row(extension, pop));
        }
        if rows.is_empty() {
            children.push(Node::Column {
                gap: 6.,
                pad: (24., 20.),
                children: vec![
                    Node::Glyph { name: "puzzlepiece.extension", size: 26., ink: Ink::Secondary },
                    Node::Text { text: "No extensions yet".into(), size: 13., weight: Weight::Medium, ink: Ink::Primary, wrap: Wrap::Center, shrink: false },
                    Node::Text {
                        text: "Add one from the Chrome Web Store, or load an unpacked folder.".into(),
                        size: 12.,
                        weight: Weight::Regular,
                        ink: Ink::Secondary,
                        wrap: Wrap::Center,
                        shrink: false,
                    },
                ],
            });
        } else {
            children.push(Node::Scroll { max: 360., child: Box::new(Node::Column { gap: 2., pad: (6., 0.), children: rows }) });
        }
        let install = pop.act(|this, cx| {
            let raw = this.popover_texts.get(&EXT_FIELD).cloned().unwrap_or_default();
            this.install_from_input(&raw, cx);
        });
        let browse = pop.act(|this, _| {
            this.popover = None;
            this.with_model(|model| { let _ = model.create_tab(STORE_URL); });
        });
        let unpacked = pop.act(|this, cx| this.load_unpacked(cx));
        let done = pop.act(|this, cx| {
            this.popover = None;
            cx.notify();
        });
        let reload = pop.act(|this, _| {
            this.reload_extensions();
            this.status = "Extensions reloaded".into();
        });
        children.push(Node::Separator);
        children.push(Node::Column {
            gap: 10.,
            pad: (12., 12.),
            children: vec![
                Node::Row {
                    gap: 8.,
                    children: vec![
                        field(EXT_FIELD, "", "Chrome Web Store link or extension ID", install),
                        button("Install", install, Style::Primary, Key::None),
                    ],
                },
                Node::Row {
                    gap: 16.,
                    children: vec![
                        Node::Link { title: "Browse the store".into(), action: browse },
                        Node::Link { title: "Load unpacked…".into(), action: unpacked },
                        Node::Link { title: "Reload all".into(), action: reload },
                        Node::Spacer,
                        button("Done", done, Style::Plain, Key::Escape),
                    ],
                },
            ],
        });
        Node::Column { gap: 0., pad: (0., 0.), children }
    }

    /// One installed extension: click to open it; pin, options, logs, remove and on/off at the end.
    fn extension_row(&self, extension: &browser_core::ExtensionRecord, pop: &mut Pop) -> Node {
        let id = extension.id.clone();
        let (enabled, pinned) = (extension.enabled, extension.pinned);
        let meta = self.ext_meta.get(&id).cloned().unwrap_or_default();
        let errors = if enabled { self.engine.extensions().errors(&id) } else { Vec::new() };
        let has_options = enabled && self.engine.extensions().options_url(&id, &self.active_store()).is_some();
        let source = match extension.source.as_str() {
            "store" => "Chrome Web Store",
            "unpacked" => "Unpacked",
            _ => "Example",
        };
        // The manifest is current; the stored name can predate an update.
        let name = if meta.name.is_empty() { extension.name.clone() } else { meta.name.clone() };
        let detail = if meta.version.is_empty() { source.to_string() } else { format!("{} · {source}", meta.version) };
        let detail = match errors.first() {
            Some(first) => title(if errors.len() > 1 { format!("{} issues · {first}", errors.len()) } else { first.clone() }, 11., Weight::Regular, Ink::Danger),
            None => title(detail, 11., Weight::Regular, Ink::Secondary),
        };
        let icon = extension_image(&name, meta.icon, 28.);
        // Secondary actions stay out of the way until the row is hovered; a pinned pin always shows.
        let mut actions = Vec::new();
        if has_options {
            let id = id.clone();
            actions.push(Node::Tool {
                symbol: "gearshape",
                tip: "Options",
                action: pop.act(move |this, cx| {
                    this.popover = None;
                    this.open_extension_options(&id, cx);
                }),
                on: false,
            });
        }
        let logs = id.clone();
        actions.push(Node::Tool {
            symbol: "list.bullet.rectangle",
            tip: "Logs",
            action: pop.act(move |this, _| {
                this.popover = None;
                this.ext_logs = Some(logs.clone());
            }),
            on: false,
        });
        let remove = id.clone();
        actions.push(Node::Tool { symbol: "trash", tip: "Remove", action: pop.act(move |this, _| this.remove_extension(&remove)), on: false });
        let mut children = vec![
            if enabled { icon } else { Node::Faded(Box::new(icon)) },
            Node::Column { gap: 2., pad: (0., 0.), children: vec![title(name, 13., Weight::Medium, if enabled { Ink::Primary } else { Ink::Secondary }), detail] },
            Node::Hover(true, Box::new(Node::Row { gap: 0., children: actions })),
        ];
        if enabled {
            let pin_id = id.clone();
            let pin = Node::Tool {
                symbol: if pinned { "pin.fill" } else { "pin" },
                tip: if pinned { "Unpin from the title bar" } else { "Pin to the title bar" },
                action: pop.act(move |this, _| {
                    let _ = this.model.lock().set_extension_pinned(&pin_id, !pinned);
                }),
                on: pinned,
            };
            children.push(if pinned { pin } else { Node::Hover(true, Box::new(pin)) });
        }
        let toggle_id = id.clone();
        children.push(Node::Switch {
            on: enabled,
            action: pop.act(move |this, _| {
                let _ = this.model.lock().set_extension_enabled(&toggle_id, !enabled);
                this.sync_extension(&toggle_id);
            }),
        });
        let open = id.clone();
        Node::Item {
            height: 44.,
            click: enabled.then(|| {
                pop.act(move |this, cx| {
                    this.popover = None;
                    let at = this.popover_anchors.get(&Popover::Extensions).map_or(0., |bounds| f32::from(bounds.center().x));
                    this.open_extension(&open, at, cx);
                })
            }),
            drag: None,
            children,
        }
    }

    /// An extension's errors, one per line, with a button that copies them all.
    // shortcut: WebKit exposes only the error list (runtime errors and console.error), not console.log; capture
    // console output in the polyfill if full logs are needed.
    pub(crate) fn logs_sheet(&self, id: &str, pop: &mut Pop) -> Node {
        let name = self.ext_meta.get(id).map(|meta| meta.name.clone()).filter(|name| !name.is_empty()).unwrap_or_else(|| id.to_string());
        let lines = self.engine.extensions().errors(id);
        let text_ = lines.join("\n");
        let list = if lines.is_empty() {
            empty(None, "No logs")
        } else {
            let mut rows = Vec::new();
            for line in lines {
                rows.push(Node::Text { text: line, size: 11., weight: Weight::Mono, ink: Ink::Primary, wrap: Wrap::Left, shrink: false });
                rows.push(Node::Separator);
            }
            Node::Scroll { max: 380., child: Box::new(Node::Column { gap: 6., pad: (0., 0.), children: rows }) }
        };
        let copy = pop.act(move |this, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text_.clone()));
            this.status = "Copied logs".into();
        });
        let done = pop.act(|this, cx| this.close_ext_logs(cx));
        sheet(format!("Logs · {name}"), vec![list], vec![button("Copy", copy, Style::Plain, Key::None), button("Done", done, Style::Primary, Key::Escape)])
    }

    fn close_ext_logs(&mut self, cx: &mut Context<Self>) {
        self.ext_logs = None;
        cx.notify();
    }

    fn answer_prompt(&mut self, prompt: u64, allow: bool) {
        self.ext_prompts.retain(|(pending, _, _)| *pending != prompt);
        self.engine.extensions().answer_prompt(prompt, allow);
    }

    /// The store id when the active page is a Chrome Web Store listing not yet installed.
    pub(crate) fn store_page_id(&self) -> Option<String> {
        let model = self.model.lock();
        let url = model.active_tab()?.url.clone();
        let id = store_id(&url).filter(|_| url.contains("://"))?;
        let installed = model.extensions.iter().any(|extension| extension.id == id) || self.installing.contains(&id);
        (!installed).then_some(id)
    }
}

/// `extension_icon` for a native popover.
pub(crate) fn extension_image(name: &str, icon: Option<std::path::PathBuf>, size: f64) -> Node {
    let letter = name.chars().next().unwrap_or('?').to_uppercase().to_string();
    Node::Image { path: icon, letter, rgb: rgb(site_color(name)), size, radius: size / 4. }
}
