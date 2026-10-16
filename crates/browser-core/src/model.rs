use crate::lifecycle::{suspend_candidates, LiveTab, TabState};
use crate::{now_secs, Settings};
use anyhow::Context;
use browser_storage::Storage;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub storage_path: PathBuf,
    pub data_store_id: String,
    pub created_at: i64,
    /// Emoji or `.svg` asset path shown in place of the initial letter; empty means the letter.
    pub icon: String,
    /// `#rrggbb`; empty means the theme accent.
    pub color: String,
}

/// Data store ids with this prefix are in-memory WebKit stores: nothing reaches disk and the profile is purged at launch.
pub const PRIVATE_STORE_PREFIX: &str = "private-";

impl Profile {
    pub fn is_private(&self) -> bool {
        self.data_store_id.starts_with(PRIVATE_STORE_PREFIX)
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub profile_id: String,
    pub name: String,
    pub active: bool,
    /// Windows float where they are put instead of tiling the stage.
    pub free: bool,
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: String,
    pub session_id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
    pub position: i32,
    pub state: TabState,
    /// Window frame/snap/z-order saved by the UI as JSON; empty means default placement.
    pub layout: String,
    /// Profile a window moved in from another profile keeps (its site data and history); empty means the space's.
    pub profile_id: String,
}

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub id: i64,
    pub profile_id: String,
    pub url: String,
    pub title: String,
    pub visited_at: i64,
}

#[derive(Clone, Debug)]
pub struct Bookmark {
    pub id: String,
    pub profile_id: String,
    pub url: String,
    pub title: String,
    pub created_at: i64,
    /// Shown as a tile on the new-tab page.
    pub pinned: bool,
}

#[derive(Clone, Debug)]
pub struct DownloadRecord {
    pub id: String,
    pub profile_id: String,
    pub url: String,
    pub path: String,
    pub status: String,
    pub created_at: i64,
}

#[derive(Clone, Debug)]
pub struct ExtensionRecord {
    /// Chrome-style 32-letter id; also WebKit's unique identifier and `webkit-extension://` host.
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    /// `store` (unpacked into app support), `unpacked` (a folder the user picked) or `bundled`.
    pub source: String,
    pub enabled: bool,
    pub pinned: bool,
}

