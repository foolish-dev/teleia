from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

from .llm import ToolDef


def definitions() -> list[ToolDef]:
    return [
        ToolDef(
            "read",
            "Read a file from disk. Returns the file contents as text.",
            {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            },
        ),
        ToolDef(
            "write",
            "Write contents to a file, creating or overwriting it.",
            {
                "type": "object",
                "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
                "required": ["path", "content"],
            },
        ),
        ToolDef(
            "edit",
            "Replace a unique substring in a file. Fails if old_string is missing or non-unique.",
            {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                },
                "required": ["path", "old_string", "new_string"],
            },
        ),
        ToolDef(
            "bash",
            "Run a shell command and return its combined stdout/stderr. 30s timeout.",
            {
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
            },
        ),
    ]


def dispatch(name: str, arguments: str) -> str:
    args = json.loads(arguments or "{}")
    if name == "read":
        return _read(args["path"])
    if name == "write":
        return _write(args["path"], args["content"])
    if name == "edit":
        return _edit(args["path"], args["old_string"], args["new_string"])
    if name == "bash":
        return _bash(args["command"])
    raise ValueError(f"unknown tool: {name}")


def _read(path: str) -> str:
    return Path(path).read_text()


def _write(path: str, content: str) -> str:
    p = Path(path)
    if p.parent and str(p.parent) not in ("", "."):
        p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(content)
    return f"wrote {len(content)} bytes to {path}"


def _edit(path: str, old: str, new: str) -> str:
    text = Path(path).read_text()
    n = text.count(old)
    if n == 0:
        raise RuntimeError(f"old_string not found in {path}")
    if n > 1:
        raise RuntimeError(f"old_string matches {n} times in {path}; needs to be unique")
    Path(path).write_text(text.replace(old, new, 1))
    return f"edited {path}"


def _bash(command: str) -> str:
    try:
        result = subprocess.run(
            ["bash", "-lc", command],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=30,
            env=os.environ.copy(),
        )
    except subprocess.TimeoutExpired as e:
        return (e.stdout or "") + "\n[bash timed out after 30s]"
    out = result.stdout
    if result.returncode != 0:
        out += f"\n[exit {result.returncode}]"
    return out
