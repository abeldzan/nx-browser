use crate::settings::{heading, measure_overlay};
use crate::ui::{action_button, Kind};
use crate::window::{paint, site_icon, BrowserWindow, Paint};
use browser_core::passwords::{self, site_of};
use browser_engine::WebEngine;
use browser_engine_macos::{authenticate, vault_key};
use gpui::{div, prelude::*, px, ClickEvent, Context, Entity, FontWeight, SharedString, Subscription};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::text_field::text_field;
use gpuikit::icons::Icons;
use gpuikit::input::{InputState, InputStateEvent, SubmitOn};
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::traits::control_sized::ControlSized;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long one Touch ID / password check keeps reveal, copy and export open.
const UNLOCK_FOR: Duration = Duration::from_secs(5 * 60);

/// A sign-in the page just submitted, waiting on Save / Update.
#[derive(Clone)]
pub(crate) struct SavePrompt {
    /// The profile of the tab the form was sent from, which may not be the one active when Save is clicked.
    profile_id: String,
    origin: String,
    username: String,
    password: String,
    /// The saved login this would update.
    existing: Option<String>,
}

pub(crate) struct PasswordState {
    pub(crate) prompt: Option<SavePrompt>,
    /// Login editor: `Some(None)` adds one, `Some(Some(id))` edits that login.
    pub(crate) edit: Option<Option<String>>,
    unlocked_until: Option<Instant>,
    /// Passwords shown in the manager, by login id; dropped when settings close.
    revealed: HashMap<String, String>,
    /// (weak, reused) by login id, from the last "Check passwords".
    health: HashMap<String, (bool, bool)>,
    search: Entity<InputState>,
    site: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    note: Entity<InputState>,
}

impl PasswordState {
    pub(crate) fn new(cx: &mut Context<BrowserWindow>) -> (Self, Vec<Subscription>) {
        let search = cx.new(InputState::new_singleline);
        let fields = [(); 4].map(|_| cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter)));
        let mut subscriptions = vec![cx.subscribe(&search, |this: &mut BrowserWindow, _, event, cx| match event {
            InputStateEvent::Focus => this.engine.focus_chrome(),
            InputStateEvent::TextChanged => cx.notify(),
            _ => {}
        })];
        for field in &fields {
            subscriptions.push(cx.subscribe(field, |this: &mut BrowserWindow, _, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit => this.save_login_edit(cx),
                _ => {}
            }));
        }
        let [site, username, password, note] = fields;
        let state = Self {
            prompt: None,
            edit: None,
            unlocked_until: None,
            revealed: HashMap::new(),
            health: HashMap::new(),
            search,
            site,
            username,
            password,
            note,
        };
        (state, subscriptions)
    }

    fn unlocked(&self) -> bool {
        self.unlocked_until.is_some_and(|until| Instant::now() < until)
    }

    /// Forgets every decrypted password the manager holds.
    pub(crate) fn lock_view(&mut self) {
        self.revealed.clear();
    }
}

/// What the page script sends (see `browser-engine-macos/src/passwords.js`).
#[derive(Deserialize)]
struct PageMessage {
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    new_password: Option<String>,
}

impl BrowserWindow {
    /// Loads the Keychain key into the model the first time a password is read or written.
    fn load_vault_key(&mut self) -> bool {
        if self.model.lock().has_vault_key() {
            return true;
        }
        match vault_key() {
            Ok(key) => {
                self.model.lock().set_vault_key(key);
                true
            }
            Err(error) => {
                self.status = format!("Passwords unavailable: {error}").into();
                false
            }
        }
    }

    /// The profile the Settings manager shows: the active one.
    fn manager_profile(&self) -> String {
        self.model.lock().active_profile_id.clone()
    }