#[derive(Clone, Debug)]
pub enum Effect {
    EnsureTab {
        tab_id: String,
        data_store_id: String,
        url: String,
    },
    CloseTab {
        tab_id: String,
    },
    SuspendTab {
        tab_id: String,
    },
    ShowTab {
        tab_id: Option<String>,
    },
    Navigate {
        tab_id: String,
        url: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileBundle {
    pub profile: BundleProfile,
    pub tabs: Vec<BundleTab>,
    pub bookmarks: Vec<BundleBookmark>,
    pub history: Vec<BundleHistory>,
    pub permissions: Vec<BundlePermission>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleProfile {
    pub name: String,
    pub data_store_id: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub color: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleTab {
    pub url: String,
    pub title: String,
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleBookmark {
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleHistory {
    pub url: String,
    pub title: String,
    pub visited_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundlePermission {
    pub origin: String,
    pub permission: String,
    pub decision: String,
}

pub struct Model {
    storage: Storage,
    pub profiles: Vec<Profile>,
    pub sessions: Vec<Session>,
    pub tabs: Vec<Tab>,
    pub extensions: Vec<ExtensionRecord>,
    pub active_profile_id: String,
    pub active_session_id: String,
    pub settings: Settings,
    background_since: Vec<(String, i64)>,
    effects: Vec<Effect>,
    /// Recently closed tabs as (session, url, layout), newest last.
    // shortcut: in-memory only, lost on quit; persist to SQLite if users want it across restarts.
    closed: Vec<(String, String, String)>,
    /// Extension lists of inactive profiles, so flipping profiles (a detached window's every lock) skips SQLite.
    ext_cache: HashMap<String, Vec<ExtensionRecord>>,
    /// Last history and suggestion answers; render asks the same LIKE scan every frame while a list is open.
    history_memo: Memo<Vec<HistoryEntry>>,
    suggest_memo: Memo<Vec<(String, String)>>,
    bookmarks_memo: Memo<Vec<Bookmark>>,
}

/// (storage generation, profile, query, limit): any write or a different question misses.
/// Visits older than this are dropped at startup.
const HISTORY_DAYS: i64 = 90;

type MemoKey = (u64, String, String, usize);
type Memo<T> = RefCell<Option<(MemoKey, T)>>;

fn memo<T: Clone>(slot: &Memo<T>, key: MemoKey, load: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    if let Some((cached, value)) = &*slot.borrow() {
        if *cached == key {
            return Ok(value.clone());
        }
    }
    let value = load()?;
    *slot.borrow_mut() = Some((key, value.clone()));
    Ok(value)
}

impl Model {
    pub fn open(storage: Storage) -> anyhow::Result<Self> {
        let mut model = Self {
            storage,
            profiles: Vec::new(),
            sessions: Vec::new(),
            tabs: Vec::new(),
            extensions: Vec::new(),
            active_profile_id: String::new(),
            active_session_id: String::new(),
            settings: Settings::default(),
            background_since: Vec::new(),
            effects: Vec::new(),
            closed: Vec::new(),
            ext_cache: HashMap::new(),
            history_memo: RefCell::new(None),
            suggest_memo: RefCell::new(None),
            bookmarks_memo: RefCell::new(None),
        };
        model.reload()?;
        // A private profile left by the last run (quit or crash) has nothing worth keeping.
        let leftover: Vec<String> = model.profiles.iter().filter(|p| p.is_private()).map(|p| p.id.clone()).collect();
        for id in &leftover {
            model.purge_profile_rows(id)?;
        }
        if !leftover.is_empty() {
            model.active_profile_id.clear();
            model.active_session_id.clear();
            model.reload()?;
        }
        if model.profiles.is_empty() {
            model.create_profile("Personal")?;
        }
        if let Some(value) = model.storage.setting("prefs")? {
            model.settings = serde_json::from_str(&value).unwrap_or_else(|error| {
                // Defaults get written back over these on the next save; keep the user's copy to recover from.
                eprintln!("settings unreadable ({error}); defaults in use, original kept as prefs.bak");
                let _ = model.storage.set_setting("prefs.bak", &value);
                Settings::default()
            });
        }
        // Bounds the LIKE scans behind suggestions and history search.
        model.storage.execute(
            "DELETE FROM history WHERE visited_at < ?1",
            params![now_secs() - HISTORY_DAYS * 24 * 60 * 60],
        )?;
        Ok(model)
    }

    pub fn update_settings(&mut self, f: impl FnOnce(&mut Settings)) -> anyhow::Result<()> {
        f(&mut self.settings);
        self.storage.set_setting("prefs", &serde_json::to_string(&self.settings)?)
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    pub fn request_show_active(&mut self) {
        self.queue_show_active();
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == self.active_profile_id)
    }

    /// The profile whose site data a tab uses: its own if it was moved in from another profile, else its space's.
    pub fn tab_profile(&self, tab_id: &str) -> Option<&Profile> {
        let tab = self.tabs.iter().find(|tab| tab.id == tab_id)?;
        let id = match tab.profile_id.as_str() {
            "" => &self.sessions.iter().find(|session| session.id == tab.session_id)?.profile_id,
            id => id,
        };
        self.profiles.iter().find(|profile| profile.id == *id)
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.active && tab.session_id == self.active_session_id)
    }

    pub fn session_tabs(&self) -> Vec<&Tab> {
        let mut tabs: Vec<&Tab> = self
            .tabs
            .iter()
            .filter(|tab| tab.session_id == self.active_session_id)
            .collect();
        tabs.sort_by_key(|tab| tab.position);
        tabs
    }

    pub fn profile_sessions(&self) -> Vec<&Session> {
        self.sessions
            .iter()
            .filter(|session| session.profile_id == self.active_profile_id)
            .collect()
    }

    pub fn create_profile(&mut self, name: &str) -> anyhow::Result<String> {
        self.insert_new_profile(name, Uuid::new_v4().to_string(), "")
    }

    /// A profile whose cookies and site data live only in memory and that keeps no history.
    pub fn create_private_profile(&mut self) -> anyhow::Result<String> {
        self.insert_new_profile("Private", format!("{PRIVATE_STORE_PREFIX}{}", Uuid::new_v4()), "🕶️")
    }

    fn insert_new_profile(&mut self, name: &str, data_store_id: String, icon: &str) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let storage_path = crate::app_support_dir().join("profiles").join(&id);
        std::fs::create_dir_all(&storage_path)?;
        let profile = Profile {
            id: id.clone(),
            name: name.to_string(),
            storage_path,
            data_store_id,
            created_at: now_secs(),
            icon: icon.to_string(),
            color: String::new(),
        };
        // All or nothing: a profile row without its session can never be switched to.
        let tx = self.storage.conn().unchecked_transaction()?;
        self.insert_profile(&profile)?;
        let session_id = self.insert_session(&profile.id, "Session 1")?;
        self.insert_tab(&session_id, "nxbrowser://home", "New Tab", true, 0, TabState::Suspended)?;
        tx.commit()?;
        self.active_profile_id = profile.id.clone();
        self.active_session_id = session_id;
        self.save_active()?;
        self.reload()?;
        self.queue_show_active();
        Ok(id)
    }

    pub fn rename_profile(&mut self, id: &str, name: &str) -> anyhow::Result<()> {
        self.storage.execute(
            "UPDATE profiles SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        self.reload()?;
        Ok(())
    }

    pub fn set_profile_appearance(&mut self, id: &str, icon: &str, color: &str) -> anyhow::Result<()> {
        self.storage.execute(
            "UPDATE profiles SET icon = ?1, color = ?2 WHERE id = ?3",
            params![icon, color, id],
        )?;
        self.reload()?;
        Ok(())
    }

    /// Moves a profile to `index` in the rail order.
    pub fn move_profile(&mut self, id: &str, index: usize) -> anyhow::Result<()> {
        let mut ids: Vec<String> = self.profiles.iter().map(|profile| profile.id.clone()).collect();
        let Some(from) = ids.iter().position(|existing| existing == id) else {
            return Ok(());
        };
        let moved = ids.remove(from);
        ids.insert(index.min(ids.len()), moved);
        let tx = self.storage.conn().unchecked_transaction()?;
        for (position, id) in ids.iter().enumerate() {
            self.storage.execute("UPDATE profiles SET position = ?1 WHERE id = ?2", params![position as i64, id])?;
        }
        tx.commit()?;
        self.reload()?;
        Ok(())
    }

    /// Moves a tab to `index` in its space's tab order.
    pub fn move_tab(&mut self, id: &str, index: usize) -> anyhow::Result<()> {
        let mut ids: Vec<String> = self.session_tabs().iter().map(|tab| tab.id.clone()).collect();
        let Some(from) = ids.iter().position(|existing| existing == id) else {
            return Ok(());
        };
        let moved = ids.remove(from);
        ids.insert(index.min(ids.len()), moved);
        let tx = self.storage.conn().unchecked_transaction()?;
        for (position, id) in ids.iter().enumerate() {
            self.storage.execute("UPDATE tabs SET position = ?1 WHERE id = ?2", params![position as i32, id])?;
        }
        tx.commit()?;
        self.reload()?;
        Ok(())
    }

    /// Clears the active profile's history and/or download list.
    pub fn clear_data(&self, history: bool, downloads: bool) -> anyhow::Result<()> {
        if history {
            self.storage.execute("DELETE FROM history WHERE profile_id = ?1", params![self.active_profile_id])?;
        }
        if downloads {
            self.storage.execute("DELETE FROM downloads WHERE profile_id = ?1", params![self.active_profile_id])?;
        }
        Ok(())
    }

    pub fn delete_profile(&mut self, id: &str) -> anyhow::Result<Option<String>> {
        if self.profiles.len() <= 1 {
            return Ok(None);
        }
        let data_store_id = self
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .map(|profile| profile.data_store_id.clone());
        let tab_ids: Vec<String> = self
            .tabs
            .iter()
            .filter(|tab| self.tab_profile(&tab.id).is_some_and(|profile| profile.id == id))
            .map(|tab| tab.id.clone())
            .collect();
        for tab_id in tab_ids {
            self.effects.push(Effect::CloseTab { tab_id });
        }
        self.purge_profile_rows(id)?;
        self.reload()?;
        // Older imports could share a store between profiles; the survivor still needs its site data.
        let data_store_id = data_store_id.filter(|store| !self.profiles.iter().any(|profile| &profile.data_store_id == store));
        if self.active_profile_id == id {
            self.active_profile_id = self.profiles[0].id.clone();
            self.extensions = query_extensions(&self.storage, &self.active_profile_id)?;
            self.active_session_id = self
                .sessions
                .iter()
                .find(|session| session.profile_id == self.active_profile_id)
                .map(|session| session.id.clone())
                .unwrap_or_default();
            self.save_active()?;
            self.queue_show_active();
        }
        Ok(data_store_id)
    }

    pub fn set_tab_layout(&mut self, id: &str, layout: &str) -> anyhow::Result<()> {
        self.storage.execute("UPDATE tabs SET layout = ?1 WHERE id = ?2", params![layout, id])?;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.layout = layout.to_string();
        }
        Ok(())
    }

    /// Rewrites a tab's URL without a navigation or history entry (a terminal tab saving its folder).
    pub fn set_tab_url(&mut self, id: &str, url: &str) -> anyhow::Result<()> {
        self.storage.execute("UPDATE tabs SET url = ?1 WHERE id = ?2", params![url, id])?;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.url = url.to_string();
        }
        Ok(())
    }

    pub fn switch_profile(&mut self, id: &str) -> anyhow::Result<()> {
        let Some(profile) = self.profiles.iter().find(|profile| profile.id == id) else {
            return Ok(());
        };
        let profile_id = profile.id.clone();
        self.mark_active_background();
        self.active_profile_id = profile_id;
        self.active_session_id = self
            .sessions
            .iter()
            .find(|session| session.profile_id == id && session.active)
            .or_else(|| self.sessions.iter().find(|session| session.profile_id == id))
            .map(|session| session.id.clone())
            .context("profile has no session")?;
        self.storage.execute(
            "UPDATE sessions SET active = CASE WHEN id = ?1 THEN 1 ELSE 0 END WHERE profile_id = ?2",
            params![self.active_session_id, id],
        )?;
        self.focus_session_tab()?;
        self.save_active()?;
        self.reload()?;
        self.queue_show_active();
        Ok(())
    }

    fn purge_profile_rows(&self, id: &str) -> anyhow::Result<()> {
        let tx = self.storage.conn().unchecked_transaction()?;
        self.storage.execute("DELETE FROM tabs WHERE session_id IN (SELECT id FROM sessions WHERE profile_id = ?1)", params![id])?;
        self.storage.execute("DELETE FROM tabs WHERE profile_id = ?1", params![id])?;
        self.storage.execute("DELETE FROM sessions WHERE profile_id = ?1", params![id])?;
        self.storage.execute("DELETE FROM profiles WHERE id = ?1", params![id])?;
        self.storage.execute("DELETE FROM bookmarks WHERE profile_id = ?1", params![id])?;
        self.storage.execute("DELETE FROM history WHERE profile_id = ?1", params![id])?;
        self.storage.execute("DELETE FROM permissions WHERE profile_id = ?1", params![id])?;
        self.storage.execute("DELETE FROM profile_extensions WHERE profile_id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_session(&mut self) -> anyhow::Result<()> {
        let taken = self.profile_sessions().into_iter().map(|session| session.name.clone()).collect::<Vec<_>>();
        let name = (1..).map(|n| format!("Space {n}")).find(|name| !taken.contains(name)).unwrap_or_default();
        let session_id = self.insert_session(&self.active_profile_id.clone(), &name)?;
        self.insert_tab(&session_id, "nxbrowser://home", "New Tab", true, 0, TabState::Suspended)?;
        // switch_session only knows sessions already loaded.
        self.reload()?;
        self.switch_session(&session_id)?;
        Ok(())
    }

    pub fn rename_session(&mut self, session_id: &str, name: &str) -> anyhow::Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        self.storage.execute("UPDATE sessions SET name = ?1 WHERE id = ?2", params![name, session_id])?;
        self.reload()?;
        Ok(())
    }

    pub fn set_session_free(&mut self, session_id: &str, free: bool) -> anyhow::Result<()> {
        self.storage.execute("UPDATE sessions SET free = ?1 WHERE id = ?2", params![free as i32, session_id])?;
        self.reload()?;
        Ok(())
    }

    pub fn switch_session(&mut self, session_id: &str) -> anyhow::Result<()> {
        if !self.sessions.iter().any(|session| session.id == session_id) {
            return Ok(());
        }
        self.mark_active_background();
        self.active_session_id = session_id.to_string();
        self.storage.execute(
            "UPDATE sessions SET active = CASE WHEN id = ?1 THEN 1 ELSE 0 END WHERE profile_id = ?2",
            params![session_id, self.active_profile_id],
        )?;
        self.focus_session_tab()?;
        self.save_active()?;
        self.reload()?;
        self.queue_show_active();
        Ok(())
    }

    /// Closes a space and its tabs; the profile always keeps at least one.
    pub fn delete_session(&mut self, session_id: &str) -> anyhow::Result<()> {
        let siblings = self.profile_sessions().into_iter().map(|session| session.id.clone()).collect::<Vec<_>>();
        if siblings.len() <= 1 || !siblings.iter().any(|id| id == session_id) {
            return Ok(());
        }
        for tab in self.tabs.iter().filter(|tab| tab.session_id == session_id) {
            self.effects.push(Effect::CloseTab { tab_id: tab.id.clone() });
        }
        let tx = self.storage.conn().unchecked_transaction()?;
        self.storage.execute("DELETE FROM tabs WHERE session_id = ?1", params![session_id])?;
        self.storage.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        tx.commit()?;
        if self.active_session_id == session_id {
            let index = siblings.iter().position(|id| id == session_id).unwrap_or(0);
            let next = siblings.iter().filter(|id| *id != session_id).nth(index.saturating_sub(1)).cloned().unwrap_or_default();
            self.reload()?;
            return self.switch_session(&next);
        }
        self.reload()
    }

    /// Points the model at another window's space without saving it as the launch context.
    pub fn set_active(&mut self, session_id: &str) -> anyhow::Result<()> {
        let Some(profile_id) = self.sessions.iter().find(|session| session.id == session_id).map(|session| session.profile_id.clone()) else {
            return Ok(());
        };
        self.active_session_id = session_id.to_string();
        if profile_id != self.active_profile_id {
            let extensions = match self.ext_cache.remove(&profile_id) {
                Some(extensions) => extensions,
                None => query_extensions(&self.storage, &profile_id)?,
            };
            let old = std::mem::replace(&mut self.active_profile_id, profile_id);
            self.ext_cache.insert(old, std::mem::replace(&mut self.extensions, extensions));
        }
        Ok(())
    }

    /// A new space in the active profile holding no tabs, for tabs about to be moved in; stays in the background.
    pub fn create_empty_session(&mut self) -> anyhow::Result<String> {
        let taken = self.profile_sessions().into_iter().map(|session| session.name.clone()).collect::<Vec<_>>();
        let name = (1..).map(|n| format!("Space {n}")).find(|name| !taken.contains(name)).unwrap_or_default();
        let id = Uuid::new_v4().to_string();
        self.storage.execute(
            "INSERT INTO sessions (id, profile_id, name, active) VALUES (?1, ?2, ?3, 0)",
            params![id, self.active_profile_id, name],
        )?;
        self.reload()?;
        Ok(id)
    }

    /// Moves tabs of the active space into another space, of this or another profile; an emptied space keeps a home tab.
    /// Tabs taken to another profile keep their own, and lose that mark when they come back home.
    pub fn move_tabs_to_session(&mut self, ids: &[String], session_id: &str) -> anyhow::Result<()> {
        let from = self.active_session_id.clone();
        let Some(to_profile) = self.sessions.iter().find(|session| session.id == session_id).map(|session| session.profile_id.clone()) else {
            return Ok(());
        };
        if session_id == from {
            return Ok(());
        }
        let base = self.tabs.iter().filter(|tab| tab.session_id == session_id).map(|tab| tab.position + 1).max().unwrap_or(0);
        let tx = self.storage.conn().unchecked_transaction()?;
        for (index, id) in ids.iter().enumerate() {
            self.storage.execute(
                "UPDATE tabs SET session_id = ?1, active = 0, position = ?2,
                 profile_id = CASE WHEN COALESCE(NULLIF(profile_id, ''), ?5) = ?6 THEN '' ELSE COALESCE(NULLIF(profile_id, ''), ?5) END
                 WHERE id = ?3 AND session_id = ?4",
                params![session_id, base + index as i32, id, from, self.active_profile_id, to_profile],
            )?;
        }
        tx.commit()?;
        self.reload()?;
        if !self.tabs.iter().any(|tab| tab.session_id == from) {
            self.insert_tab(&from, "nxbrowser://home", "New Tab", true, 0, TabState::Suspended)?;
            self.reload()?;
        }
        Ok(())
    }

    pub fn create_tab(&mut self, url: &str) -> anyhow::Result<String> {
        self.create_tab_beside(url, "")
    }

    /// A new tab that shares the profile of tab `beside`, for one opened in that tab's window.
    pub fn create_tab_beside(&mut self, url: &str, beside: &str) -> anyhow::Result<String> {
        let guest = self.tabs.iter().find(|tab| tab.id == beside).map(|tab| tab.profile_id.clone()).unwrap_or_default();
        let session_id = self.active_session_id.clone();
        // Closed and moved tabs leave gaps in positions, so the count could land mid-list.
        let position = self.session_tabs().last().map_or(0, |tab| tab.position + 1);
        self.mark_active_background();
        let id = self.insert_tab(&session_id, url, "New Tab", true, position, TabState::Active)?;
        if !guest.is_empty() {
            self.storage.execute("UPDATE tabs SET profile_id = ?1 WHERE id = ?2", params![guest, id])?;
        }
        self.reload()?;
        self.queue_show_active();
        Ok(id)
    }

    pub fn close_tab(&mut self, tab_id: &str) -> anyhow::Result<()> {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id).cloned() else {
            return Ok(());
        };
        if tab.session_id != self.active_session_id {
            return Ok(());
        }
        let remaining = self.session_tabs().len();
        if tab.url != "nxbrowser://home" && !tab.url.is_empty() {
            self.closed.push((tab.session_id.clone(), tab.url.clone(), tab.layout.clone()));
            if self.closed.len() > 20 {
                self.closed.remove(0);
            }
        }
        self.storage.execute("DELETE FROM tabs WHERE id = ?1", params![tab_id])?;
        self.effects.push(Effect::CloseTab {
            tab_id: tab_id.to_string(),
        });
        if remaining <= 1 {
            self.insert_tab(&self.active_session_id.clone(), "nxbrowser://home", "New Tab", true, 0, TabState::Active)?;
        } else if tab.active {
            self.reload()?;
            if let Some(next) = self.session_tabs().first().map(|tab| tab.id.clone()) {
                self.activate_tab(&next)?;
                return Ok(());
            }
        }
        self.reload()?;
        self.queue_show_active();
        Ok(())
    }

    /// Reopens this session's most recently closed tab, returning its new id.
    pub fn reopen_closed(&mut self) -> anyhow::Result<Option<String>> {
        self.reopen_closed_url(None)
    }

    /// This session's recently closed tab URLs, newest first.
    pub fn closed_urls(&self) -> Vec<String> {
        self.closed.iter().rev().filter(|(session, ..)| *session == self.active_session_id).map(|(_, url, _)| url.clone()).collect()
    }

    /// Like `reopen_closed`, limited to the newest closed tab with `url` when given.
    pub fn reopen_closed_url(&mut self, url: Option<&str>) -> anyhow::Result<Option<String>> {
        let Some(index) = self.closed.iter().rposition(|(session, closed, _)| {
            *session == self.active_session_id && url.is_none_or(|url| url == closed)
        }) else {
            return Ok(None);
        };
        let (_, url, layout) = self.closed.remove(index);
        let id = self.create_tab(&url)?;
        self.set_tab_layout(&id, &layout)?;
        Ok(Some(id))
    }

    pub fn activate_tab(&mut self, tab_id: &str) -> anyhow::Result<()> {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) else {
            return Ok(());
        };
        if tab.session_id != self.active_session_id {
            return Ok(());
        }
        self.mark_active_background();
        self.storage.execute(
            "UPDATE tabs SET active = CASE WHEN id = ?1 THEN 1 ELSE 0 END, state = CASE WHEN id = ?1 THEN 'active' ELSE state END WHERE session_id = ?2",
            params![tab_id, self.active_session_id],
        )?;
        self.storage.execute(
            "UPDATE tabs SET state = 'active' WHERE id = ?1",
            params![tab_id],
        )?;
        self.background_since.retain(|(id, _)| id != tab_id);
        self.reload()?;
        self.queue_show_active();
        Ok(())
    }

    pub fn navigate_active(&mut self, url: &str) -> anyhow::Result<()> {
        let Some(tab_id) = self.active_tab().map(|tab| tab.id.clone()) else {
            return Ok(());
        };
        self.storage.execute(
            "UPDATE tabs SET url = ?1, state = 'active' WHERE id = ?2",
            params![url, tab_id],
        )?;
        self.reload()?;
        self.effects.push(Effect::Navigate {
            tab_id,
            url: url.to_string(),
        });
        Ok(())
    }

    pub fn note_navigation(&mut self, tab_id: &str, url: &str, title: &str) -> anyhow::Result<()> {
        // WebKit reports a nil URL while a page is torn down (e.g. at quit) or after some failed loads;
        // saving that would restore the tab blank.
        if url.is_empty() {
            return Ok(());
        }
        // Title updates and in-page URL events re-report the current URL; they refresh the entry instead of adding one.
        let same_url = self.tabs.iter().any(|tab| tab.id == tab_id && tab.url == url);
        self.storage.execute(
            "UPDATE tabs SET url = ?1, title = ?2 WHERE id = ?3",
            params![url, title, tab_id],
        )?;
        if let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) {
            if let Some(profile_id) = self.tab_profile(&tab.id).map(|profile| profile.id.clone()) {
                if same_url {
                    self.storage.execute(
                        "UPDATE history SET title = ?1 WHERE id = (SELECT MAX(id) FROM history WHERE profile_id = ?2 AND url = ?3)",
                        params![title, profile_id, url],
                    )?;
                } else if self.settings.save_history
                    && !self.profiles.iter().any(|p| p.id == profile_id && p.is_private())
                    && (url.starts_with("http://") || url.starts_with("https://")) {
                    self.storage.execute(
                        "INSERT INTO history (profile_id, url, title, visited_at) VALUES (?1, ?2, ?3, ?4)",
                        params![profile_id, url, title, now_secs()],
                    )?;
                }
            }
        }
        self.set_tab_text(tab_id, Some(url), title);
        Ok(())
    }

    /// Mirrors a tabs-row url/title write into memory; a full `reload` per title tick is wasted work.
    fn set_tab_text(&mut self, tab_id: &str, url: Option<&str>, title: &str) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            if let Some(url) = url {
                tab.url = url.to_string();
            }
            tab.title = title.to_string();
        }
    }

    pub fn set_loading_title(&mut self, tab_id: &str, title: &str) -> anyhow::Result<()> {
        if !title.is_empty() {
            self.storage.execute("UPDATE tabs SET title = ?1 WHERE id = ?2", params![title, tab_id])?;
            self.set_tab_text(tab_id, None, title);
        }
        Ok(())
    }

    pub fn tabs_due_for_suspension(&mut self) -> Vec<String> {
        let now = now_secs();
        let live = self
            .tabs
            .iter()
            .filter(|tab| tab.session_id == self.active_session_id)
            .map(|tab| LiveTab {
                id: tab.id.clone(),
                state: tab.state,
                background_since: self
                    .background_since
                    .iter()
                    .find(|(id, _)| id == &tab.id)
                    .map(|(_, since)| *since),
            })
            .collect::<Vec<_>>();
        suspend_candidates(&live, now, self.settings.suspend_after_secs, self.settings.max_live_tabs)
    }

    /// Restarts a background tab's suspension timer, for tabs still showing on screen.
    pub fn keep_alive(&mut self, tab_id: &str) {
        let now = now_secs();
        for (id, since) in &mut self.background_since {
            if id == tab_id {
                *since = now;
            }
        }
    }

    /// A suspended tab whose page was loaded again (it came on screen): back under the live-tab cap.
    pub fn mark_live(&mut self, tab_id: &str) -> anyhow::Result<()> {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id && tab.state == TabState::Suspended) else {
            return Ok(());
        };
        self.storage.execute("UPDATE tabs SET state = 'background' WHERE id = ?1", params![tab_id])?;
        tab.state = TabState::Background;
        self.background_since.push((tab_id.to_string(), now_secs()));
        Ok(())
    }

    pub fn mark_suspended(&mut self, tab_id: &str) -> anyhow::Result<()> {
        self.storage.execute(
            "UPDATE tabs SET state = 'suspended' WHERE id = ?1 AND active = 0",
            params![tab_id],
        )?;
        self.background_since.retain(|(id, _)| id != tab_id);
        self.effects.push(Effect::SuspendTab {
            tab_id: tab_id.to_string(),
        });
        self.reload()?;
        Ok(())
    }

    pub fn toggle_bookmark(&mut self) -> anyhow::Result<bool> {
        let Some(tab) = self.active_tab().cloned() else {
            return Ok(false);
        };
        let profile_id = self.active_profile_id.clone();
        let existing: Option<String> = self
            .storage
            .conn()
            .query_row(
                "SELECT id FROM bookmarks WHERE profile_id = ?1 AND url = ?2",
                params![profile_id, tab.url],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = existing {
            self.storage.execute("DELETE FROM bookmarks WHERE id = ?1", params![id])?;
            return Ok(false);
        }
        self.storage.execute(
            "INSERT INTO bookmarks (id, profile_id, url, title, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![Uuid::new_v4().to_string(), profile_id, tab.url, tab.title, now_secs()],
        )?;
        Ok(true)
    }

    /// Newest first. Memoized: the star button and new-tab page ask every frame.
    pub fn bookmarks(&self) -> anyhow::Result<Vec<Bookmark>> {
        memo(&self.bookmarks_memo, self.memo_key("", 0), || {
            let mut stmt = self.storage.conn().prepare_cached(
                "SELECT id, profile_id, url, title, created_at, pinned FROM bookmarks WHERE profile_id = ?1 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map(params![self.active_profile_id], |row| {
                Ok(Bookmark {
                    id: row.get(0)?,
                    profile_id: row.get(1)?,
                    url: row.get(2)?,
                    title: row.get(3)?,
                    created_at: row.get(4)?,
                    pinned: row.get(5)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
    }

    /// Edits bookmark `id`, or the one already saved for `url`, or adds a new one.
    pub fn save_bookmark(&mut self, id: Option<&str>, url: &str, title: &str, pinned: bool) -> anyhow::Result<()> {
        let existing = id.map(str::to_string).or_else(|| self.bookmarks().ok()?.into_iter().find(|b| b.url == url).map(|b| b.id));
        match existing {
            Some(id) => self.storage.execute(
                "UPDATE bookmarks SET url = ?2, title = ?3, pinned = ?4 WHERE id = ?1",
                params![id, url, title, pinned],
            )?,
            None => self.storage.execute(
                "INSERT INTO bookmarks (id, profile_id, url, title, created_at, pinned) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![Uuid::new_v4().to_string(), self.active_profile_id, url, title, now_secs(), pinned],
            )?,
        };
        Ok(())
    }

    pub fn delete_bookmark(&mut self, id: &str) -> anyhow::Result<()> {
        self.storage.execute("DELETE FROM bookmarks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn history(&self) -> anyhow::Result<Vec<HistoryEntry>> {
        self.search_history("", 40)
    }

    /// Recent visits, or those whose title or URL contains `query`. Blank query skips the filter.
    pub fn search_history(&self, query: &str, limit: usize) -> anyhow::Result<Vec<HistoryEntry>> {
        let query = query.trim();
        let key = self.memo_key(query, limit);
        memo(&self.history_memo, key, || {
            let limit = limit as i64;
            let rows = if query.is_empty() {
                let mut stmt = self.storage.conn().prepare_cached(
                    "SELECT id, profile_id, url, title, visited_at FROM history WHERE profile_id = ?1 ORDER BY visited_at DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![self.active_profile_id, limit], read_history)?.collect::<Result<Vec<_>, _>>()?;
                rows
            } else {
                let pattern = like_pattern(query);
                let mut stmt = self.storage.conn().prepare_cached(
                    "SELECT id, profile_id, url, title, visited_at FROM history WHERE profile_id = ?1 AND (url LIKE ?2 ESCAPE '\\' OR title LIKE ?2 ESCAPE '\\') ORDER BY visited_at DESC LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![self.active_profile_id, pattern, limit], read_history)?.collect::<Result<Vec<_>, _>>()?;
                rows
            };
            Ok(rows)
        })
    }

    /// Address bar matches as (url, title): bookmarks first, then history by recency, one row per url.
    pub fn suggest(&self, query: &str, limit: usize) -> anyhow::Result<Vec<(String, String)>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let key = self.memo_key(query, limit);
        memo(&self.suggest_memo, key, || {
            let pattern = like_pattern(query);
            let mut stmt = self.storage.conn().prepare_cached(
                "SELECT url, title, MIN(rank), MAX(at) FROM (
                    SELECT url, title, 0 AS rank, created_at AS at FROM bookmarks WHERE profile_id = ?1
                    UNION ALL
                    SELECT url, title, 1 AS rank, visited_at AS at FROM history WHERE profile_id = ?1
                 ) WHERE url LIKE ?2 ESCAPE '\\' OR title LIKE ?2 ESCAPE '\\'
                 GROUP BY url ORDER BY MIN(rank), MAX(at) DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![self.active_profile_id, pattern, limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
    }

    fn memo_key(&self, query: &str, limit: usize) -> MemoKey {
        (self.storage.generation(), self.active_profile_id.clone(), query.to_string(), limit)
    }

    pub fn downloads(&self) -> anyhow::Result<Vec<DownloadRecord>> {
        let mut stmt = self.storage.conn().prepare_cached(
            "SELECT id, profile_id, url, path, status, created_at FROM downloads WHERE profile_id = ?1 ORDER BY created_at DESC LIMIT 30",
        )?;
        let rows = stmt.query_map(params![self.active_profile_id], |row| {
            Ok(DownloadRecord {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                url: row.get(2)?,
                path: row.get(3)?,
                status: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Removes finished and failed downloads from the active profile's list; running ones stay.
    pub fn clear_finished_downloads(&self) -> anyhow::Result<()> {
        self.storage.execute(
            "DELETE FROM downloads WHERE profile_id = ?1 AND status != 'running'",
            params![self.active_profile_id],
        )?;
        Ok(())
    }

    pub fn add_download(&self, id: &str, url: &str, path: &str) -> anyhow::Result<()> {
        self.storage.execute(
            "INSERT INTO downloads (id, profile_id, url, path, status, created_at) VALUES (?1, ?2, ?3, ?4, 'running', ?5)",
            params![id, self.active_profile_id, url, path, now_secs()],
        )?;
        Ok(())
    }

    pub fn update_download(&self, id: &str, status: &str, path: &str) -> anyhow::Result<()> {
        self.storage.execute(
            "UPDATE downloads SET status = ?1, path = ?2 WHERE id = ?3",
            params![status, path, id],
        )?;
        Ok(())
    }

    pub fn permission(&self, origin: &str, permission: &str) -> anyhow::Result<Option<String>> {
        let value = self.storage.conn().query_row(
            "SELECT decision FROM permissions WHERE profile_id = ?1 AND origin = ?2 AND permission = ?3",
            params![self.active_profile_id, origin, permission],
            |row| row.get(0),
        );
        match value {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub fn set_permission(&self, origin: &str, permission: &str, decision: &str) -> anyhow::Result<()> {
        self.storage.execute(
            "INSERT INTO permissions (profile_id, origin, permission, decision) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(profile_id, origin, permission) DO UPDATE SET decision = excluded.decision",
            params![self.active_profile_id, origin, permission, decision],
        )?;
        Ok(())
    }

    pub fn remove_permission(&self, origin: &str, permission: &str) -> anyhow::Result<()> {
        self.storage.execute(
            "DELETE FROM permissions WHERE profile_id = ?1 AND origin = ?2 AND permission = ?3",
            params![self.active_profile_id, origin, permission],
        )?;
        Ok(())
    }

    pub fn permissions_map(&self) -> anyhow::Result<Vec<(String, String, String)>> {
        let mut stmt = self.storage.conn().prepare_cached(
            "SELECT origin, permission, decision FROM permissions WHERE profile_id = ?1",
        )?;
        let rows = stmt.query_map(params![self.active_profile_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Adds or updates an installed extension, keeping its enabled and pinned flags.
    pub fn upsert_extension(&mut self, id: &str, name: &str, path: &Path, source: &str) -> anyhow::Result<()> {
        self.storage.execute(
            "INSERT INTO extensions (id, path, enabled, manifest_json, source) VALUES (?1, ?2, 1, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET path = excluded.path, manifest_json = excluded.manifest_json, source = excluded.source",
            params![id, path.display().to_string(), serde_json::json!({ "name": name }).to_string(), source],
        )?;
        self.storage.execute(
            "INSERT OR IGNORE INTO profile_extensions (profile_id, extension_id) VALUES (?1, ?2)",
            params![self.active_profile_id, id],
        )?;
        self.extensions = query_extensions(&self.storage, &self.active_profile_id)?;
        Ok(())
    }

    /// Removes the extension from the active profile; true when no profile has it any more.
    pub fn remove_extension(&mut self, id: &str) -> anyhow::Result<bool> {
        self.storage.execute(
            "DELETE FROM profile_extensions WHERE profile_id = ?1 AND extension_id = ?2",
            params![self.active_profile_id, id],
        )?;
        self.extensions.retain(|extension| extension.id != id);
        let last = !self
            .storage
            .conn()
            .prepare_cached("SELECT 1 FROM profile_extensions WHERE extension_id = ?1")?
            .exists(params![id])?;
        if last {
            self.storage.execute("DELETE FROM extensions WHERE id = ?1", params![id])?;
        }
        Ok(last)
    }

    /// Data stores of every profile that has this extension installed and enabled.
    pub fn extension_stores(&self, id: &str) -> anyhow::Result<Vec<String>> {
        let mut stmt = self.storage.conn().prepare_cached(
            "SELECT p.data_store_id FROM profile_extensions l JOIN profiles p ON p.id = l.profile_id
             WHERE l.extension_id = ?1 AND l.enabled = 1",
        )?;
        let rows = stmt.query_map(params![id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Every extension installed in any profile: id, folder and source.
    pub fn installed_extensions(&self) -> anyhow::Result<Vec<(String, PathBuf, String)>> {
        let mut stmt = self.storage.conn().prepare_cached("SELECT id, path, source FROM extensions ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?), row.get(2)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_extension_enabled(&mut self, id: &str, enabled: bool) -> anyhow::Result<()> {
        self.storage.execute(
            "UPDATE profile_extensions SET enabled = ?1 WHERE profile_id = ?2 AND extension_id = ?3",
            params![enabled as i32, self.active_profile_id, id],
        )?;
        if let Some(extension) = self.extensions.iter_mut().find(|extension| extension.id == id) {
            extension.enabled = enabled;
        }
        Ok(())
    }

    pub fn set_extension_pinned(&mut self, id: &str, pinned: bool) -> anyhow::Result<()> {
        self.storage.execute(
            "UPDATE profile_extensions SET pinned = ?1 WHERE profile_id = ?2 AND extension_id = ?3",
            params![pinned as i32, self.active_profile_id, id],
        )?;
        if let Some(extension) = self.extensions.iter_mut().find(|extension| extension.id == id) {
            extension.pinned = pinned;
        }
        Ok(())
    }

    pub fn export_active(&self) -> anyhow::Result<ProfileBundle> {
        let profile = self.active_profile().context("no profile")?;
        let tabs = self
            .session_tabs()
            .into_iter()
            .map(|tab| BundleTab {
                url: tab.url.clone(),
                title: tab.title.clone(),
                active: tab.active,
            })
            .collect();
        let bookmarks = self
            .bookmarks()?
            .into_iter()
            .map(|item| BundleBookmark { url: item.url, title: item.title })
            .collect();
        let history = self
            .history()?
            .into_iter()
            .map(|item| BundleHistory {
                url: item.url,
                title: item.title,
                visited_at: item.visited_at,
            })
            .collect();
        let permissions = self
            .permissions_map()?
            .into_iter()
            .map(|(origin, permission, decision)| BundlePermission {
                origin,
                permission,
                decision,
            })
            .collect();
        Ok(ProfileBundle {
            profile: BundleProfile {
                name: profile.name.clone(),
                data_store_id: profile.data_store_id.clone(),
                icon: profile.icon.clone(),
                color: profile.color.clone(),
            },
            tabs,
            bookmarks,
            history,
            permissions,
        })
    }

    pub fn import_bundle(&mut self, bundle: ProfileBundle) -> anyhow::Result<String> {
        let id = self.create_profile(&bundle.profile.name)?;
        // Reuse the bundle's store (restoring a backup keeps its logins) only when no profile has it: deleting
        // one of two profiles sharing a store would wipe the other's site data.
        let store = Some(bundle.profile.data_store_id.as_str())
            .filter(|store| Uuid::parse_str(store).is_ok() && !self.profiles.iter().any(|profile| profile.data_store_id == *store));
        // One transaction: a bundle can carry thousands of history rows.
        let tx = self.storage.conn().unchecked_transaction()?;
        self.storage.execute(
            "UPDATE profiles SET data_store_id = COALESCE(?1, data_store_id), icon = ?2, color = ?3 WHERE id = ?4",
            params![store, bundle.profile.icon, bundle.profile.color, id],
        )?;
        let session_id = self.active_session_id.clone();
        self.storage.execute("DELETE FROM tabs WHERE session_id = ?1", params![session_id])?;
        if bundle.tabs.is_empty() {
            self.insert_tab(&session_id, "nxbrowser://home", "New Tab", true, 0, TabState::Suspended)?;
        }
        for (index, tab) in bundle.tabs.iter().enumerate() {
            self.insert_tab(
                &session_id,
                &tab.url,
                &tab.title,
                tab.active || index == 0,
                index as i32,
                TabState::Suspended,
            )?;
        }
        for bookmark in bundle.bookmarks {
            self.storage.execute(
                "INSERT INTO bookmarks (id, profile_id, url, title, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![Uuid::new_v4().to_string(), id, bookmark.url, bookmark.title, now_secs()],
            )?;
        }
        for entry in bundle.history {
            self.storage.execute(
                "INSERT INTO history (profile_id, url, title, visited_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, entry.url, entry.title, entry.visited_at],
            )?;
        }
        for permission in bundle.permissions {
            self.set_permission(&permission.origin, &permission.permission, &permission.decision)?;
        }
        tx.commit()?;
        self.reload()?;
        self.queue_show_active();
        Ok(id)
    }

    fn queue_show_active(&mut self) {
        let tab = self.active_tab().cloned();
        let store = tab.as_ref().and_then(|tab| self.tab_profile(&tab.id)).map(|profile| profile.data_store_id.clone());
        if let (Some(tab), Some(data_store_id)) = (tab, store) {
            self.effects.push(Effect::EnsureTab {
                tab_id: tab.id.clone(),
                data_store_id,
                url: tab.url.clone(),
            });
            self.effects.push(Effect::ShowTab {
                tab_id: Some(tab.id),
            });
        } else {
            self.effects.push(Effect::ShowTab { tab_id: None });
        }
    }

    fn mark_active_background(&mut self) {
        let now = now_secs();
        let previous: Vec<String> = self
            .tabs
            .iter()
            .filter(|tab| tab.session_id == self.active_session_id && tab.active)
            .map(|tab| tab.id.clone())
            .collect();
        for id in &previous {
            if !self.background_since.iter().any(|(tab_id, _)| tab_id == id) {
                self.background_since.push((id.clone(), now));
            }
            let _ = self.storage.execute(
                "UPDATE tabs SET active = 0, state = 'background' WHERE id = ?1",
                params![id],
            );
        }
    }

    fn focus_session_tab(&mut self) -> anyhow::Result<()> {
        let active = self
            .tabs
            .iter()
            .find(|tab| tab.session_id == self.active_session_id && tab.active)
            .map(|tab| tab.id.clone());
        let chosen = active.or_else(|| {
            self.tabs
                .iter()
                .find(|tab| tab.session_id == self.active_session_id)
                .map(|tab| tab.id.clone())
        });
        if let Some(id) = chosen {
            self.storage.execute(
                "UPDATE tabs SET active = CASE WHEN id = ?1 THEN 1 ELSE 0 END WHERE session_id = ?2",
                params![id, self.active_session_id],
            )?;
            self.storage.execute("UPDATE tabs SET state = 'active' WHERE id = ?1", params![id])?;
        }
        Ok(())
    }

    fn insert_profile(&self, profile: &Profile) -> anyhow::Result<()> {
        self.storage.execute(
            "INSERT INTO profiles (id, name, storage_path, data_store_id, created_at, icon, color, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, (SELECT COALESCE(MAX(position), -1) + 1 FROM profiles))",
            params![
                profile.id,
                profile.name,
                profile.storage_path.display().to_string(),
                profile.data_store_id,
                profile.created_at,
                profile.icon,
                profile.color
            ],
        )?;
        Ok(())
    }

    fn insert_session(&self, profile_id: &str, name: &str) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        self.storage.execute(
            "UPDATE sessions SET active = 0 WHERE profile_id = ?1",
            params![profile_id],
        )?;
        self.storage.execute(
            "INSERT INTO sessions (id, profile_id, name, active) VALUES (?1, ?2, ?3, 1)",
            params![id, profile_id, name],
        )?;
        Ok(id)
    }

    fn insert_tab(
        &self,
        session_id: &str,
        url: &str,
        title: &str,
        active: bool,
        position: i32,
        state: TabState,
    ) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        if active {
            self.storage.execute(
                "UPDATE tabs SET active = 0 WHERE session_id = ?1",
                params![session_id],
            )?;
        }
        self.storage.execute(
            "INSERT INTO tabs (id, session_id, url, title, active, position, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, session_id, url, title, active as i32, position, state.as_str()],
        )?;
        Ok(id)
    }

    fn save_active(&self) -> anyhow::Result<()> {
        self.storage.execute(
            "INSERT INTO active_context (id, profile_id, session_id) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET profile_id = excluded.profile_id, session_id = excluded.session_id",
            params![self.active_profile_id, self.active_session_id],
        )?;
        Ok(())
    }

    fn reload(&mut self) -> anyhow::Result<()> {
        self.profiles = query_profiles(&self.storage)?;
        self.sessions = query_sessions(&self.storage)?;
        self.tabs = query_tabs(&self.storage)?;
        // Only at open: afterwards the in-memory ids lead, and windows pin their own (see `set_active`).
        if let Ok((profile_id, session_id)) = self.storage.conn().query_row(
            "SELECT profile_id, session_id FROM active_context WHERE id = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ) {
            if self.active_profile_id.is_empty() && self.profiles.iter().any(|profile| profile.id == profile_id) {
                self.active_profile_id = profile_id;
            }
            if self.active_session_id.is_empty() && self.sessions.iter().any(|session| session.id == session_id) {
                self.active_session_id = session_id;
            }
        }
        if self.active_profile_id.is_empty() {
            if let Some(profile) = self.profiles.first() {
                self.active_profile_id = profile.id.clone();
            }
        }
        if self.active_session_id.is_empty()
            || !self.sessions.iter().any(|session| {
                session.id == self.active_session_id && session.profile_id == self.active_profile_id
            })
        {
            if let Some(session) = self
                .sessions
                .iter()
                .find(|session| session.profile_id == self.active_profile_id)
            {
                self.active_session_id = session.id.clone();
            }
        }
        self.ext_cache.clear();
        self.extensions = query_extensions(&self.storage, &self.active_profile_id)?;
        Ok(())
    }
}

fn like_pattern(query: &str) -> String {
    format!("%{}%", query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

fn read_history(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        url: row.get(2)?,
        title: row.get(3)?,
        visited_at: row.get(4)?,
    })
}

fn query_extensions(storage: &Storage, profile_id: &str) -> anyhow::Result<Vec<ExtensionRecord>> {
    let mut stmt = storage.conn().prepare_cached(
        "SELECT e.id, e.path, l.enabled, e.manifest_json, e.source, l.pinned
         FROM extensions e JOIN profile_extensions l ON l.extension_id = e.id AND l.profile_id = ?1
         ORDER BY e.rowid",
    )?;
    let rows = stmt.query_map(params![profile_id], |row| {
        let manifest: String = row.get(3)?;
        let name = serde_json::from_str::<serde_json::Value>(&manifest).ok().and_then(|json| json["name"].as_str().map(str::to_string));
        Ok(ExtensionRecord {
            id: row.get(0)?,
            path: PathBuf::from(row.get::<_, String>(1)?),
            enabled: row.get::<_, i32>(2)? != 0,
            name: name.unwrap_or_default(),
            source: row.get(4)?,
            pinned: row.get::<_, i32>(5)? != 0,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn query_profiles(storage: &Storage) -> anyhow::Result<Vec<Profile>> {
    let mut stmt = storage.conn().prepare_cached(
        "SELECT id, name, storage_path, data_store_id, created_at, icon, color FROM profiles ORDER BY position, created_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Profile {
            id: row.get(0)?,
            name: row.get(1)?,
            storage_path: PathBuf::from(row.get::<_, String>(2)?),
            data_store_id: row.get(3)?,
            created_at: row.get(4)?,
            icon: row.get(5)?,
            color: row.get(6)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn query_sessions(storage: &Storage) -> anyhow::Result<Vec<Session>> {
    let mut stmt = storage
        .conn()
        .prepare_cached("SELECT id, profile_id, name, active, free FROM sessions")?;
    let rows = stmt.query_map([], |row| {
        Ok(Session {
            id: row.get(0)?,
            profile_id: row.get(1)?,
            name: row.get(2)?,
            active: row.get::<_, i32>(3)? != 0,
            free: row.get::<_, i32>(4)? != 0,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn query_tabs(storage: &Storage) -> anyhow::Result<Vec<Tab>> {
    let mut stmt = storage.conn().prepare_cached(
        "SELECT id, session_id, url, title, active, position, state, layout, profile_id FROM tabs ORDER BY position",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Tab {
            id: row.get(0)?,
            session_id: row.get(1)?,
            url: row.get(2)?,
            title: row.get(3)?,
            active: row.get::<_, i32>(4)? != 0,
            position: row.get(5)?,
            state: TabState::parse(&row.get::<_, String>(6)?),
            layout: row.get(7)?,
            profile_id: row.get(8)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspended_tab_shown_again_counts_toward_the_live_cap() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let first = model.active_tab().unwrap().id.clone();
        model.create_tab("https://example.com").unwrap();
        model.mark_suspended(&first).unwrap();
        model.mark_live(&first).unwrap();
        let tab = model.tabs.iter().find(|tab| tab.id == first).unwrap();
        assert_eq!(tab.state, TabState::Background);
        assert!(model.background_since.iter().any(|(id, _)| id == &first));
        // Persisted, so a reopen agrees.
        model.reload().unwrap();
        assert_eq!(model.tabs.iter().find(|tab| tab.id == first).unwrap().state, TabState::Background);
    }

    #[test]
    fn unreadable_settings_are_backed_up_before_defaults() {
        let storage = Storage::open_in_memory().unwrap();
        storage.set_setting("prefs", r#"{"page_zoom": "not a number"}"#).unwrap();
        let model = Model::open(storage).unwrap();
        assert_eq!(model.storage.setting("prefs.bak").unwrap().as_deref(), Some(r#"{"page_zoom": "not a number"}"#));
    }

    #[test]
    fn import_never_shares_a_live_profiles_data_store() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let bundle = model.export_active().unwrap();
        let original = bundle.profile.data_store_id.clone();
        let imported = model.import_bundle(bundle).unwrap();
        let store = |model: &Model, id: &str| model.profiles.iter().find(|p| p.id == id).unwrap().data_store_id.clone();
        assert_ne!(store(&model, &imported), original);

        // A backup whose store no profile uses (original deleted) gets it back.
        let mut bundle = model.export_active().unwrap();
        let unused = Uuid::new_v4().to_string();
        bundle.profile.data_store_id = unused.clone();
        let restored = model.import_bundle(bundle).unwrap();
        assert_eq!(store(&model, &restored), unused);
    }

    #[test]
    fn two_profiles_keep_separate_ids_and_restore() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let first = model.active_profile().unwrap().data_store_id.clone();
        let second = {
            model.create_profile("Work").unwrap();
            model.active_profile().unwrap().data_store_id.clone()
        };
        assert_ne!(first, second);
        model.create_tab("https://example.com").unwrap();
        let path = std::env::temp_dir().join(format!("nxbrowser-test-{}.sqlite", Uuid::new_v4()));
        let mut disk = Model::open(Storage::open(&path).unwrap()).unwrap();
        disk.create_profile("Other").unwrap();
        disk.navigate_active("https://example.org").unwrap();
        let profile_id = disk.active_profile_id.clone();
        let url = disk.active_tab().unwrap().url.clone();
        drop(disk);
        let mut loaded = Model::open(Storage::open(&path).unwrap()).unwrap();
        assert!(loaded.profiles.iter().any(|profile| profile.id == profile_id));
        loaded.switch_profile(&profile_id).unwrap();
        assert_eq!(loaded.active_tab().unwrap().url, url);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn private_profile_keeps_no_history_and_is_gone_after_relaunch() {
        let path = std::env::temp_dir().join(format!("nxbrowser-test-{}.sqlite", Uuid::new_v4()));
        let mut disk = Model::open(Storage::open(&path).unwrap()).unwrap();
        let id = disk.create_private_profile().unwrap();
        assert!(disk.active_profile().unwrap().is_private());
        let tab = disk.create_tab("https://example.com/").unwrap();
        disk.note_navigation(&tab, "https://example.org/", "Example").unwrap();
        assert!(disk.history().unwrap().is_empty());
        drop(disk);
        let loaded = Model::open(Storage::open(&path).unwrap()).unwrap();
        assert!(loaded.profiles.iter().all(|profile| profile.id != id));
        assert!(!loaded.active_profile().unwrap().is_private());
        assert!(loaded.active_tab().is_some());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn moved_tabs_change_space_and_leave_a_home_tab() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let from = model.active_session_id.clone();
        let ids = model.session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        model.create_session().unwrap();
        let to = model.active_session_id.clone();
        model.switch_session(&from).unwrap();
        model.move_tabs_to_session(&ids, &to).unwrap();
        assert!(ids.iter().all(|id| model.tabs.iter().any(|tab| tab.id == *id && tab.session_id == to)));
        let left = model.tabs.iter().filter(|tab| tab.session_id == from).collect::<Vec<_>>();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].url, "nxbrowser://home");
    }

    #[test]
    fn moved_tab_takes_the_target_place() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        model.create_tab("https://a.com/").unwrap();
        model.create_tab("https://b.com/").unwrap();
        let ids = model.session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        model.move_tab(&ids[0], 2).unwrap();
        let order = model.session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        assert_eq!(order, vec![ids[1].clone(), ids[2].clone(), ids[0].clone()]);
    }

    #[test]
    fn new_tab_opens_last_after_a_close() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let a = model.create_tab("https://a.com/").unwrap();
        model.create_tab("https://b.com/").unwrap();
        model.close_tab(&a).unwrap();
        let c = model.create_tab("https://c.com/").unwrap();
        assert_eq!(model.session_tabs().last().unwrap().id, c);
    }

    #[test]
    fn empty_navigation_url_keeps_the_tab() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let id = model.active_tab().unwrap().id.clone();
        model.note_navigation(&id, "https://www.google.com/search?q=x", "x - Google Search").unwrap();
        model.note_navigation(&id, "", "").unwrap();
        model.reload().unwrap();
        let tab = model.active_tab().unwrap();
        assert_eq!((tab.url.as_str(), tab.title.as_str()), ("https://www.google.com/search?q=x", "x - Google Search"));
    }

    #[test]
    fn closed_tab_reopens_with_its_layout() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let id = model.create_tab("https://example.com/").unwrap();
        model.set_tab_layout(&id, "{\"x\":1}").unwrap();
        model.close_tab(&id).unwrap();
        let again = model.reopen_closed().unwrap().unwrap();
        let tab = model.tabs.iter().find(|tab| tab.id == again).unwrap();
        assert_eq!((tab.url.as_str(), tab.layout.as_str()), ("https://example.com/", "{\"x\":1}"));
        assert!(model.reopen_closed().unwrap().is_none());
    }

    #[test]
    fn navigation_updates_memory_like_a_reload() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let tab = model.active_tab().unwrap().id.clone();
        model.set_loading_title(&tab, "Loading").unwrap();
        model.note_navigation(&tab, "https://example.com/", "Example").unwrap();
        model.note_navigation(&tab, "https://example.com/", "Example 2").unwrap();
        let live = model.active_tab().map(|tab| (tab.url.clone(), tab.title.clone()));
        model.reload().unwrap();
        assert_eq!(live, model.active_tab().map(|tab| (tab.url.clone(), tab.title.clone())));
        assert_eq!(model.history().unwrap().len(), 1, "same url refreshes the entry");
    }

    #[test]
    fn suggestions_rank_bookmarks_then_recent_history() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let tab = model.active_tab().unwrap().id.clone();
        model.note_navigation(&tab, "https://rust-lang.org/", "Rust").unwrap();
        model.note_navigation(&tab, "https://docs.rs/", "Docs rust").unwrap();
        model.note_navigation(&tab, "https://example.com/", "Example").unwrap();
        model.navigate_active("https://rust-lang.org/").unwrap();
        model.note_navigation(&tab, "https://rust-lang.org/", "Rust").unwrap();
        model.toggle_bookmark().unwrap();
        let urls: Vec<String> = model.suggest("rust", 5).unwrap().into_iter().map(|(url, _)| url).collect();
        assert_eq!(urls, ["https://rust-lang.org/", "https://docs.rs/"]);
        assert!(model.suggest("100%", 5).unwrap().is_empty());
    }

    #[test]
    fn pinning_reuses_the_bookmark_and_edits_stick() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        model.save_bookmark(None, "https://a.com/", "A", false).unwrap();
        model.save_bookmark(None, "https://a.com/", "A", true).unwrap();
        let saved = model.bookmarks().unwrap();
        assert_eq!(saved.len(), 1, "same url is one bookmark");
        assert!(saved[0].pinned);
        model.save_bookmark(Some(&saved[0].id), "https://b.com/", "B", true).unwrap();
        assert_eq!(model.bookmarks().unwrap()[0].url, "https://b.com/");
        model.delete_bookmark(&saved[0].id).unwrap();
        assert!(model.bookmarks().unwrap().is_empty());
    }

    #[test]
    fn search_history_filters_title_and_url() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let tab = model.active_tab().unwrap().id.clone();
        model.note_navigation(&tab, "https://example.com/docs", "Example Docs").unwrap();
        model.note_navigation(&tab, "https://rust-lang.org/", "Rust").unwrap();
        model.note_navigation(&tab, "https://docs.rs/percent", "Crate").unwrap();
        let urls: Vec<String> = model.search_history("rust", 10).unwrap().into_iter().map(|entry| entry.url).collect();
        assert_eq!(urls, ["https://rust-lang.org/"]);
        assert_eq!(model.search_history("docs", 10).unwrap().len(), 2);
        assert!(model.search_history("100%", 10).unwrap().is_empty());
        assert_eq!(model.search_history("", 1).unwrap()[0].url, "https://docs.rs/percent");
    }

    #[test]
    fn extensions_install_per_profile() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let first = model.active_profile_id.clone();
        model.upsert_extension("ext", "Ext", Path::new("/x"), "store").unwrap();
        model.create_profile("Work").unwrap();
        assert!(model.extensions.is_empty(), "new profile starts without extensions");
        model.upsert_extension("ext", "Ext", Path::new("/x"), "store").unwrap();
        model.set_extension_enabled("ext", false).unwrap();
        assert_eq!(model.extension_stores("ext").unwrap().len(), 1);
        assert!(!model.remove_extension("ext").unwrap(), "first profile still has it");
        model.switch_profile(&first).unwrap();
        assert_eq!(model.extensions.len(), 1);
        assert!(model.remove_extension("ext").unwrap());
        assert!(model.installed_extensions().unwrap().is_empty());
    }

    #[test]
    fn appearance_and_settings_persist() {
        let path = std::env::temp_dir().join(format!("nxbrowser-test-{}.sqlite", Uuid::new_v4()));
        let mut model = Model::open(Storage::open(&path).unwrap()).unwrap();
        let id = model.active_profile_id.clone();
        model.set_profile_appearance(&id, "🦊", "#e5484d").unwrap();
        model.update_settings(|s| { s.save_history = false; s.search_engine = "Google".into(); }).unwrap();
        let tab = model.active_tab().unwrap().id.clone();
        model.note_navigation(&tab, "https://example.com/", "Example").unwrap();
        assert!(model.history().unwrap().is_empty());
        drop(model);
        let loaded = Model::open(Storage::open(&path).unwrap()).unwrap();
        let profile = loaded.profiles.iter().find(|p| p.id == id).unwrap();
        assert_eq!((profile.icon.as_str(), profile.color.as_str()), ("🦊", "#e5484d"));
        assert_eq!(loaded.settings.search_engine, "Google");
        assert!(!loaded.settings.save_history);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn closing_a_space_drops_its_tabs_and_keeps_the_last() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let first = model.active_session_id.clone();
        model.create_session().unwrap();
        let second = model.active_session_id.clone();
        assert_ne!(first, second);
        model.create_tab("https://example.com").unwrap();
        assert_eq!(model.session_tabs().len(), 2);
        model.delete_session(&second).unwrap();
        assert_eq!(model.active_session_id, first);
        assert!(model.tabs.iter().all(|tab| tab.session_id != second));
        model.delete_session(&first).unwrap();
        assert_eq!(model.profile_sessions().len(), 1);
    }

    #[test]
    fn profiles_reorder() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        model.create_profile("B").unwrap();
        model.create_profile("C").unwrap();
        let names = |m: &Model| m.profiles.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&model), ["Personal", "B", "C"]);
        let c = model.profiles[2].id.clone();
        model.move_profile(&c, 0).unwrap();
        assert_eq!(names(&model), ["C", "Personal", "B"]);
        model.move_profile(&c, 99).unwrap();
        assert_eq!(names(&model), ["Personal", "B", "C"]);
    }

    #[test]
    fn extension_flags_survive_reinstall_and_reopen() {
        let path = std::env::temp_dir().join(format!("nxbrowser-test-{}.sqlite", Uuid::new_v4()));
        let mut model = Model::open(Storage::open(&path).unwrap()).unwrap();
        model.upsert_extension("abc", "Ext", Path::new("/tmp/ext"), "store").unwrap();
        model.set_extension_pinned("abc", true).unwrap();
        model.set_extension_enabled("abc", false).unwrap();
        model.upsert_extension("abc", "Ext 2", Path::new("/tmp/ext"), "store").unwrap();
        drop(model);
        let mut model = Model::open(Storage::open(&path).unwrap()).unwrap();
        let ext = &model.extensions[0];
        assert_eq!((ext.name.as_str(), ext.enabled, ext.pinned), ("Ext 2", false, true));
        model.remove_extension("abc").unwrap();
        assert!(model.extensions.is_empty());
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn detached_space_keeps_its_tabs_and_pin() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let main = model.active_session_id.clone();
        let moved = model.create_tab("https://example.com/").unwrap();
        let space = model.create_empty_session().unwrap();
        assert_eq!(model.active_session_id, main);
        model.move_tabs_to_session(std::slice::from_ref(&moved), &space).unwrap();
        model.set_active(&space).unwrap();
        assert_eq!(model.session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>(), std::slice::from_ref(&moved));
        // A tab opened while pinned stays in the pinned space even though reload runs.
        let second = model.create_tab("https://rust-lang.org/").unwrap();
        assert_eq!(model.active_session_id, space);
        model.set_active(&main).unwrap();
        assert!(model.session_tabs().iter().all(|tab| tab.id != moved && tab.id != second));
    }
    #[test]
    fn window_moved_to_another_profile_keeps_its_own_until_home() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let (home, home_space) = (model.active_profile_id.clone(), model.active_session_id.clone());
        let tab = model.create_tab("https://example.com/").unwrap();
        let other = model.create_profile("Work").unwrap();
        let other_space = model.active_session_id.clone();
        model.switch_profile(&home).unwrap();
        model.move_tabs_to_session(std::slice::from_ref(&tab), &other_space).unwrap();
        model.switch_profile(&other).unwrap();
        assert_eq!(model.tab_profile(&tab).unwrap().id, home);
        // A tab opened in its window shares its profile.
        let sibling = model.create_tab_beside("https://rust-lang.org/", &tab).unwrap();
        assert_eq!(model.tab_profile(&sibling).unwrap().id, home);
        model.move_tabs_to_session(std::slice::from_ref(&tab), &home_space).unwrap();
        assert!(model.tabs.iter().any(|t| t.id == tab && t.profile_id.is_empty()));
        // Deleting a profile takes its windows parked in other profiles with it.
        model.delete_profile(&home).unwrap();
        assert!(!model.tabs.iter().any(|t| t.id == sibling));
    }
    #[test]
    fn torn_off_window_merges_back_and_its_space_goes() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let main = model.active_session_id.clone();
        let tab = model.create_tab("https://example.com/").unwrap();
        let hidden = model.create_empty_session().unwrap();
        model.move_tabs_to_session(std::slice::from_ref(&tab), &hidden).unwrap();
        model.set_active(&hidden).unwrap();
        model.move_tabs_to_session(std::slice::from_ref(&tab), &main).unwrap();
        model.set_active(&main).unwrap();
        model.delete_session(&hidden).unwrap();
        assert!(model.session_tabs().iter().any(|t| t.id == tab));
        assert!(!model.sessions.iter().any(|s| s.id == hidden));
        assert_eq!(model.active_session_id, main);
    }

    #[test]
    fn profile_flips_reuse_extension_lists_and_keep_edits() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let personal = model.active_session_id.clone();
        let personal_profile = model.active_profile_id.clone();
        model.upsert_extension("ext", "Ext", Path::new("/tmp/ext"), "bundled").unwrap();
        model.set_extension_enabled("ext", false).unwrap();
        model.create_profile("Work").unwrap();
        let work = model.active_session_id.clone();
        assert!(model.extensions.is_empty());
        // A detached window flips back and forth like this on every lock.
        for _ in 0..2 {
            model.set_active(&personal).unwrap();
            assert_eq!(model.extensions.len(), 1);
            assert!(!model.extensions[0].enabled);
            assert_eq!(model.extensions.len(), query_extensions(&model.storage, &personal_profile).unwrap().len());
            model.set_active(&work).unwrap();
            assert!(model.extensions.is_empty());
        }
    }

    #[test]
    fn deleting_the_active_profile_loads_the_next_profiles_extensions() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        model.upsert_extension("ext", "Ext", Path::new("/tmp/ext"), "bundled").unwrap();
        model.create_profile("Work").unwrap();
        let work = model.active_profile_id.clone();
        model.delete_profile(&work).unwrap();
        assert_eq!(model.extensions.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["ext"]);
    }

    #[test]
    fn history_search_sees_new_visits_despite_the_memo() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        let tab = model.active_tab().unwrap().id.clone();
        model.note_navigation(&tab, "https://example.com/a", "A").unwrap();
        assert_eq!(model.search_history("example", 10).unwrap().len(), 1);
        assert_eq!(model.suggest("example", 10).unwrap().len(), 1);
        model.note_navigation(&tab, "https://example.com/b", "B").unwrap();
        assert_eq!(model.search_history("example", 10).unwrap().len(), 2);
        assert_eq!(model.suggest("example", 10).unwrap().len(), 2);
    }

    #[test]
    fn old_history_is_pruned_on_open() {
        let path = std::env::temp_dir().join(format!("nxbrowser-test-{}.sqlite", Uuid::new_v4()));
        let model = Model::open(Storage::open(&path).unwrap()).unwrap();
        let old = now_secs() - (HISTORY_DAYS + 1) * 24 * 60 * 60;
        for (url, at) in [("https://old.example/", old), ("https://new.example/", now_secs())] {
            model
                .storage
                .execute(
                    "INSERT INTO history (profile_id, url, title, visited_at) VALUES (?1, ?2, '', ?3)",
                    params![model.active_profile_id, url, at],
                )
                .unwrap();
        }
        drop(model);
        let model = Model::open(Storage::open(&path).unwrap()).unwrap();
        let urls = model.history().unwrap().into_iter().map(|entry| entry.url).collect::<Vec<_>>();
        assert_eq!(urls, ["https://new.example/"]);
        let _ = std::fs::remove_file(path);
    }
}
