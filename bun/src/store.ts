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
}
