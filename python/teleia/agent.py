from __future__ import annotations

from dataclasses import dataclass
from typing import Iterator

from . import tools as tools_mod
from .llm import ContentDelta, LlmClient, Message, StreamDone, ToolCall
from .store import Store

SYSTEM_PROMPT = (
    "You are Teleia, a terse coding assistant running in a terminal. "
    "Use the provided tools (read, write, edit, bash) to do real work. "
    "Default to brief replies. When you finish a turn, stop — do not narrate."
)
MAX_TOOL_HOPS = 16


class TurnEvent:
    pass


@dataclass
class AssistantStart(TurnEvent):
    pass


@dataclass
class AssistantDelta(TurnEvent):
    text: str


@dataclass
class AssistantEnd(TurnEvent):
    pass


@dataclass
class ToolStart(TurnEvent):
    name: str
    arguments: str


@dataclass
class ToolEnd(TurnEvent):
    name: str
    output: str


@dataclass
class TurnEnd(TurnEvent):
    pass


class Agent:
    def __init__(self, llm: LlmClient, store: Store) -> None:
        self.llm = llm
        self.store = store
        self.tools = tools_mod.definitions()
        self.session_id = store.create_session(llm.model)
        self.messages: list[Message] = []
        self.seq = 0
        self._push(Message(role="system", content=SYSTEM_PROMPT))

    def _push(self, m: Message) -> None:
        self.store.append(self.session_id, self.seq, m)
        self.seq += 1
        self.messages.append(m)

    def reset(self) -> None:
        self.session_id = self.store.create_session(self.llm.model)
        self.messages = []
        self.seq = 0
        self._push(Message(role="system", content=SYSTEM_PROMPT))

    def save_alias(self, name: str) -> None:
        self.store.save_alias(name, self.session_id)

    def load_alias(self, name: str) -> str:
        session_id = self.store.resolve_alias(name)
        self.session_id = session_id
        self.messages = self.store.load(session_id)
        self.seq = len(self.messages)
        return session_id

    def turn(self, user_input: str) -> Iterator[TurnEvent]:
        self._push(Message(role="user", content=user_input))

        for _ in range(MAX_TOOL_HOPS):
            yield AssistantStart()
            content_buf = ""
            tool_calls: list[ToolCall] = []

            for ev in self.llm.stream(self.messages, self.tools):
                if isinstance(ev, ContentDelta):
                    content_buf += ev.text
                    yield AssistantDelta(ev.text)
                elif isinstance(ev, StreamDone):
                    tool_calls = ev.tool_calls

            yield AssistantEnd()
            self._push(
                Message(
                    role="assistant",
                    content=content_buf or None,
                    tool_calls=tool_calls,
                )
            )

            if not tool_calls:
                yield TurnEnd()
                return

            for call in tool_calls:
                yield ToolStart(name=call.name, arguments=call.arguments)
                try:
                    output = tools_mod.dispatch(call.name, call.arguments)
                except Exception as e:  # noqa: BLE001
                    output = f"error: {e}"
                yield ToolEnd(name=call.name, output=output)
                self._push(Message(role="tool", tool_call_id=call.id, content=output))

        yield AssistantDelta(f"[stopped: hit tool-hop limit of {MAX_TOOL_HOPS}]")
        yield TurnEnd()
