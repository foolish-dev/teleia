use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::{params, Connection};
use std::path::PathBuf;
use teleia_llm::Message;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open() -> Result<Self> {
        let path = data_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("mkdir {parent:?}"))?;
        }
        let conn = Connection::open(&path).with_context(|| format!("open {path:?}"))?;
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
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);",
        )?;
        Ok(Self { conn })
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
}

fn data_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "foolish", "teleia")
        .context("could not resolve project directories")?;
    Ok(dirs.data_dir().join("teleia.sqlite"))
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
