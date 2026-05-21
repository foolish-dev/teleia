import React, { useCallback, useEffect, useRef, useState } from "react"
import { Box, Text, useApp, useInput, render } from "ink"

import type { Agent } from "./agent"

const HINTS = "enter send · ↑↓ scroll · /help cmds · ctrl-c quit"

// Tokyo Night palette
const TN = {
  cyan: "#7dcfff",
  purple: "#bb9af7",
  yellow: "#e0af68",
  red: "#f7768e",
  blue: "#7aa2f7",
  dim: "#565f89",
} as const

type Entry =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; complete: boolean }
  | { kind: "tool"; name: string; arguments: string; output: string; complete: boolean }
  | { kind: "error"; text: string }
  | { kind: "info"; text: string }

function shortId(s: string): string {
  return s.length > 12 ? s.slice(0, 12) : s
}

function applyEvent(entries: Entry[], ev: import("./agent").TurnEvent): Entry[] {
  const next = [...entries]
  switch (ev.kind) {
    case "assistant_start":
      next.push({ kind: "assistant", text: "", complete: false })
      return next
    case "assistant_delta": {
      const last = next[next.length - 1]
      if (last && last.kind === "assistant" && !last.complete) {
        next[next.length - 1] = { ...last, text: last.text + ev.text }
      } else {
        next.push({ kind: "assistant", text: ev.text, complete: false })
      }
      return next
    }
    case "assistant_end": {
      const last = next[next.length - 1]
      if (last && last.kind === "assistant") {
        if (last.text === "") {
          next.pop()
        } else {
          next[next.length - 1] = { ...last, complete: true }
        }
      }
      return next
    }
    case "tool_start":
      next.push({
        kind: "tool",
        name: ev.name,
        arguments: ev.arguments,
        output: "",
        complete: false,
      })
      return next
    case "tool_end": {
      const last = next[next.length - 1]
      if (last && last.kind === "tool") {
        next[next.length - 1] = { ...last, output: ev.output, complete: true }
      }
      return next
    }
    default:
      return next
  }
}

function EntryView({ e }: { e: Entry }) {
  if (e.kind === "user") {
    return (
      <Box flexDirection="column" marginBottom={1}>
        <Text bold color={TN.cyan}>you</Text>
        <Text>{e.text}</Text>
      </Box>
    )
  }
  if (e.kind === "assistant") {
    return (
      <Box flexDirection="column" marginBottom={1}>
        <Text bold color={TN.purple}>{e.complete ? "teleia" : "teleia ▌"}</Text>
        <Text>{e.text}</Text>
      </Box>
    )
  }
  if (e.kind === "tool") {
    const lines = e.output.split("\n").slice(0, 20)
    const marker = e.complete ? "⚙" : "⚙ …"
    return (
      <Box flexDirection="column" marginBottom={1}>
        <Text color={TN.yellow}>{`${marker} ${e.name}(${e.arguments})`}</Text>
        {lines.map((l, i) => (
          <Text key={i} color={TN.dim}>{`  ${l}`}</Text>
        ))}
      </Box>
    )
  }
  if (e.kind === "error") {
    return (
      <Box marginBottom={1}><Text color={TN.red}>{`error: ${e.text}`}</Text></Box>
    )
  }
  return (
    <Box marginBottom={1}><Text color={TN.blue}>{`· ${e.text}`}</Text></Box>
  )
}

function App({ agent }: { agent: Agent }) {
  const { exit } = useApp()
  const [entries, setEntries] = useState<Entry[]>([])
  const [input, setInput] = useState("")
  const [status, setStatus] = useState(`session ${shortId(agent.sessionId)} · ready`)
  const [working, setWorking] = useState(false)
  // scroll offset (number of lines from bottom; 0 = follow). We don't actually
  // window the view; ink hands rendering to the terminal scrollback.
  const scrollRef = useRef(0)

  const push = useCallback((e: Entry) => {
    setEntries((prev) => [...prev, e])
  }, [])

  const handleSlash = useCallback(
    (cmd: string) => {
      const [name, ...rest] = cmd.split(/\s+/)
      const arg = rest.join(" ").trim()
      try {
        if (name === "reset") {
          agent.reset()
          setEntries([])
          push({ kind: "info", text: `started new session ${shortId(agent.sessionId)}` })
        } else if (name === "save") {
          if (!arg) {
            push({ kind: "error", text: "usage: /save NAME" })
            return
          }
          agent.saveAlias(arg)
          push({ kind: "info", text: `saved current session as '${arg}'` })
        } else if (name === "load") {
          if (!arg) {
            push({ kind: "error", text: "usage: /load NAME" })
            return
          }
          const id = agent.loadAlias(arg)
          setEntries([])
          push({ kind: "info", text: `loaded '${arg}' → session ${shortId(id)}` })
        } else if (name === "help" || name === "?") {
          push({ kind: "info", text: "commands: /reset · /save NAME · /load NAME · /help" })
        } else {
          push({ kind: "error", text: `unknown command: /${name}` })
        }
      } catch (e) {
        push({ kind: "error", text: `${name}: ${(e as Error).message}` })
      }
    },
    [agent, push],
  )

  useInput((ch, key) => {
    if (key.ctrl && ch === "c") {
      exit()
      return
    }
    if (working) return
    if (key.upArrow) { scrollRef.current += 1; return }
    if (key.downArrow) { scrollRef.current = Math.max(0, scrollRef.current - 1); return }
    if (key.pageUp) { scrollRef.current += 5; return }
    if (key.pageDown) { scrollRef.current = Math.max(0, scrollRef.current - 5); return }
    if (key.return) {
      const raw = input.trim()
      setInput("")
      if (!raw) return
      if (raw.startsWith("/")) {
        handleSlash(raw.slice(1))
        return
      }
      push({ kind: "user", text: raw })
      setStatus("thinking…")
      setWorking(true)
      void (async () => {
        try {
          for await (const ev of agent.turn(raw)) {
            setEntries((prev) => applyEvent(prev, ev))
            if (ev.kind === "turn_end") break
          }
          setStatus(`session ${shortId(agent.sessionId)} · ready`)
        } catch (e) {
          push({ kind: "error", text: (e as Error).message })
          setStatus("error · ready")
        } finally {
          setWorking(false)
        }
      })()
      return
    }
    if (key.backspace || key.delete) {
      setInput((prev) => prev.slice(0, -1))
      return
    }
    if (ch && !key.ctrl && !key.meta) {
      setInput((prev) => prev + ch)
    }
  })

  useEffect(() => {
    scrollRef.current = 0
  }, [entries])

  return (
    <Box flexDirection="column">
      {entries.map((e, i) => (
        <EntryView key={i} e={e} />
      ))}
      <Box borderStyle="round" paddingX={1}>
        <Text color={TN.cyan}>{"> "}</Text>
        <Text dimColor={working}>{input}</Text>
      </Box>
      <Text color={TN.dim} dimColor>
        {`${status}   ${HINTS}`}
      </Text>
    </Box>
  )
}

export function run(agent: Agent): void {
  render(<App agent={agent} />)
}
