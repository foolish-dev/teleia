from __future__ import annotations

import curses
import textwrap
from typing import Iterable

from .agent import Agent, AssistantStep, ToolStep, Step

# colour pair ids
C_USER = 1
C_ASSISTANT = 2
C_TOOL = 3
C_ERROR = 4
C_STATUS = 5
C_PROMPT = 6


def _init_colors() -> None:
    curses.start_color()
    curses.use_default_colors()
    curses.init_pair(C_USER, curses.COLOR_CYAN, -1)
    curses.init_pair(C_ASSISTANT, curses.COLOR_MAGENTA, -1)
    curses.init_pair(C_TOOL, curses.COLOR_YELLOW, -1)
    curses.init_pair(C_ERROR, curses.COLOR_RED, -1)
    curses.init_pair(C_STATUS, 8, -1)
    curses.init_pair(C_PROMPT, curses.COLOR_CYAN, -1)


class Entry:
    pass


class UserEntry(Entry):
    def __init__(self, text: str) -> None:
        self.text = text


class AssistantEntry(Entry):
    def __init__(self, text: str) -> None:
        self.text = text


class ToolEntry(Entry):
    def __init__(self, name: str, arguments: str, output: str) -> None:
        self.name = name
        self.arguments = arguments
        self.output = output


class ErrorEntry(Entry):
    def __init__(self, text: str) -> None:
        self.text = text


def _render(stdscr, history: list[Entry], input_buf: str, status: str, working: bool) -> None:
    stdscr.erase()
    rows, cols = stdscr.getmaxyx()
    log_h = rows - 3
    log_w = cols

    lines: list[tuple[str, int]] = []
    for entry in history:
        if isinstance(entry, UserEntry):
            lines.append(("you", curses.color_pair(C_USER) | curses.A_BOLD))
            lines.extend((l, 0) for l in _wrap(entry.text, log_w))
            lines.append(("", 0))
        elif isinstance(entry, AssistantEntry):
            lines.append(("teleia", curses.color_pair(C_ASSISTANT) | curses.A_BOLD))
            lines.extend((l, 0) for l in _wrap(entry.text, log_w))
            lines.append(("", 0))
        elif isinstance(entry, ToolEntry):
            lines.append((f"⚙ {entry.name}({entry.arguments})", curses.color_pair(C_TOOL)))
            for l in entry.output.splitlines()[:20]:
                lines.append((f"  {l}", curses.color_pair(C_STATUS)))
            lines.append(("", 0))
        elif isinstance(entry, ErrorEntry):
            lines.append((f"error: {entry.text}", curses.color_pair(C_ERROR)))
            lines.append(("", 0))

    start = max(0, len(lines) - log_h)
    for i, (text, attr) in enumerate(lines[start : start + log_h]):
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

    try:
        stdscr.addnstr(rows - 1, 0, status, cols - 1, curses.color_pair(C_STATUS) | curses.A_DIM)
    except curses.error:
        pass

    stdscr.refresh()


def _wrap(text: str, width: int) -> Iterable[str]:
    out: list[str] = []
    for line in text.splitlines() or [""]:
        if not line:
            out.append("")
            continue
        out.extend(textwrap.wrap(line, width=max(20, width - 1)) or [""])
    return out


def _to_entry(step: Step) -> Entry:
    if isinstance(step, AssistantStep):
        return AssistantEntry(step.text)
    return ToolEntry(step.name, step.arguments, step.output)


def run(agent: Agent) -> None:
    def loop(stdscr) -> None:
        curses.curs_set(1)
        _init_colors()
        history: list[Entry] = []
        input_buf = ""
        status = f"session {agent.session_id[:12]} ready · enter to send · ctrl-c to quit"
        working = False

        while True:
            _render(stdscr, history, input_buf, status, working)
            ch = stdscr.get_wch()
            if isinstance(ch, str):
                if ch == "\x03":  # ctrl-c
                    break
                if ch in ("\n", "\r"):
                    prompt = input_buf.strip()
                    if not prompt:
                        continue
                    input_buf = ""
                    history.append(UserEntry(prompt))
                    working = True
                    status = "thinking…"
                    _render(stdscr, history, input_buf, status, working)
                    try:
                        for step in agent.turn(prompt):
                            history.append(_to_entry(step))
                        status = "ready"
                    except Exception as e:  # noqa: BLE001
                        history.append(ErrorEntry(str(e)))
                        status = "error · ready"
                    working = False
                elif ch in ("\x7f", "\b"):
                    input_buf = input_buf[:-1]
                elif ch.isprintable():
                    input_buf += ch
            elif isinstance(ch, int):
                if ch in (curses.KEY_BACKSPACE,):
                    input_buf = input_buf[:-1]

    curses.wrapper(loop)
