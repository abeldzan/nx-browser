use crate::lifecycle::{suspend_candidates, LiveTab, TabState};
use crate::{now_secs, Settings};
use anyhow::Context;
use browser_storage::Storage;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub storage_path: PathBuf,
    pub data_store_id: String,
    pub created_at: i64,
    /// Emoji shown in place of the initial letter; empty means the letter.
    pub icon: String,
    /// `#rrggbb`; empty means the theme accent.
    pub color: String,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub profile_id: String,
    pub name: String,
    pub active: bool,
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
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub enabled: bool,
    pub permissions: Vec<String>,
    pub host_permissions: Vec<String>,
    pub matches: Vec<String>,
    pub content_script: String,
    pub background: Option<String>,
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
    pub extension_storage: Vec<BundleStorage>,
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
pub struct BundleStorage {
    pub extension_id: String,
    pub key: String,
    pub value: String,
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
        };
        model.reload()?;
        if model.profiles.is_empty() {
            model.create_profile("Personal")?;
        }
        if let Some(value) = model.storage.setting("prefs")? {
            model.settings = serde_json::from_str(&value).unwrap_or_default();
        }
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
        let id = Uuid::new_v4().to_string();
        let data_store_id = Uuid::new_v4().to_string();
        let storage_path = crate::app_support_dir().join("profiles").join(&id);
        std::fs::create_dir_all(&storage_path)?;
        let profile = Profile {
            id: id.clone(),
            name: name.to_string(),
            storage_path,
            data_store_id,
            created_at: now_secs(),
            icon: String::new(),
            color: String::new(),
        };
        self.insert_profile(&profile)?;
        let session_id = self.insert_session(&profile.id, "Session 1")?;
        self.insert_tab(&session_id, "nxbrowser://home", "New Tab", true, 0, TabState::Suspended)?;
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
        for (position, id) in ids.iter().enumerate() {
            self.storage.execute("UPDATE profiles SET position = ?1 WHERE id = ?2", params![position as i64, id])?;
        }
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
            .filter(|tab| {
                self.sessions
                    .iter()
                    .any(|session| session.id == tab.session_id && session.profile_id == id)
            })
            .map(|tab| tab.id.clone())
            .collect();
        for tab_id in tab_ids {
            self.effects.push(Effect::CloseTab { tab_id });
        }
        self.storage.execute("DELETE FROM tabs WHERE session_id IN (SELECT id FROM sessions WHERE profile_id = ?1)", params![id])?;
        self.storage.execute("DELETE FROM sessions WHERE profile_id = ?1", params![id])?;
        self.storage.execute("DELETE FROM profiles WHERE id = ?1", params![id])?;
        self.storage.execute("DELETE FROM bookmarks WHERE profile_id = ?1", params![id])?;
        self.storage.execute("DELETE FROM history WHERE profile_id = ?1", params![id])?;
        self.storage.execute("DELETE FROM extension_storage WHERE profile_id = ?1", params![id])?;
        self.storage.execute("DELETE FROM permissions WHERE profile_id = ?1", params![id])?;
        self.reload()?;
        if self.active_profile_id == id {
            self.active_profile_id = self.profiles[0].id.clone();
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
        self.storage.execute("DELETE FROM tabs WHERE session_id = ?1", params![session_id])?;
        self.storage.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        if self.active_session_id == session_id {
            let index = siblings.iter().position(|id| id == session_id).unwrap_or(0);
            let next = siblings.iter().filter(|id| *id != session_id).nth(index.saturating_sub(1)).cloned().unwrap_or_default();
            self.reload()?;
            return self.switch_session(&next);
        }
        self.reload()
    }

    pub fn create_tab(&mut self, url: &str) -> anyhow::Result<String> {
        let session_id = self.active_session_id.clone();
        let position = self.session_tabs().len() as i32;
        self.mark_active_background();
        let id = self.insert_tab(&session_id, url, "New Tab", true, position, TabState::Active)?;
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
        // Title updates and in-page URL events re-report the current URL; they refresh the entry instead of adding one.
        let same_url = self.tabs.iter().any(|tab| tab.id == tab_id && tab.url == url);
        self.storage.execute(
            "UPDATE tabs SET url = ?1, title = ?2 WHERE id = ?3",
            params![url, title, tab_id],
        )?;
        if let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) {
            let profile_id = self
                .sessions
                .iter()
                .find(|session| session.id == tab.session_id)
                .map(|session| session.profile_id.clone());
            if let Some(profile_id) = profile_id {
                if same_url {
                    self.storage.execute(
                        "UPDATE history SET title = ?1 WHERE id = (SELECT MAX(id) FROM history WHERE profile_id = ?2 AND url = ?3)",
                        params![title, profile_id, url],
                    )?;
                } else if self.settings.save_history && (url.starts_with("http://") || url.starts_with("https://")) {
                    self.storage.execute(
                        "INSERT INTO history (profile_id, url, title, visited_at) VALUES (?1, ?2, ?3, ?4)",
                        params![profile_id, url, title, now_secs()],
                    )?;
                }
            }
        }
        self.reload()?;
        Ok(())
    }

    pub fn set_loading_title(&mut self, tab_id: &str, title: &str) -> anyhow::Result<()> {
        if !title.is_empty() {
            self.storage.execute("UPDATE tabs SET title = ?1 WHERE id = ?2", params![title, tab_id])?;
            self.reload()?;
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

    pub fn bookmarks(&self) -> anyhow::Result<Vec<Bookmark>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT id, profile_id, url, title, created_at FROM bookmarks WHERE profile_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![self.active_profile_id], |row| {
            Ok(Bookmark {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                url: row.get(2)?,
                title: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn history(&self) -> anyhow::Result<Vec<HistoryEntry>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT id, profile_id, url, title, visited_at FROM history WHERE profile_id = ?1 ORDER BY visited_at DESC LIMIT 40",
        )?;
        let rows = stmt.query_map(params![self.active_profile_id], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                url: row.get(2)?,
                title: row.get(3)?,
                visited_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn downloads(&self) -> anyhow::Result<Vec<DownloadRecord>> {
        let mut stmt = self.storage.conn().prepare(
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
        let mut stmt = self.storage.conn().prepare(
            "SELECT origin, permission, decision FROM permissions WHERE profile_id = ?1",
        )?;
        let rows = stmt.query_map(params![self.active_profile_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn replace_extensions(&mut self, extensions: Vec<ExtensionRecord>) -> anyhow::Result<()> {
        for extension in &extensions {
            let manifest = serde_json::json!({
                "name": extension.name,
                "permissions": extension.permissions,
                "host_permissions": extension.host_permissions,
            });
            self.storage.execute(
                "INSERT INTO extensions (id, path, enabled, manifest_json) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET path = excluded.path, manifest_json = excluded.manifest_json",
                params![
                    extension.id,
                    extension.path.display().to_string(),
                    extension.enabled as i32,
                    manifest.to_string()
                ],
            )?;
        }
        let mut merged = extensions;
        for extension in &mut merged {
            if let Ok(enabled) = self.storage.conn().query_row(
                "SELECT enabled FROM extensions WHERE id = ?1",
                params![extension.id],
                |row| row.get::<_, i32>(0),
            ) {
                extension.enabled = enabled != 0;
            }
        }
        self.extensions = merged;
        Ok(())
    }

    pub fn set_extension_enabled(&mut self, id: &str, enabled: bool) -> anyhow::Result<()> {
        self.storage.execute(
            "UPDATE extensions SET enabled = ?1 WHERE id = ?2",
            params![enabled as i32, id],
        )?;
        if let Some(extension) = self.extensions.iter_mut().find(|extension| extension.id == id) {
            extension.enabled = enabled;
        }
        Ok(())
    }

    pub fn storage_get(&self, extension_id: &str, key: &str) -> anyhow::Result<Option<String>> {
        let value = self.storage.conn().query_row(
            "SELECT value FROM extension_storage WHERE profile_id = ?1 AND extension_id = ?2 AND key = ?3",
            params![self.active_profile_id, extension_id, key],
            |row| row.get(0),
        );
        match value {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub fn storage_set(&self, extension_id: &str, key: &str, value: &str) -> anyhow::Result<()> {
        self.storage.execute(
            "INSERT INTO extension_storage (profile_id, extension_id, key, value) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(profile_id, extension_id, key) DO UPDATE SET value = excluded.value",
            params![self.active_profile_id, extension_id, key, value],
        )?;
        Ok(())
    }

    pub fn extension_call(&mut self, extension_id: &str, method: &str, args: &serde_json::Value) -> serde_json::Value {
        let Some(extension) = self.extensions.iter().find(|item| item.id == extension_id).cloned() else {
            return error_value("unknown extension");
        };
        if !extension.enabled {
            return error_value("extension disabled");
        }
        match method {
            "storage.get" => {
                if !extension.permissions.iter().any(|item| item == "storage") {
                    return error_value("missing storage permission");
                }
                let key = args.get("key").and_then(|value| value.as_str()).unwrap_or("");
                match self.storage_get(extension_id, key) {
                    Ok(Some(value)) => ok_value(serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value))),
                    Ok(None) => ok_value(serde_json::Value::Null),
                    Err(err) => error_value(&err.to_string()),
                }
            }
            "storage.set" => {
                if !extension.permissions.iter().any(|item| item == "storage") {
                    return error_value("missing storage permission");
                }
                let Some(items) = args.get("items").and_then(|value| value.as_object()) else {
                    return error_value("storage.set expects an object");
                };
                for (key, value) in items {
                    if self.storage_set(extension_id, key, &value.to_string()).is_err() {
                        return error_value("storage write failed");
                    }
                }
                ok_value(serde_json::json!({}))
            }
            "tabs.query" => {
                if !extension.permissions.iter().any(|item| item == "tabs") {
                    return error_value("missing tabs permission");
                }
                let tabs = self
                    .session_tabs()
                    .into_iter()
                    .map(|tab| serde_json::json!({ "id": tab.id, "url": tab.url, "title": tab.title, "active": tab.active }))
                    .collect::<Vec<_>>();
                ok_value(serde_json::Value::Array(tabs))
            }
            "tabs.create" => {
                if !extension.permissions.iter().any(|item| item == "tabs") {
                    return error_value("missing tabs permission");
                }
                let url = args.get("url").and_then(|value| value.as_str()).unwrap_or("nxbrowser://home");
                match self.create_tab(url) {
                    Ok(id) => ok_value(serde_json::json!({ "id": id, "url": url })),
                    Err(err) => error_value(&err.to_string()),
                }
            }
            "tabs.remove" => {
                if !extension.permissions.iter().any(|item| item == "tabs") {
                    return error_value("missing tabs permission");
                }
                let id = args.get("tabId").and_then(|value| value.as_str()).unwrap_or("");
                match self.close_tab(id) {
                    Ok(()) => ok_value(serde_json::json!({})),
                    Err(err) => error_value(&err.to_string()),
                }
            }
            "runtime.sendMessage" => ok_value(args.get("message").cloned().unwrap_or(serde_json::Value::Null)),
            _ => error_value("unknown method"),
        }
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
        let mut stmt = self.storage.conn().prepare(
            "SELECT extension_id, key, value FROM extension_storage WHERE profile_id = ?1",
        )?;
        let extension_storage = stmt
            .query_map(params![profile.id], |row| {
                Ok(BundleStorage {
                    extension_id: row.get(0)?,
                    key: row.get(1)?,
                    value: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
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
            extension_storage,
            permissions,
        })
    }

    pub fn import_bundle(&mut self, bundle: ProfileBundle) -> anyhow::Result<String> {
        let id = self.create_profile(&bundle.profile.name)?;
        self.storage.execute(
            "UPDATE profiles SET data_store_id = ?1, icon = ?2, color = ?3 WHERE id = ?4",
            params![bundle.profile.data_store_id, bundle.profile.icon, bundle.profile.color, id],
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
        for item in bundle.extension_storage {
            self.storage_set(&item.extension_id, &item.key, &item.value)?;
        }
        for permission in bundle.permissions {
            self.set_permission(&permission.origin, &permission.permission, &permission.decision)?;
        }
        self.reload()?;
        self.queue_show_active();
        Ok(id)
    }

    fn queue_show_active(&mut self) {
        let tab = self.active_tab().cloned();
        let store = self.active_profile().map(|profile| profile.data_store_id.clone());
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
        if let Ok((profile_id, session_id)) = self.storage.conn().query_row(
            "SELECT profile_id, session_id FROM active_context WHERE id = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ) {
            if self.profiles.iter().any(|profile| profile.id == profile_id) {
                self.active_profile_id = profile_id;
            }
            if self.sessions.iter().any(|session| session.id == session_id) {
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
        Ok(())
    }
}

fn error_value(message: &str) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": message, "value": null })
}

fn ok_value(value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "ok": true, "error": "", "value": value })
}

fn query_profiles(storage: &Storage) -> anyhow::Result<Vec<Profile>> {
    let mut stmt = storage.conn().prepare(
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
        .prepare("SELECT id, profile_id, name, active FROM sessions")?;
    let rows = stmt.query_map([], |row| {
        Ok(Session {
            id: row.get(0)?,
            profile_id: row.get(1)?,
            name: row.get(2)?,
            active: row.get::<_, i32>(3)? != 0,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn query_tabs(storage: &Storage) -> anyhow::Result<Vec<Tab>> {
    let mut stmt = storage.conn().prepare(
        "SELECT id, session_id, url, title, active, position, state, layout FROM tabs ORDER BY position",
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
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn profile_dir(root: &Path, id: &str) -> PathBuf {
    root.join("profiles").join(id)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn extension_storage_requires_permission() {
        let mut model = Model::open(Storage::open_in_memory().unwrap()).unwrap();
        model
            .replace_extensions(vec![ExtensionRecord {
                id: "ext".into(),
                name: "Ext".into(),
                path: PathBuf::new(),
                enabled: true,
                permissions: vec![],
                host_permissions: vec![],
                matches: vec![],
                content_script: String::new(),
                background: None,
            }])
            .unwrap();
        let denied = model.extension_call("ext", "storage.get", &serde_json::json!({"key": "a"}));
        assert_eq!(denied["ok"], false);
    }
}
