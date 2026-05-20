from __future__ import annotations

from dataclasses import dataclass

from . import tools as tools_mod
from .llm import LlmClient, Message
from .store import Store

SYSTEM_PROMPT = (
    "You are Teleia, a terse coding assistant running in a terminal. "
    "Use the provided tools (read, write, edit, bash) to do real work. "
    "Default to brief replies. When you finish a turn, stop — do not narrate."
)
MAX_TOOL_HOPS = 16


@dataclass
class AssistantStep:
    text: str


@dataclass
class ToolStep:
    name: str
    arguments: str
    output: str


Step = AssistantStep | ToolStep


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

    def turn(self, user_input: str) -> list[Step]:
        self._push(Message(role="user", content=user_input))
        steps: list[Step] = []

        for _ in range(MAX_TOOL_HOPS):
            reply = self.llm.chat(self.messages, self.tools)
            self._push(reply)

            if reply.content:
                steps.append(AssistantStep(text=reply.content))

            if not reply.tool_calls:
                return steps

            for call in reply.tool_calls:
                try:
                    output = tools_mod.dispatch(call.name, call.arguments)
                except Exception as e:  # noqa: BLE001
                    output = f"error: {e}"
                steps.append(ToolStep(name=call.name, arguments=call.arguments, output=output))
                self._push(Message(role="tool", tool_call_id=call.id, content=output))

        steps.append(AssistantStep(text=f"[stopped: hit tool-hop limit of {MAX_TOOL_HOPS}]"))
        return steps
