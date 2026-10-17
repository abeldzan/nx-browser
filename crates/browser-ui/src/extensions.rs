use crate::settings::{measure_overlay, toggle};
use crate::ui::{action_button, Kind};
use crate::window::{paint, site_color, BrowserWindow, Paint};
use browser_extensions::store_id;
use gpui::{div, img, prelude::*, px, ClickEvent, Context, FontWeight, SharedString};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::elements::text_field::text_field;
use gpuikit::icons::Icons;
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::theme::{ActiveTheme, Themeable};
use gpuikit::traits::control_sized::ControlSized;

const STORE_URL: &str = "https://chromewebstore.google.com/category/extensions";

impl BrowserWindow {
    /// Extension manager, dropped from the title bar's puzzle button.
    pub(crate) fn extensions_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let extensions = self.model.lock().extensions.clone();
        let on = extensions.iter().filter(|extension| extension.enabled).count();
        let summary = match extensions.len() {
            0 => "Nothing installed".to_string(),
            total => format!("{total} installed · {on} on"),
        };
        let mut list = v_stack().w_full().gap(px(2.));
        for id in &self.installing {
            list = list.child(
                h_stack()
                    .gap(px(12.))
                    .px(px(10.))
                    .py(px(8.))
                    .items_center()
                    .child(div().size(px(28.)).flex_none().rounded(px(7.)).bg(paint.secondary))
                    .child(v_stack().gap(px(2.)).child("Installing…").child(div().text_size(px(11.)).text_color(paint.muted).child(id.clone()))),
            );
        }
        if extensions.is_empty() && self.installing.is_empty() {
            list = list.child(
                v_stack()
                    .items_center()
                    .gap(px(6.))
                    .py(px(28.))
                    .px(px(24.))
                    .child(gpui::svg().path(crate::PUZZLE_ICON).size(px(28.)).text_color(paint.muted))
                    .child(div().mt(px(4.)).font_weight(FontWeight::MEDIUM).child("No extensions yet"))
                    .child(div().text_size(px(12.)).text_color(paint.muted).text_center().child("Add one from the Chrome Web Store, or load an unpacked folder.")),
            );
        }
        for extension in &extensions {
            list = list.child(self.extension_row(extension, paint, cx));
        }
        // The scroll area needs a definite height; size it to the rows, capped so long lists scroll.
        let rows = extensions.len().max(1) + self.installing.len();
        let extra = if self.store_page_id().is_some() { 60. } else { 0. } + self.ext_prompts.len() as f32 * 104.;
        let empty = if extensions.is_empty() && self.installing.is_empty() { 120. } else { 0. };
        let height = (172. + rows as f32 * 54. + extra + empty).min(600.);
        v_stack()
            .id("extensions-popover")
            .absolute()
            .top(px(self.titlebar_h() + 6.))
            .right(px(8.))
            .w(px(400.))
            .h(px(height))
            .rounded(px(12.))
            .overflow_hidden()
            .bg(paint.surface)
            .border_1()
            .border_color(paint.border)
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(|this, event, _, cx| this.dismiss_popover(event, cx)))
            .child(measure_overlay(12., cx))
            .child(
                h_stack()
                    .w_full()
                    .px(px(16.))
                    .pt(px(14.))
                    .pb(px(10.))
                    .items_center()
                    .justify_between()
                    .child(
                        v_stack()
                            .gap(px(2.))
                            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Extensions"))
                            .child(div().text_size(px(12.)).text_color(paint.muted).child(summary)),
                    )
                    .child(icon_button("extensions-close", Icons::cross_2()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.popover = None;
                        cx.notify();
                    }))),
            )
            .when_some(self.store_page_id(), |popover, id| popover.child(self.store_banner(id, paint, cx)))
            .children(self.ext_prompts.clone().into_iter().map(|prompt| self.prompt_row(prompt, &extensions, paint, cx)))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .px(px(6.))
                    .pb(px(6.))
                    .child(scroll_area("extensions-list").vertical().child(list)),
            )
            .child(
                v_stack()
                    .w_full()
                    .gap(px(10.))
                    .px(px(12.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(paint.border)
                    .child(
                        h_stack()
                            .gap(px(8.))
                            .items_center()
                            .child(div().flex_1().child(text_field(&self.ext_input, cx).placeholder("Chrome Web Store link or extension ID").full_width(true)))
                            .child(action_button("install-extension", "Install", Kind::Primary, true, cx, |this, cx| {
                                let raw = this.ext_input.read(cx).content().to_string();
                                this.install_from_input(&raw, cx);
                            })),
                    )
                    .child(
                        h_stack()
                            .gap(px(16.))
                            .items_center()
                            .child(link("browse-store", "Browse the store", paint, cx, |this, cx| {
                                this.popover = None;
                                this.with_model(|model| { let _ = model.create_tab(STORE_URL); });
                                cx.notify();
                            }))
                            .child(link("load-unpacked", "Load unpacked…", paint, cx, |this, cx| {
                                this.load_unpacked(cx);
                                cx.notify();
                            }))
                            .child(div().flex_1())
                            .child(link("reload-extensions", "Reload all", paint, cx, |this, cx| {
                                this.reload_extensions();
                                this.status = "Extensions reloaded".into();
                                cx.notify();
                            })),
                    ),
            )
    }

    /// One installed extension: click to open it; pin, options, remove and on/off at the end.
    fn extension_row(&self, extension: &browser_core::ExtensionRecord, paint: Paint, cx: &mut Context<Self>) -> impl IntoElement {
        let danger = cx.theme().danger();
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
        let hover = SharedString::from(format!("ext-row-{id}"));
        h_stack()
            .id(SharedString::from(format!("ext-{id}")))
            .group(hover.clone())
            .w_full()
            .gap(px(12.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(8.))
            .items_center()
            .when(enabled, |row| {
                row.cursor_pointer().hover(|style| style.bg(paint.secondary)).on_click(cx.listener({
                    let id = id.clone();
                    move |this, event: &ClickEvent, _, cx| {
                        this.popover = None;
                        this.open_extension(&id, f32::from(event.position().x), cx);
                    }
                }))
            })
            .child(div().flex_none().when(!enabled, |icon| icon.opacity(0.4)).child(extension_icon(&name, meta.icon.clone(), 28.)))
            .child(
                v_stack()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(2.))
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(if enabled { paint.fg } else { paint.muted }).child(name.clone()))
                    .child(match errors.first() {
                        Some(first) => div()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(danger)
                            .child(if errors.len() > 1 { format!("{} issues · {first}", errors.len()) } else { first.clone() }),
                        None => div().truncate().text_size(px(11.)).text_color(paint.muted).child(detail),
                    }),
            )
            .child(
                h_stack()
                    .flex_none()
                    .gap(px(2.))
                    .items_center()
                    // Secondary actions stay out of the way until the row is hovered; a pinned pin always shows.
                    .child(
                        div()
                            .flex()
                            .gap(px(2.))
                            .opacity(0.)
                            .group_hover(hover.clone(), |style| style.opacity(1.))
                            .when(has_options, |actions| {
                                actions.child(icon_button(SharedString::from(format!("ext-options-{id}")), Icons::gear()).small().on_click(cx.listener({
                                    let id = id.clone();
                                    move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.popover = None;
                                        this.open_extension_options(&id, cx);
                                    }
                                })))
                            })
                            .child(icon_button(SharedString::from(format!("ext-remove-{id}")), Icons::trash()).small().on_click(cx.listener({
                                let id = id.clone();
                                move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.remove_extension(&id);
                                    cx.notify();
                                }
                            }))),
                    )
                    .when(enabled, |actions| {
                        actions.child(
                            div().when(!pinned, |pin| pin.opacity(0.).group_hover(hover.clone(), |style| style.opacity(1.))).child(
                                icon_button(SharedString::from(format!("ext-pin-{id}")), if pinned { Icons::drawing_pin_filled() } else { Icons::drawing_pin() })
                                    .small()
                                    .selected(pinned)
                                    .on_click(cx.listener({
                                        let id = id.clone();
                                        move |this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            let _ = this.model.lock().set_extension_pinned(&id, !pinned);
                                            cx.notify();
                                        }
                                    })),
                            ),
                        )
                    })
                    .child(div().ml(px(6.)).child(toggle(SharedString::from(format!("ext-toggle-{id}")), enabled, paint).on_click(cx.listener({
                        let id = id.clone();
                        move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            let _ = this.model.lock().set_extension_enabled(&id, !enabled);
                            this.sync_extension(&id);
                            cx.notify();
                        }
                    })))),
            )
    }

    fn store_banner(&self, id: String, paint: Paint, cx: &mut Context<Self>) -> impl IntoElement {
        h_stack()
            .mx(px(12.))
            .mb(px(8.))
            .p(px(10.))
            .gap(px(10.))
            .items_center()
            .rounded(px(8.))
            .bg(paint.accent.opacity(0.12))
            .child(gpui::svg().path(crate::PUZZLE_ICON).size(px(16.)).flex_none().text_color(paint.accent))
            .child(div().flex_1().text_size(px(12.)).child("This page is a Chrome Web Store listing."))
            .child(action_button("add-from-store", "Add to NxBrowser", Kind::Primary, true, cx, move |this, cx| {
                this.install_from_store(id.clone());
                cx.notify();
            }))
    }

    fn prompt_row(&self, (prompt, id, request): (u64, String, String), extensions: &[browser_core::ExtensionRecord], paint: Paint, cx: &mut Context<Self>) -> impl IntoElement {
        let extension = extensions.iter().find(|extension| extension.id == id);
        let name = extension.map_or(id.clone(), |extension| extension.name.clone());
        let icon = self.ext_meta.get(&id).and_then(|meta| meta.icon.clone());
        v_stack()
            .mx(px(12.))
            .mb(px(8.))
            .p(px(12.))
            .gap(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(paint.border)
            .bg(paint.secondary.opacity(0.5))
            .child(
                h_stack()
                    .gap(px(10.))
                    .items_center()
                    .child(extension_icon(&name, icon, 20.))
                    .child(div().flex_1().min_w(px(0.)).text_size(px(12.)).child(format!("{name} wants {request}"))),
            )
            .child(
                h_stack()
                    .gap(px(8.))
                    .justify_end()
                    .child(action_button(SharedString::from(format!("prompt-deny-{prompt}")), "Deny", Kind::Secondary, true, cx, move |this, cx| {
                        this.answer_prompt(prompt, false);
                        cx.notify();
                    }))
                    .child(action_button(SharedString::from(format!("prompt-allow-{prompt}")), "Allow", Kind::Primary, true, cx, move |this, cx| {
                        this.answer_prompt(prompt, true);
                        cx.notify();
                    })),
            )
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
                    .text_size(px(12.))
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

/// Quiet text button for the popover footer.
fn link(
    id: &'static str,
    label: &'static str,
    paint: Paint,
    cx: &mut Context<BrowserWindow>,
    on_click: impl Fn(&mut BrowserWindow, &mut Context<BrowserWindow>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .text_size(px(12.))
        .text_color(paint.muted)
        .cursor_pointer()
        .hover(|style| style.text_color(paint.fg))
        .child(label)
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| on_click(this, cx)))
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
