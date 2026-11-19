use crate::history::{popover_header, INSET};
use crate::native::{rgb, text, title, Ink, Node, Pop, Weight};
use crate::window::Popover;
use crate::ui::{action_button, Kind};
use crate::window::{paint, site_color, BrowserWindow};
use browser_extensions::store_id;
use gpui::{div, img, prelude::*, px, ClickEvent, Context, FontWeight, SharedString};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::icons::Icons;
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::traits::control_sized::ControlSized;

/// The install field in the extensions popover.
pub(crate) const EXT_FIELD: u32 = 0;

const STORE_URL: &str = "https://chromewebstore.google.com/category/extensions";

impl BrowserWindow {
    /// Extension manager, dropped from the title bar's puzzle button.
    pub(crate) fn extensions_popover(&self, pop: &mut Pop) -> Node {
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
                                Node::Button { title: "Add to NxBrowser".into(), action: add, primary: true },
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
                                    Node::Button { title: "Deny".into(), action: deny, primary: false },
                                    Node::Button { title: "Allow".into(), action: allow, primary: true },
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
                    Node::Text { text: "No extensions yet".into(), size: 13., weight: Weight::Medium, ink: Ink::Primary, wrap: Some(352.), shrink: false },
                    Node::Text {
                        text: "Add one from the Chrome Web Store, or load an unpacked folder.".into(),
                        size: 12.,
                        weight: Weight::Regular,
                        ink: Ink::Secondary,
                        wrap: Some(352.),
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
                        Node::Field { field: EXT_FIELD, placeholder: "Chrome Web Store link or extension ID".into(), submit: install },
                        Node::Button { title: "Install".into(), action: install, primary: true },
                    ],
                },
                Node::Row {
                    gap: 16.,
                    children: vec![
                        Node::Link { title: "Browse the store".into(), action: browse },
                        Node::Link { title: "Load unpacked…".into(), action: unpacked },
                        Node::Spacer,
                        Node::Link { title: "Reload all".into(), action: reload },
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
    pub(crate) fn ext_logs_modal(&self, id: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let name = self.ext_meta.get(id).map(|meta| meta.name.clone()).filter(|name| !name.is_empty()).unwrap_or_else(|| id.to_string());
        let lines = self.engine.extensions().errors(id);
        let text = lines.join("\n");
        let mut list = v_stack().w_full().gap(px(6.)).p(px(INSET)).font_family("Menlo").text_size(paint.text(11.));
        if lines.is_empty() {
            list = list.child(div().py(px(24.)).text_center().text_color(paint.muted).child("No logs"));
        }
        for line in lines {
            list = list.child(div().w_full().pb(px(6.)).border_b_1().border_color(paint.border).child(line));
        }
        let card = v_stack()
            .w(px(640.))
            .h(px(480.))
            .max_w_full()
            .max_h_full()
            .child(popover_header(
                format!("Logs · {name}"),
                h_stack()
                    .child(action_button("ext-logs-copy", "Copy", Kind::Secondary, !text.is_empty(), cx, move |this, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                        this.status = "Copied logs".into();
                    }))
                    .child(icon_button("ext-logs-close", Icons::cross_2()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.close_ext_logs(cx);
                    }))),
                paint,
            ))
            .child(div().flex_1().min_h(px(0.)).overflow_hidden().child(scroll_area("ext-logs-list").vertical().full_height(true).child(list)));
        self.modal_shell("ext-logs-modal", card, Self::close_ext_logs, cx)
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

    /// Buttons for pinned extensions in the title bar, with the badge text the extension sets.
    pub(crate) fn pinned_extensions(&self, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let (store, tab) = (self.active_store(), self.active_tab_id());
        let pinned = self.model.lock().extensions.iter().filter(|extension| extension.enabled && extension.pinned).cloned().collect::<Vec<_>>();
        let mut row = h_stack().items_center().gap(px(2.));
        if self.store_page_id().is_some() {
            row = row.child(
                div()
                    .id("titlebar-add-extension")
                    .h(px(22.))
                    .px(px(10.))
                    .mr(px(4.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(paint.accent)
                    .text_color(paint.bg)
                    .text_size(paint.text(12.))
                    .cursor_pointer()
                    .child("Add to NxBrowser")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(id) = this.store_page_id() {
                            this.install_from_store(id);
                        }
                        cx.notify();
                    })),
            );
        }
        for extension in pinned {
            let id = extension.id.clone();
            let icon = self.ext_meta.get(&id).and_then(|meta| meta.icon.clone());
            let badge = self.engine.extensions().badge(&id, &store, tab.as_deref());
            row = row.child(
                div()
                    .id(SharedString::from(format!("pinned-{id}")))
                    .relative()
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|style| style.bg(paint.secondary))
                    .child(extension_icon(&extension.name, icon, 16.))
                    .when(!badge.is_empty(), |button| {
                        button.child(
                            div()
                                .absolute()
                                .bottom(px(1.))
                                .right(px(0.))
                                .px(px(3.))
                                .rounded(px(4.))
                                .bg(paint.accent)
                                .text_color(paint.bg)
                                .text_size(px(8.))
                                .child(badge),
                        )
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.open_extension(&id, f32::from(event.position().x), cx))),
            );
        }
        row
    }
}

/// `extension_icon` for a native popover.
fn extension_image(name: &str, icon: Option<std::path::PathBuf>, size: f64) -> Node {
    let letter = name.chars().next().unwrap_or('?').to_uppercase().to_string();
    Node::Image { path: icon, letter, rgb: rgb(site_color(name)), size, radius: size / 4. }
}

/// An extension's manifest icon, or a lettered tile when it has none.
pub(crate) fn extension_icon(name: &str, icon: Option<std::path::PathBuf>, size: f32) -> gpui::AnyElement {
    let letter = name.chars().next().unwrap_or('?').to_uppercase().to_string();
    let color = site_color(name);
    let tile = move || {
        div()
            .size(px(size))
            .flex_none()
            .rounded(px(size / 4.))
            .flex()
            .items_center()
            .justify_center()
            .bg(color)
            .text_color(gpui::rgb(0xFFFFFF))
            .text_size(px(size * 0.55))
            .font_weight(FontWeight::SEMIBOLD)
            .child(letter.clone())
            .into_any_element()
    };
    match icon {
        Some(path) => img(path).size(px(size)).flex_none().with_fallback(tile).into_any_element(),
        None => tile(),
    }
}
