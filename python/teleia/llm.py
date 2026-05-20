from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any
from urllib import request, error

DEFAULT_BASE_URL = "http://127.0.0.1:11434/v1"


@dataclass
class ToolDef:
    name: str
    description: str
    parameters: dict[str, Any]

    def to_wire(self) -> dict[str, Any]:
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            },
        }


@dataclass
class ToolCall:
    id: str
    name: str
    arguments: str


@dataclass
class Message:
    role: str
    content: str | None = None
    tool_calls: list[ToolCall] = field(default_factory=list)
    tool_call_id: str | None = None

    def to_wire(self) -> dict[str, Any]:
        out: dict[str, Any] = {"role": self.role}
        if self.content is not None:
            out["content"] = self.content
        if self.tool_calls:
            out["tool_calls"] = [
                {
                    "id": tc.id,
                    "type": "function",
                    "function": {"name": tc.name, "arguments": tc.arguments},
                }
                for tc in self.tool_calls
            ]
        if self.tool_call_id is not None:
            out["tool_call_id"] = self.tool_call_id
        return out


class LlmClient:
    def __init__(self, base_url: str, model: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.model = model

    def chat(self, messages: list[Message], tools: list[ToolDef] | None = None) -> Message:
        body: dict[str, Any] = {
            "model": self.model,
            "messages": [m.to_wire() for m in messages],
            "stream": False,
        }
        if tools:
            body["tools"] = [t.to_wire() for t in tools]
        data = json.dumps(body).encode("utf-8")
        req = request.Request(
            f"{self.base_url}/chat/completions",
            data=data,
            headers={"content-type": "application/json"},
            method="POST",
        )
        try:
            with request.urlopen(req, timeout=300) as resp:
                payload = json.loads(resp.read())
        except error.HTTPError as e:
            raise RuntimeError(f"ollama {e.code}: {e.read().decode('utf-8', 'replace')}") from e

        choice = (payload.get("choices") or [{}])[0]
        msg = choice.get("message") or {}
        tool_calls = []
        for raw in msg.get("tool_calls") or []:
            fn = raw.get("function") or {}
            tool_calls.append(
                ToolCall(
                    id=raw.get("id", ""),
                    name=fn.get("name", ""),
                    arguments=fn.get("arguments", "{}"),
                )
            )
        content = msg.get("content") or None
        return Message(role="assistant", content=content, tool_calls=tool_calls)
