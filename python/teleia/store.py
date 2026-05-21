from __future__ import annotations

import json
import os
import sqlite3
import time
import uuid
from pathlib import Path

from .llm import Message, ToolCall


def _data_path() -> Path:
    base = os.environ.get("XDG_DATA_HOME") or os.path.join(os.path.expanduser("~"), ".local", "share")
    return Path(base) / "teleia" / "teleia.sqlite"


class Store:
    def __init__(self) -> None:
        path = _data_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(path)
        self.conn.executescript(
            """
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
            """
        )
        self.conn.commit()

    def create_session(self, model: str) -> str:
        sid = uuid.uuid4().hex
        self.conn.execute(
            "INSERT INTO sessions (id, model, created_at) VALUES (?, ?, ?)",
            (sid, model, int(time.time())),
        )
        self.conn.commit()
        return sid

    def append(self, session_id: str, seq: int, message: Message) -> None:
        payload = json.dumps(_to_jsonable(message))
        self.conn.execute(
            "INSERT INTO messages (session_id, seq, payload) VALUES (?, ?, ?)",
            (session_id, seq, payload),
        )
        self.conn.commit()

    def load(self, session_id: str) -> list[Message]:
        rows = self.conn.execute(
            "SELECT payload FROM messages WHERE session_id = ? ORDER BY seq ASC",
            (session_id,),
        ).fetchall()
        return [_from_jsonable(json.loads(p)) for (p,) in rows]

    def save_alias(self, name: str, session_id: str) -> None:
        self.conn.execute(
            "INSERT OR REPLACE INTO aliases (name, session_id, created_at) VALUES (?, ?, ?)",
            (name, session_id, int(time.time())),
        )
        self.conn.commit()

    def resolve_alias(self, name: str) -> str:
        row = self.conn.execute(
            "SELECT session_id FROM aliases WHERE name = ?", (name,)
        ).fetchone()
        if row is None:
            raise RuntimeError(f"no session saved as '{name}'")
        return row[0]


def _to_jsonable(m: Message) -> dict:
    out: dict = {"role": m.role}
    if m.content is not None:
        out["content"] = m.content
    if m.tool_calls:
        out["tool_calls"] = [{"id": tc.id, "name": tc.name, "arguments": tc.arguments} for tc in m.tool_calls]
    if m.tool_call_id is not None:
        out["tool_call_id"] = m.tool_call_id
    return out


def _from_jsonable(raw: dict) -> Message:
    return Message(
        role=raw["role"],
        content=raw.get("content"),
        tool_calls=[ToolCall(id=tc["id"], name=tc["name"], arguments=tc["arguments"]) for tc in raw.get("tool_calls", [])],
        tool_call_id=raw.get("tool_call_id"),
    )


__all__ = ["Store", "Message", "ToolCall"]
