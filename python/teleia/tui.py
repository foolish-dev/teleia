from __future__ import annotations

import curses
import textwrap
from dataclasses import dataclass
from typing import Iterable

from .agent import (
    Agent,
    AssistantDelta,
    AssistantEnd,
    AssistantStart,
    ToolEnd,
    ToolStart,
    TurnEnd,
)

HINTS = "enter send · ↑↓ scroll · /help cmds · ctrl-c quit"

C_USER = 1
C_ASSISTANT = 2
C_TOOL = 3
C_ERROR = 4
C_STATUS = 5
C_PROMPT = 6
C_INFO = 7


def _init_colors() -> None:
    curses.start_color()
    curses.use_default_colors()
    curses.init_pair(C_USER, curses.COLOR_CYAN, -1)
    curses.init_pair(C_ASSISTANT, curses.COLOR_MAGENTA, -1)
    curses.init_pair(C_TOOL, curses.COLOR_YELLOW, -1)
    curses.init_pair(C_ERROR, curses.COLOR_RED, -1)
    curses.init_pair(C_STATUS, 8, -1)
    curses.init_pair(C_PROMPT, curses.COLOR_CYAN, -1)
    curses.init_pair(C_INFO, curses.COLOR_BLUE, -1)


@dataclass
class UserEntry:
    text: str


@dataclass
class AssistantEntry:
    text: str
    complete: bool = False


@dataclass
class ToolEntry:
    name: str
    arguments: str
    output: str
    complete: bool = False


@dataclass
class ErrorEntry:
    text: str


@dataclass
class InfoEntry:
    text: str


Entry = UserEntry | AssistantEntry | ToolEntry | ErrorEntry | InfoEntry


def _wrap_lines(text: str, width: int) -> Iterable[str]:
    out: list[str] = []
    for line in text.splitlines() or [""]:
        if not line:
            out.append("")
            continue
        out.extend(textwrap.wrap(line, width=max(20, width - 1)) or [""])
    return out


def _entry_lines(entry: Entry, width: int) -> list[tuple[str, int]]:
    lines: list[tuple[str, int]] = []
    if isinstance(entry, UserEntry):
        lines.append(("you", curses.color_pair(C_USER) | curses.A_BOLD))
        for l in _wrap_lines(entry.text, width):
            lines.append((l, 0))
        lines.append(("", 0))
    elif isinstance(entry, AssistantEntry):
        header = "teleia" if entry.complete else "teleia ▌"
        lines.append((header, curses.color_pair(C_ASSISTANT) | curses.A_BOLD))
        for l in _wrap_lines(entry.text, width):
            lines.append((l, 0))
        lines.append(("", 0))
    elif isinstance(entry, ToolEntry):
        marker = "⚙" if entry.complete else "⚙ …"
        lines.append((f"{marker} {entry.name}({entry.arguments})", curses.color_pair(C_TOOL)))
        for l in entry.output.splitlines()[:20]:
            lines.append((f"  {l}", curses.color_pair(C_STATUS)))
        lines.append(("", 0))
    elif isinstance(entry, ErrorEntry):
        lines.append((f"error: {entry.text}", curses.color_pair(C_ERROR)))
        lines.append(("", 0))
    elif isinstance(entry, InfoEntry):
        lines.append((f"· {entry.text}", curses.color_pair(C_INFO)))
        lines.append(("", 0))
    return lines


def _render(
    stdscr,
    history: list[Entry],
    input_buf: str,
    status: str,
    working: bool,
    scroll: int,
) -> None:
    stdscr.erase()
    rows, cols = stdscr.getmaxyx()
    log_h = max(3, rows - 3)
    log_w = cols

    lines: list[tuple[str, int]] = []
    for e in history:
        lines.extend(_entry_lines(e, log_w))

    max_offset = max(0, len(lines) - log_h)
    offset = max(0, max_offset - scroll)
    visible = lines[offset : offset + log_h]
    for i, (text, attr) in enumerate(visible):
        try:
            stdscr.addnstr(i, 0, text, log_w - 1, attr)
        except curses.error:
            pass

    prompt_row = rows - 2
    try:
        stdscr.addnstr(prompt_row, 0, "> ", 2, curses.color_pair(C_PROMPT))
        attr = curses.color_pair(C_STATUS) if working else curses.A_NORMAL
        stdscr.addnstr(prompt_row, 2, input_buf, cols - 3, attr)
    except curses.error:
        pass

    status_line = f"{status}   {HINTS}"
    try:
        stdscr.addnstr(rows - 1, 0, status_line, cols - 1, curses.color_pair(C_STATUS) | curses.A_DIM)
    except curses.error:
        pass

    stdscr.refresh()


