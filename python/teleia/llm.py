from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Iterator
from urllib import error, request

DEFAULT_BASE_URL = "http://127.0.0.1:11434/v1"


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


@dataclass
class ContentDelta:
    text: str


@dataclass
class StreamDone:
    tool_calls: list[ToolCall]


StreamEvent = ContentDelta | StreamDone


class LlmClient:
    def __init__(self, base_url: str, model: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.model = model

    def stream(
        self, messages: list[Message], tools: list[dict[str, Any]] | None = None
    ) -> Iterator[StreamEvent]:
        body: dict[str, Any] = {
            "model": self.model,
            "messages": [m.to_wire() for m in messages],
            "stream": True,
        }
        if tools:
            body["tools"] = tools
        data = json.dumps(body).encode("utf-8")
        req = request.Request(
            f"{self.base_url}/chat/completions",
            data=data,
            headers={"content-type": "application/json"},
            method="POST",
        )

        accumulated: dict[int, dict[str, str]] = {}

        try:
            resp = request.urlopen(req, timeout=300)
        except error.HTTPError as e:
            raise RuntimeError(f"ollama {e.code}: {e.read().decode('utf-8', 'replace')}") from e

        with resp:
            for raw_line in resp:
                line = raw_line.decode("utf-8", "replace").rstrip("\n").rstrip("\r")
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if not payload or payload == "[DONE]":
                    continue
                try:
                    chunk = json.loads(payload)
                except json.JSONDecodeError:
                    continue
                for choice in chunk.get("choices") or []:
                    delta = choice.get("delta") or {}
                    text = delta.get("content")
                    if text:
                        yield ContentDelta(text)
                    for tcd in delta.get("tool_calls") or []:
                        _accumulate(accumulated, tcd)

        tool_calls = [
            ToolCall(
                id=acc.get("id", ""),
                name=acc.get("name", ""),
                arguments=acc.get("arguments", ""),
            )
            for _, acc in sorted(accumulated.items())
        ]
        yield StreamDone(tool_calls)


def _accumulate(acc: dict[int, dict[str, str]], delta: dict[str, Any]) -> None:
    idx = int(delta.get("index", 0))
    slot = acc.setdefault(idx, {"id": "", "name": "", "arguments": ""})
    if delta.get("id"):
        slot["id"] = delta["id"]
    fn = delta.get("function") or {}
    if fn.get("name"):
        slot["name"] = fn["name"]
    if fn.get("arguments") is not None:
        slot["arguments"] += fn["arguments"]
