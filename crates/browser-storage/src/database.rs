use anyhow::Context;
use rusqlite::{params, Connection};
use std::cell::Cell;
use std::path::Path;

pub struct Storage {
    conn: Connection,
    /// Bumped on every write, so cached reads can tell they went stale.
    writes: Cell<u64>,
}

impl Storage {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // WAL + NORMAL: writes run on the UI thread, so skip the per-commit fsync of the rollback journal.
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        // `execute` caches every statement; the default 16 slots would thrash across the model's ~60 queries.
        conn.set_prepared_statement_cache_capacity(128);
        let storage = Self { conn, writes: Cell::new(0) };
        storage.migrate()?;
        Ok(storage)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let storage = Self {
            conn: Connection::open_in_memory()?,
            writes: Cell::new(0),
        };
        storage.migrate()?;
        Ok(storage)
    }

    fn migrate(&self) -> anyhow::Result<()> {
        let had_links = self
            .conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'profile_extensions'")?
            .exists([])?;
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS profiles (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                storage_path TEXT NOT NULL,
                data_store_id TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                profile_id TEXT NOT NULL,
                name TEXT NOT NULL,
                active INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS tabs (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                url TEXT NOT NULL,
                title TEXT NOT NULL,
                active INTEGER NOT NULL,
                position INTEGER NOT NULL,
                state TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS extensions (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                manifest_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS profile_extensions (
                profile_id TEXT NOT NULL,
                extension_id TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                pinned INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (profile_id, extension_id)
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                profile_id TEXT NOT NULL,
                url TEXT NOT NULL,
                title TEXT NOT NULL,
                visited_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS bookmarks (
                id TEXT PRIMARY KEY,
                profile_id TEXT NOT NULL,
                url TEXT NOT NULL,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS downloads (
                id TEXT PRIMARY KEY,
                profile_id TEXT NOT NULL,
                url TEXT NOT NULL,
                path TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS permissions (
                profile_id TEXT NOT NULL,
                origin TEXT NOT NULL,
                permission TEXT NOT NULL,
                decision TEXT NOT NULL,
                PRIMARY KEY (profile_id, origin, permission)
            );
            CREATE TABLE IF NOT EXISTS active_context (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                profile_id TEXT NOT NULL,
                session_id TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS passwords (
                id TEXT PRIMARY KEY,
                profile_id TEXT NOT NULL,
                origin TEXT NOT NULL,
                username TEXT NOT NULL,
                secret BLOB NOT NULL,
                note TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                last_used_at INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS password_never (
                profile_id TEXT NOT NULL,
                origin TEXT NOT NULL,
                PRIMARY KEY (profile_id, origin)
            );
            CREATE INDEX IF NOT EXISTS passwords_profile_origin ON passwords (profile_id, origin);
            CREATE INDEX IF NOT EXISTS history_profile_url ON history (profile_id, url);
            CREATE INDEX IF NOT EXISTS history_profile_visited ON history (profile_id, visited_at);
            CREATE INDEX IF NOT EXISTS tabs_session ON tabs (session_id);
            CREATE INDEX IF NOT EXISTS bookmarks_profile ON bookmarks (profile_id);
            ",
        )?;
        // Columns added after the first release; older databases get them here.
        for (table, column, decl) in [
            ("profiles", "icon", "TEXT NOT NULL DEFAULT ''"),
            ("profiles", "color", "TEXT NOT NULL DEFAULT ''"),
            ("profiles", "position", "INTEGER NOT NULL DEFAULT 0"),
            ("tabs", "layout", "TEXT NOT NULL DEFAULT ''"),
            ("tabs", "profile_id", "TEXT NOT NULL DEFAULT ''"),
            ("sessions", "free", "INTEGER NOT NULL DEFAULT 0"),
            ("extensions", "source", "TEXT NOT NULL DEFAULT 'bundled'"),
            ("extensions", "pinned", "INTEGER NOT NULL DEFAULT 0"),
            ("bookmarks", "pinned", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            let has = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2")?
                .exists(params![table, column])?;
            if !has {
                self.conn.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {decl}"
                ))?;
            }
        }
        // Extensions used to be global; the first time links exist, give every profile what was installed.
        if !had_links {
            self.conn.execute_batch(
                "INSERT OR IGNORE INTO profile_extensions (profile_id, extension_id, enabled, pinned)
                 SELECT p.id, e.id, e.enabled, e.pinned FROM profiles p, extensions e",
            )?;
        }
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.writes.set(self.writes.get() + 1);
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Count of writes so far; any change means rows may differ from an earlier read.
    pub fn generation(&self) -> u64 {
        self.writes.get()
    }

    pub fn execute(&self, sql: &str, params: impl rusqlite::Params) -> anyhow::Result<usize> {
        self.writes.set(self.writes.get() + 1);
        self.conn.prepare_cached(sql)?.execute(params).context("sqlite execute")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_profiles_table_gains_new_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE profiles (id TEXT PRIMARY KEY, name TEXT NOT NULL, storage_path TEXT NOT NULL, data_store_id TEXT NOT NULL, created_at INTEGER NOT NULL);
             INSERT INTO profiles VALUES ('a', 'Old', '/x', 'd', 1);
             CREATE TABLE tabs (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, active INTEGER NOT NULL, position INTEGER NOT NULL, state TEXT NOT NULL);
             INSERT INTO tabs VALUES ('t', 's', 'u', 'T', 1, 0, 'active');",
        )
        .unwrap();
        let storage = Storage { conn, writes: Cell::new(0) };
        storage.migrate().unwrap();
        storage.migrate().unwrap();
        let (icon, color): (String, String) = storage
            .conn()
            .query_row("SELECT icon, color FROM profiles WHERE id = 'a'", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!((icon.as_str(), color.as_str()), ("", ""));
        let layout: String = storage.conn().query_row("SELECT layout FROM tabs WHERE id = 't'", [], |row| row.get(0)).unwrap();
        assert_eq!(layout, "");
    }

    #[test]
    fn file_database_uses_wal() {
        let path = std::env::temp_dir().join(format!("nx-wal-{}.db", std::process::id()));
        let storage = Storage::open(&path).unwrap();
        let mode: String = storage.conn().query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
        assert_eq!(mode, "wal");
        drop(storage);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }
}