def _apply_event(history: list[Entry], evt) -> None:  # noqa: ANN001
    if isinstance(evt, AssistantStart):
        history.append(AssistantEntry(text="", complete=False))
    elif isinstance(evt, AssistantDelta):
        if history and isinstance(history[-1], AssistantEntry) and not history[-1].complete:
            history[-1].text += evt.text
        else:
            history.append(AssistantEntry(text=evt.text, complete=False))
    elif isinstance(evt, AssistantEnd):
        if history and isinstance(history[-1], AssistantEntry):
            history[-1].complete = True
            if not history[-1].text:
                history.pop()
    elif isinstance(evt, ToolStart):
        history.append(ToolEntry(name=evt.name, arguments=evt.arguments, output="", complete=False))
    elif isinstance(evt, ToolEnd):
        if history and isinstance(history[-1], ToolEntry):
            history[-1].output = evt.output
            history[-1].complete = True


def _handle_slash(agent: Agent, cmd: str) -> tuple[str | None, str | None]:
    """Return (info, error) — exactly one of them is non-None."""
    name, _, arg = cmd.partition(" ")
    name = name.strip()
    arg = arg.strip()
    if name == "reset":
        agent.reset()
        return (f"started new session {agent.session_id[:12]}", None)
    if name == "save":
        if not arg:
            return (None, "usage: /save NAME")
        try:
            agent.save_alias(arg)
            return (f"saved current session as '{arg}'", None)
        except Exception as e:  # noqa: BLE001
            return (None, f"save: {e}")
    if name == "load":
        if not arg:
            return (None, "usage: /load NAME")
        try:
            sid = agent.load_alias(arg)
            return (f"loaded '{arg}' → session {sid[:12]}", None)
        except Exception as e:  # noqa: BLE001
            return (None, f"load: {e}")
    if name in ("help", "?"):
        return ("commands: /reset · /save NAME · /load NAME · /help", None)
    return (None, f"unknown command: /{name}")


def run(agent: Agent) -> None:
    def loop(stdscr) -> None:
        curses.curs_set(1)
        _init_colors()
        history: list[Entry] = []
        input_buf = ""
        scroll = 0
        status = f"session {agent.session_id[:12]} · ready"
        working = False

        while True:
            _render(stdscr, history, input_buf, status, working, scroll)
            ch = stdscr.get_wch()
            if isinstance(ch, str):
                if ch == "\x03":  # ctrl-c
                    break
                if ch in ("\n", "\r"):
                    raw = input_buf.strip()
                    input_buf = ""
                    if not raw:
                        continue
                    if raw.startswith("/"):
                        info, err = _handle_slash(agent, raw[1:])
                        if info:
                            if info.startswith("started new session"):
                                history = []
                            elif info.startswith("loaded "):
                                history = []
                            history.append(InfoEntry(text=info))
                        if err:
                            history.append(ErrorEntry(text=err))
                        scroll = 0
                        continue
                    history.append(UserEntry(text=raw))
                    scroll = 0
                    working = True
                    status = "thinking…"
                    _render(stdscr, history, input_buf, status, working, scroll)
                    try:
                        for evt in agent.turn(raw):
                            _apply_event(history, evt)
                            if isinstance(evt, (AssistantDelta, ToolStart, ToolEnd, AssistantEnd)):
                                _render(stdscr, history, input_buf, status, working, scroll)
                            if isinstance(evt, TurnEnd):
                                break
                        status = f"session {agent.session_id[:12]} · ready"
                    except Exception as e:  # noqa: BLE001
                        history.append(ErrorEntry(text=str(e)))
                        status = "error · ready"
                    working = False
                elif ch in ("\x7f", "\b"):
                    input_buf = input_buf[:-1]
                elif ch.isprintable():
                    input_buf += ch
            elif isinstance(ch, int):
                if ch == curses.KEY_UP:
                    scroll += 1
                elif ch == curses.KEY_PPAGE:
                    scroll += 5
                elif ch == curses.KEY_DOWN:
                    scroll = max(0, scroll - 1)
                elif ch == curses.KEY_NPAGE:
                    scroll = max(0, scroll - 5)
                elif ch == curses.KEY_BACKSPACE:
                    input_buf = input_buf[:-1]

    curses.wrapper(loop)
