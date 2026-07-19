use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use teleia_llm::Message;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open() -> Result<Self> {
        Self::open_at(&data_path()?)
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("mkdir {parent:?}"))?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {path:?}"))?;
        // A second connection to the same file (e.g. the SIGUSR1 theme
        // reload opens its own) can collide with an in-flight write.
        // Without a busy timeout SQLite returns SQLITE_BUSY immediately
        // rather than waiting, which can hard-error a turn's persistence.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                model TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq INTEGER NOT NULL,
                payload TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
            CREATE TABLE IF NOT EXISTS aliases (
                name TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS prefs (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS input_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                line TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_input_history_time
                ON input_history(created_at DESC);",
        )?;
        Ok(Self { conn })
    }

    /// Persist a key→value preference (theme, permission_mode, notify…).
    /// Idempotent — same key overwrites.
    pub fn set_pref(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO prefs (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_pref(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM prefs WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Append one submitted input line to the persistent readline
    /// history. Skipped when the line is empty or duplicates the most
    /// recent entry (shell-style dedup).
    pub fn push_input_history(&self, line: &str) -> Result<()> {
        if line.is_empty() {
            return Ok(());
        }
        let last: Option<String> = self
            .conn
            .query_row(
                "SELECT line FROM input_history ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if last.as_deref() == Some(line) {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO input_history (line, created_at) VALUES (?1, ?2)",
            params![line, unix_seconds()],
        )?;
        Ok(())
    }

    /// Recent input history (most recent last), capped at `limit` so an
    /// ancient runaway log doesn't blow startup memory.
    pub fn input_history(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT line FROM (
                SELECT line, id FROM input_history ORDER BY id DESC LIMIT ?1
            ) ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn create_session(&self, model: &str) -> Result<String> {
        let id = ulid::Ulid::new().to_string();
        let now = unix_seconds();
        self.conn.execute(
            "INSERT INTO sessions (id, model, created_at) VALUES (?1, ?2, ?3)",
            params![id, model, now],
        )?;
        Ok(id)
    }

    pub fn append(&self, session_id: &str, seq: usize, message: &Message) -> Result<()> {
        let payload = serde_json::to_string(message)?;
        self.conn.execute(
            "INSERT INTO messages (session_id, seq, payload) VALUES (?1, ?2, ?3)",
            params![session_id, seq as i64, payload],
        )?;
        Ok(())
    }

    pub fn load(&self, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM messages WHERE session_id = ?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let payload = row?;
            let message: Message = serde_json::from_str(&payload)?;
            out.push(message);
        }
        Ok(out)
    }

    pub fn save_alias(&self, name: &str, session_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO aliases (name, session_id, created_at) VALUES (?1, ?2, ?3)",
            params![name, session_id, unix_seconds()],
        )?;
        Ok(())
    }

    pub fn resolve_alias(&self, name: &str) -> Result<String> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT session_id FROM aliases WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()?;
        id.ok_or_else(|| anyhow!("no session saved as '{name}'"))
    }

    pub fn list_aliases(&self) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, session_id, created_at FROM aliases ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn delete_alias(&self, name: &str) -> Result<()> {
        let changed = self
            .conn
            .execute("DELETE FROM aliases WHERE name = ?1", params![name])?;
        if changed == 0 {
            return Err(anyhow!("no alias named '{name}'"));
        }
        Ok(())
    }
}

/// Where to keep the sqlite store. Honours `$XDG_DATA_HOME` if set
/// (works for users who symlink it on macOS too); otherwise falls
/// back to the OS-native data dir:
///
/// - Linux  : `$HOME/.local/share/teleia/teleia.sqlite`
/// - macOS  : `$HOME/Library/Application Support/teleia/teleia.sqlite`
/// - Windows: `%APPDATA%\teleia\teleia.sqlite`
/// - other  : `$HOME/.teleia/teleia.sqlite` as a last resort
fn data_path() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("XDG_DATA_HOME") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v).join("teleia").join("teleia.sqlite"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").context("HOME not set")?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("teleia")
            .join("teleia.sqlite"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA").context("APPDATA not set")?;
        Ok(PathBuf::from(appdata).join("teleia").join("teleia.sqlite"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let home = std::env::var_os("HOME").context("HOME not set")?;
        Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("teleia")
            .join("teleia.sqlite"))
    }
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use teleia_llm::Message;

    fn tmp_db() -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("teleia-test-{}-{}.sqlite", std::process::id(), n))
    }

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn alias_save_resolve_list_delete_roundtrip() {
        let path = tmp_db();
        let _cleanup = Cleanup(path.clone());
        let store = Store::open_at(&path).unwrap();
        let session = store.create_session("test-model").unwrap();

        store.save_alias("foo", &session).unwrap();
        store.save_alias("bar", &session).unwrap();

        assert_eq!(store.resolve_alias("foo").unwrap(), session);
        assert!(store.resolve_alias("missing").is_err());

        let aliases = store.list_aliases().unwrap();
        assert_eq!(aliases.len(), 2);
        let names: Vec<_> = aliases.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));

        store.delete_alias("foo").unwrap();
        assert!(store.resolve_alias("foo").is_err());
        assert!(store.delete_alias("foo").is_err()); // already gone
        assert_eq!(store.list_aliases().unwrap().len(), 1);
    }

    #[test]
    fn save_alias_overwrites_existing() {
        let path = tmp_db();
        let _cleanup = Cleanup(path.clone());
        let store = Store::open_at(&path).unwrap();
        let a = store.create_session("m").unwrap();
        let b = store.create_session("m").unwrap();

        store.save_alias("x", &a).unwrap();
        store.save_alias("x", &b).unwrap();

        assert_eq!(store.resolve_alias("x").unwrap(), b);
        assert_eq!(store.list_aliases().unwrap().len(), 1);
    }

    #[test]
    fn messages_persist_in_seq_order() {
        let path = tmp_db();
        let _cleanup = Cleanup(path.clone());
        let store = Store::open_at(&path).unwrap();
        let session = store.create_session("m").unwrap();

        store
            .append(
                &session,
                0,
                &Message::User {
                    content: "hi".into(),
                },
            )
            .unwrap();
        store
            .append(
                &session,
                1,
                &Message::Assistant {
                    content: Some("hello".into()),
                    tool_calls: vec![],
                },
            )
            .unwrap();

        let messages = store.load(&session).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0], Message::User { content } if content == "hi"));
        assert!(
            matches!(&messages[1], Message::Assistant { content: Some(c), .. } if c == "hello")
        );
    }
}