    /// Answers the page's password script; the page waits on every message until it gets a reply.
    /// Everything is scoped to the profile of the tab that sent it.
    pub(crate) fn on_password_message(&mut self, tab_id: &str, origin: &str, request: u64, message: &str, cx: &mut Context<Self>) {
        let profile = self.model.lock().tab_profile(tab_id).map(|profile| (profile.id.clone(), profile.is_private()));
        let (Ok(message), Some((profile_id, private))) = (serde_json::from_str::<PageMessage>(message), profile) else {
            return self.engine.answer_passwords(request, "null");
        };
        let (autofill, offer) = {
            let model = self.model.lock();
            (model.settings.autofill_passwords, model.settings.offer_passwords)
        };
        let reply = match message.kind.as_str() {
            "form" if autofill => {
                let logins = self.model.lock().logins(&profile_id, Some(origin)).unwrap_or_default();
                let list: Vec<_> = logins.iter().map(|login| serde_json::json!({ "id": login.id, "username": login.username })).collect();
                serde_json::to_string(&list).unwrap_or_default()
            }
            "fill" if autofill && self.load_vault_key() => {
                let model = self.model.lock();
                // Scoped to the asking frame's origin: a page never gets another site's password.
                let password = model.login_password(&profile_id, &message.id, Some(origin)).ok().flatten();
                if password.is_some() {
                    let _ = model.note_login_used(&profile_id, &message.id);
                }
                serde_json::to_string(&password).unwrap_or_default()
            }
            "generate" => serde_json::to_string(&passwords::generate()).unwrap_or_default(),
            "submit" => {
                if offer && !private {
                    self.offer_save(profile_id, origin, message, cx);
                }
                "null".into()
            }
            _ => "null".into(),
        };
        self.engine.answer_passwords(request, &reply);
    }

    fn offer_save(&mut self, profile_id: String, origin: &str, message: PageMessage, cx: &mut Context<Self>) {
        let current = message.password;
        let password = message.new_password.filter(|p| !p.is_empty()).unwrap_or_else(|| current.clone());
        if password.is_empty() || self.model.lock().is_never_save(&profile_id, origin).unwrap_or(false) || !self.load_vault_key() {
            return;
        }
        let model = self.model.lock();
        let logins = model.logins(&profile_id, Some(origin)).unwrap_or_default();
        let saved = |id: &str| model.login_password(&profile_id, id, None).ok().flatten();
        // The login with this username; with none typed (a change-password page), the one whose old password was entered.
        let existing = if message.username.is_empty() {
            logins.iter().find(|login| !current.is_empty() && saved(&login.id).as_deref() == Some(current.as_str()))
        } else {
            logins.iter().find(|login| login.username == message.username)
        };
        if existing.is_some_and(|login| saved(&login.id).as_deref() == Some(password.as_str())) {
            return;
        }
        let username = existing.filter(|_| message.username.is_empty()).map_or(message.username, |login| login.username.clone());
        let existing = existing.map(|login| login.id.clone());
        drop(model);
        let prompt = SavePrompt { profile_id, origin: origin.to_string(), username, password, existing };
        // One prompt at a time: the newest submission is the one the user is looking at.
        self.passwords.prompt = Some(prompt);
        cx.notify();
    }

