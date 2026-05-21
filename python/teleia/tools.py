from __future__ import annotations

import json
import subprocess
from typing import Any

BINARY = "teleia-tools-bin"


def definitions() -> list[dict[str, Any]]:
    """OpenAI-format tool definitions, fetched from the shared rust dispatcher."""
    result = subprocess.run(
        [BINARY, "defs"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


def dispatch(name: str, arguments: str) -> str:
    """Run a tool via teleia-tools-bin. Returns the tool output text (which
    may itself be an `error: ...` string — the binary always exits 0 and
    encodes failures in stdout, matching what every wrapper does)."""
    result = subprocess.run(
        [BINARY, "run", name],
        input=arguments,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        return f"error: {result.stderr.strip()}"
    return result.stdout
