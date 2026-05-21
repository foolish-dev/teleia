import { mkdirSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import { Database } from "bun:sqlite"
import { randomUUID } from "node:crypto"

import type { Message } from "./llm"

function dataPath(): string {
  const base = process.env.XDG_DATA_HOME || join(homedir(), ".local", "share")
  return join(base, "teleia", "teleia.sqlite")
}

export class Store {
  db: Database

  constructor() {
    const path = dataPath()
    mkdirSync(dirname(path), { recursive: true })
    this.db = new Database(path)
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        model TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL,
        seq INTEGER NOT NULL,
        payload TEXT NOT NULL
      );
      CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
      CREATE TABLE IF NOT EXISTS aliases (
        name TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
    `)
  }

  createSession(model: string): string {
    const id = randomUUID().replace(/-/g, "")
    this.db
      .prepare("INSERT INTO sessions (id, model, created_at) VALUES (?, ?, ?)")
      .run(id, model, Math.floor(Date.now() / 1000))
    return id
  }

  append(sessionId: string, seq: number, message: Message): void {
    this.db
      .prepare("INSERT INTO messages (session_id, seq, payload) VALUES (?, ?, ?)")
      .run(sessionId, seq, JSON.stringify(message))
  }

  load(sessionId: string): Message[] {
    const rows = this.db
      .prepare("SELECT payload FROM messages WHERE session_id = ? ORDER BY seq ASC")
      .all(sessionId) as Array<{ payload: string }>
    return rows.map((r) => JSON.parse(r.payload) as Message)
  }

  saveAlias(name: string, sessionId: string): void {
    this.db
      .prepare(
        "INSERT OR REPLACE INTO aliases (name, session_id, created_at) VALUES (?, ?, ?)",
      )
      .run(name, sessionId, Math.floor(Date.now() / 1000))
  }

  resolveAlias(name: string): string {
    const row = this.db
      .prepare("SELECT session_id FROM aliases WHERE name = ?")
      .get(name) as { session_id: string } | null
    if (!row) throw new Error(`no session saved as '${name}'`)
    return row.session_id
  }
}