    /// "Save password for site?" floating under the title bar after a sign-in form is sent.
    pub(crate) fn save_password_prompt(&self, cx: &mut Context<Self>) -> gpui::Div {
        let Some(prompt) = self.passwords.prompt.clone() else {
            return div();
        };
        let paint = paint(cx);
        let host = site_of(&prompt.origin).to_string();
        let update = prompt.existing.is_some();
        let save = {
            let prompt = prompt.clone();
            move |this: &mut Self, cx: &mut Context<Self>| {
                let saved =
                    this.model.lock().save_login(&prompt.profile_id, prompt.existing.as_deref(), &prompt.origin, &prompt.username, &prompt.password, "");
                this.status = match saved {
                    Ok(()) => "Password saved".into(),
                    Err(error) => format!("Password not saved: {error}").into(),
                };
                this.passwords.prompt = None;
                cx.notify();
            }
        };
        let never = move |this: &mut Self, cx: &mut Context<Self>| {
            let _ = this.model.lock().never_save(&prompt.profile_id, &prompt.origin);
            this.passwords.prompt = None;
            cx.notify();
        };
        let username = if prompt.username.is_empty() { "No username".to_string() } else { prompt.username.clone() };
        div().absolute().top(px(self.titlebar_h() + 10.)).right(px(12.)).child(
            v_stack()
                .relative()
                .w(px(320.))
                .p(px(14.))
                .gap(px(10.))
                .rounded(px(10.))
                .bg(paint.surface)
                .border_1()
                .border_color(paint.border)
                .shadow_md()
                .text_size(px(13.))
                .occlude()
                .child(measure_overlay(10., cx))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(if update { format!("Update password for {host}?") } else { format!("Save password for {host}?") }),
                )
                .child(
                    v_stack()
                        .gap(px(2.))
                        .child(div().truncate().child(username))
                        .child(div().text_color(paint.muted).child("•".repeat(prompt.password.chars().count().min(16)))),
                )
                .child(
                    h_stack()
                        .gap(px(8.))
                        .when(!update, |row| row.child(action_button("password-never", "Never", Kind::Secondary, true, cx, never)))
                        .child(div().flex_1())
                        .child(action_button("password-not-now", "Not now", Kind::Secondary, true, cx, |this, cx| {
                            this.passwords.prompt = None;
                            cx.notify();
                        }))
                        .child(action_button("password-save", if update { "Update" } else { "Save" }, Kind::Primary, true, cx, save)),
                ),
        )
    }

    /// Runs `then` after Touch ID or the Mac password, or straight away within `UNLOCK_FOR` of the last check.
    fn with_unlock(&mut self, reason: &str, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static) {
        if self.passwords.unlocked() {
            if self.load_vault_key() {
                then(self, cx);
            }
            return;
        }
        let (sender, receiver) = futures::channel::oneshot::channel();
        authenticate(reason, move |ok| {
            let _ = sender.send(ok);
        });
        cx.spawn(async move |this, cx| {
            if receiver.await != Ok(true) {
                let _ = this.update(cx, |this, cx| {
                    this.settings_note = Some("Passwords stay locked".into());
                    cx.notify();
                });
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.passwords.unlocked_until = Some(Instant::now() + UNLOCK_FOR);
                if this.load_vault_key() {
                    then(this, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Settings → Passwords, below the two switches.
    pub(crate) fn passwords_settings(&self, paint: Paint, cx: &mut Context<Self>) -> gpui::Div {
        let logins = self.model.lock().logins(&self.manager_profile(), None).unwrap_or_default();
        let query = self.passwords.search.read(cx).content().trim().to_lowercase();
        let unlocked = self.passwords.unlocked();
        // Counted over this profile's logins: `health` may still hold another profile's results (ids never collide).
        let checks: Vec<(bool, bool)> = logins.iter().filter_map(|login| self.passwords.health.get(&login.id).copied()).collect();
        let (weak, reused) = checks.iter().fold((0, 0), |(w, r), &(weak, reused)| (w + weak as usize, r + reused as usize));
        let actions = h_stack()
            .gap(px(8.))
            .pt(px(8.))
            .child(action_button("password-add", "Add…", Kind::Secondary, true, cx, |this, cx| this.open_login_editor(None, cx)))
            .child(action_button("password-import", "Import CSV…", Kind::Secondary, true, cx, |this, cx| this.import_logins(cx)))
            .child(action_button("password-export", "Export CSV…", Kind::Secondary, !logins.is_empty(), cx, |this, cx| {
                this.with_unlock("export your saved passwords", cx, |this, cx| this.export_logins(cx))
            }))
            .child(action_button("password-check", "Check passwords", Kind::Secondary, !logins.is_empty(), cx, |this, cx| {
                this.with_unlock("check your saved passwords", cx, |this, cx| this.check_logins(cx))
            }));
        let mut list = v_stack().w_full().gap(px(2.));
        let shown: Vec<_> = logins
            .iter()
            .filter(|login| query.is_empty() || login.origin.to_lowercase().contains(&query) || login.username.to_lowercase().contains(&query))
            .collect();
        for login in &shown {
            let revealed = unlocked.then(|| self.passwords.revealed.get(&login.id)).flatten();
            let (is_weak, is_reused) = self.passwords.health.get(&login.id).copied().unwrap_or_default();
            let (reveal, copy, edit) = (login.id.clone(), login.id.clone(), login.id.clone());
            let badge = |text: &'static str| {
                div().flex_none().px(px(6.)).py(px(1.)).rounded(px(4.)).text_size(px(10.5)).bg(paint.secondary).text_color(paint.muted).child(text)
            };
            list = list.child(
                h_stack()
                    .w_full()
                    .gap(px(10.))
                    .py(px(6.))
                    .items_center()
                    .border_b_1()
                    .border_color(paint.border.opacity(0.5))
                    .child(div().flex_none().child(site_icon(&login.origin, 16., false)))
                    .child(
                        v_stack()
                            .flex_1()
                            .min_w(px(0.))
                            .child(div().truncate().child(site_of(&login.origin).to_string()))
                            .child(div().truncate().text_size(px(11.)).text_color(paint.muted).child(if login.username.is_empty() {
                                "No username".to_string()
                            } else {
                                login.username.clone()
                            })),
                    )
                    .when(is_weak, |row| row.child(badge("Weak")))
                    .when(is_reused, |row| row.child(badge("Reused")))
                    .child(div().flex_none().w(px(140.)).truncate().text_size(px(12.)).text_color(paint.muted).child(
                        revealed.cloned().unwrap_or_else(|| "••••••••".into()),
                    ))
                    .child(
                        icon_button(SharedString::from(format!("password-reveal-{}", login.id)), if revealed.is_some() { Icons::eye_closed() } else { Icons::eye_open() })
                            .small()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if this.passwords.unlocked() && this.passwords.revealed.remove(&reveal).is_some() {
                                    return cx.notify();
                                }
                                let id = reveal.clone();
                                this.with_unlock("show a saved password", cx, move |this, cx| {
                                    let profile_id = this.manager_profile();
                                    if let Ok(Some(password)) = this.model.lock().login_password(&profile_id, &id, None) {
                                        this.passwords.revealed.insert(id, password);
                                    }
                                    cx.notify();
                                });
                            })),
                    )
                    .child(icon_button(SharedString::from(format!("password-copy-{}", login.id)), Icons::copy()).small().on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            let id = copy.clone();
                            this.with_unlock("copy a saved password", cx, move |this, cx| {
                                let profile_id = this.manager_profile();
                                if let Ok(Some(password)) = this.model.lock().login_password(&profile_id, &id, None) {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(password));
                                    this.settings_note = Some("Password copied".into());
                                }
                                cx.notify();
                            });
                        },
                    )))
                    .child(icon_button(SharedString::from(format!("password-edit-{}", login.id)), Icons::pencil_1()).small().on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.open_login_editor(Some(edit.clone()), cx),
                    ))),
            );
        }
        let summary = if logins.is_empty() {
            "No saved passwords yet. They are kept on this Mac only, encrypted with a key in your Keychain.".to_string()
        } else if checks.is_empty() {
            format!("{} saved on this Mac.", logins.len())
        } else {
            format!("{} saved on this Mac · {weak} weak · {reused} reused.", logins.len())
        };
        v_stack()
            .w_full()
            .child(actions)
            .child(heading("Saved passwords (current profile)", paint))
            .child(div().pb(px(8.)).text_size(px(11.)).text_color(paint.muted).child(summary))
            .when(!logins.is_empty(), |column| {
                column.child(div().w(px(260.)).pb(px(8.)).child(text_field(&self.passwords.search, cx).placeholder("Search sites and usernames").full_width(true)))
            })
            .when(!logins.is_empty() && shown.is_empty(), |column| column.child(div().text_color(paint.muted).child("No matches")))
            .child(list)
    }

    fn check_logins(&mut self, cx: &mut Context<Self>) {
        let profile_id = self.manager_profile();
        let rows = self.model.lock().logins_with_passwords(&profile_id).unwrap_or_default();
        let reused = passwords::reused(&rows);
        self.passwords.health = rows
            .iter()
            .map(|(login, password)| (login.id.clone(), (passwords::is_weak(password), reused.contains(&login.id))))
            .collect();
        cx.notify();
    }

    fn import_logins(&mut self, cx: &mut Context<Self>) {
        // gpui's sheet, not runModal: a modal loop inside this click lets the search field's blink timer re-enter gpui and abort.
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            if let Some(path) = paths.into_iter().next() {
                let _ = this.update(cx, |this, cx| this.import_logins_from(&path, cx));
            }
        })
        .detach();
    }

    fn import_logins_from(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if !self.load_vault_key() {
            self.settings_note = Some(self.status.clone());
            return cx.notify();
        }
        let imported = std::fs::read_to_string(path)
            .map_err(anyhow::Error::from)
            .and_then(|text| passwords::from_csv(&text))
            .and_then(|rows| {
                let total = rows.len();
                let profile_id = self.manager_profile();
                Ok((self.model.lock().import_logins(&profile_id, rows)?, total))
            });
        self.settings_note = Some(match imported {
            Ok((taken, total)) if taken == total => format!("Imported {taken} passwords").into(),
            Ok((taken, total)) => format!("Imported {taken} of {total} passwords; the rest had no web address or password").into(),
            Err(error) => format!("Import failed: {error}").into(),
        });
        self.passwords.health.clear();
        cx.notify();
    }

    fn export_logins(&mut self, cx: &mut Context<Self>) {
        let profile_id = self.manager_profile();
        let rows = match self.model.lock().logins_with_passwords(&profile_id) {
            Ok(rows) => rows,
            Err(error) => {
                self.settings_note = Some(format!("Export failed: {error}").into());
                return cx.notify();
            }
        };
        // Same re-entrancy as import: gpui's save sheet instead of runModal.
        let path = cx.prompt_for_new_path(&browser_core::downloads_dir(), Some("Passwords.csv"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else { return };
            let _ = this.update(cx, |this, cx| this.write_logins(&path, &rows, cx));
        })
        .detach();
    }

    fn write_logins(&mut self, path: &std::path::Path, rows: &[(passwords::Login, String)], cx: &mut Context<Self>) {
        let written = write_private(path, passwords::to_csv(rows).as_bytes());
        self.settings_note = Some(match written {
            Ok(()) => format!("Exported {} passwords to {}. Anyone who can open that file can read them.", rows.len(), path.display()).into(),
            Err(error) => format!("Export failed: {error}").into(),
        });
        cx.notify();
    }

    /// `None` adds a login; editing shows the password, so it asks for Touch ID first.
    fn open_login_editor(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let Some(id) = id else {
            for field in [&self.passwords.site, &self.passwords.username, &self.passwords.password, &self.passwords.note] {
                field.update(cx, |input, cx| input.set_content_silent(String::new(), cx));
            }
            self.passwords.edit = Some(None);
            return cx.notify();
        };
        self.with_unlock("edit a saved password", cx, move |this, cx| {
            let (login, password) = {
                let model = this.model.lock();
                let profile_id = model.active_profile_id.clone();
                let login = model.logins(&profile_id, None).unwrap_or_default().into_iter().find(|login| login.id == id);
                (login, model.login_password(&profile_id, &id, None).ok().flatten())
            };
            let (Some(login), Some(password)) = (login, password) else {
                return;
            };
            let values = [login.origin, login.username, password, login.note];
            for (field, value) in [&this.passwords.site, &this.passwords.username, &this.passwords.password, &this.passwords.note].into_iter().zip(values) {
                field.update(cx, |input, cx| input.set_content_silent(value, cx));
            }
            this.passwords.edit = Some(Some(id));
            cx.notify();
        });
    }

    pub(crate) fn save_login_edit(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.passwords.edit.clone() else {
            return;
        };
        let read = |field: &Entity<InputState>| field.read(cx).content().to_string();
        let (site, username, password, note) =
            (read(&self.passwords.site), read(&self.passwords.username).trim().to_string(), read(&self.passwords.password), read(&self.passwords.note));
        let site = site.trim();
        let origin = passwords::origin_of(site).or_else(|| passwords::origin_of(&format!("https://{site}")));
        let (Some(origin), false) = (origin, password.is_empty()) else {
            self.settings_note = Some("A login needs a web address and a password".into());
            return cx.notify();
        };
        if !self.load_vault_key() {
            return cx.notify();
        }
        let profile_id = self.manager_profile();
        match self.model.lock().save_login(&profile_id, id.as_deref(), &origin, &username, &password, &note) {
            Ok(()) => self.settings_note = Some("Password saved".into()),
            Err(error) => self.settings_note = Some(format!("Password not saved: {error}").into()),
        }
        self.passwords.edit = None;
        self.passwords.health.clear();
        self.passwords.revealed.clear();
        cx.notify();
    }

    pub(crate) fn login_editor(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let paint = paint(cx);
        let id = self.passwords.edit.clone().flatten();
        let label = |text: &'static str| div().text_size(px(12.)).text_color(paint.muted).child(text);
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.passwords.edit = None;
            cx.notify();
        };
        let card = v_stack()
            .w(px(420.))
            .max_w_full()
            .gap(px(8.))
            .p(px(24.))
            .child(div().pb(px(6.)).text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(if id.is_some() { "Edit password" } else { "Add password" }))
            .child(label("Website"))
            .child(text_field(&self.passwords.site, cx).placeholder("example.com").full_width(true))
            .child(label("Username"))
            .child(text_field(&self.passwords.username, cx).placeholder("Username or email").full_width(true))
            .child(label("Password"))
            .child(
                h_stack()
                    .gap(px(8.))
                    .child(div().flex_1().child(text_field(&self.passwords.password, cx).placeholder("Password").full_width(true)))
                    .child(action_button("password-generate", "Generate", Kind::Secondary, true, cx, |this, cx| {
                        let password = passwords::generate();
                        this.passwords.password.update(cx, |input, cx| input.set_content_silent(password, cx));
                        cx.notify();
                    })),
            )
            .child(label("Note"))
            .child(text_field(&self.passwords.note, cx).placeholder("Optional").full_width(true))
            .child(
                h_stack()
                    .pt(px(16.))
                    .gap(px(10.))
                    .when_some(id, |row, id| {
                        row.child(action_button("password-delete", "Delete", Kind::Danger, true, cx, move |this, cx| {
                            let profile_id = this.manager_profile();
                            let _ = this.model.lock().delete_login(&profile_id, &id);
                            this.passwords.edit = None;
                            this.passwords.revealed.remove(&id);
                            this.passwords.health.clear();
                            this.settings_note = Some("Password deleted".into());
                            cx.notify();
                        }))
                    })
                    .child(div().flex_1())
                    .child(action_button("password-cancel", "Cancel", Kind::Secondary, true, cx, close))
                    .child(action_button("password-save-edit", "Save", Kind::Primary, true, cx, |this, cx| this.save_login_edit(cx))),
            );
        self.modal_shell("login-editor", card, close, cx)
    }
}

/// Owner-only from creation: the export is the one place passwords sit unencrypted.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    // `mode` only applies to a new file; an overwritten one keeps its old permissions otherwise.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)
}
