from __future__ import annotations

import argparse

from .agent import Agent
from .llm import DEFAULT_BASE_URL, LlmClient
from .store import Store
from .tui import run


def main() -> None:
    p = argparse.ArgumentParser(prog="teleia", description="Minimal TUI coding agent (Python)")
    p.add_argument("--model", default="hf.co/FoolDev/Thanatos-27B:Q4_K_M")
    p.add_argument("--base-url", default=DEFAULT_BASE_URL)
    args = p.parse_args()

    llm = LlmClient(args.base_url, args.model)
    store = Store()
    agent = Agent(llm, store)
    run(agent)


if __name__ == "__main__":
    main()
