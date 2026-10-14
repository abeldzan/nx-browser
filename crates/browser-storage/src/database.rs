use anyhow::Context;
use rusqlite::{params, Connection};
use std::path::Path;

pub struct Storage {
    conn: Connection,
}

impl Storage {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let storage = Self { conn };
        storage.migrate()?;
        Ok(storage)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let storage = Self {
            conn: Connection::open_in_memory()?,
        };
        storage.migrate()?;
        Ok(storage)
    }

    fn migrate(&self) -> anyhow::Result<()> {
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
            CREATE TABLE IF NOT EXISTS extension_storage (
                profile_id TEXT NOT NULL,
                extension_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (profile_id, extension_id, key)
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
            ",
        )?;
        // Columns added after the first release; older databases get them here.
        for (column, decl) in [("icon", "TEXT NOT NULL DEFAULT ''"), ("color", "TEXT NOT NULL DEFAULT ''"), ("position", "INTEGER NOT NULL DEFAULT 0")] {
            let has = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('profiles') WHERE name = ?1")?
                .exists(params![column])?;
            if !has {
                self.conn.execute_batch(&format!(
                    "ALTER TABLE profiles ADD COLUMN {column} {decl}"
                ))?;
            }
        }
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn execute(&self, sql: &str, params: impl rusqlite::Params) -> anyhow::Result<usize> {
        Ok(self.conn.execute(sql, params).context("sqlite execute")?)
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
             INSERT INTO profiles VALUES ('a', 'Old', '/x', 'd', 1);",
        )
        .unwrap();
        let storage = Storage { conn };
        storage.migrate().unwrap();
        storage.migrate().unwrap();
        let (icon, color): (String, String) = storage
            .conn()
            .query_row("SELECT icon, color FROM profiles WHERE id = 'a'", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!((icon.as_str(), color.as_str()), ("", ""));
    }
}
