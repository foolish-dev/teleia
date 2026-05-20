import React, { useState } from "react"
import { Box, Text, useApp, useInput, render } from "ink"

import type { Agent, Step } from "./agent"

type Entry =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "tool"; name: string; arguments: string; output: string }
  | { kind: "error"; text: string }

function Log({ entries }: { entries: Entry[] }) {
  return (
    <Box flexDirection="column">
      {entries.map((e, i) => {
        if (e.kind === "user") {
          return (
            <Box key={i} flexDirection="column" marginBottom={1}>
              <Text bold color="cyan">
                you
              </Text>
              <Text>{e.text}</Text>
            </Box>
          )
        }
        if (e.kind === "assistant") {
          return (
            <Box key={i} flexDirection="column" marginBottom={1}>
              <Text bold color="magenta">
                teleia
              </Text>
              <Text>{e.text}</Text>
            </Box>
          )
        }
        if (e.kind === "tool") {
          const lines = e.output.split("\n").slice(0, 20)
          return (
            <Box key={i} flexDirection="column" marginBottom={1}>
              <Text color="yellow">
                ⚙ {e.name}({e.arguments})
              </Text>
              {lines.map((l, j) => (
                <Text key={j} color="gray">
                  {"  "}
                  {l}
                </Text>
              ))}
            </Box>
          )
        }
        return (
          <Box key={i} marginBottom={1}>
            <Text color="red">error: {e.text}</Text>
          </Box>
        )
      })}
    </Box>
  )
}

function App({ agent }: { agent: Agent }) {
  const { exit } = useApp()
  const [entries, setEntries] = useState<Entry[]>([])
  const [input, setInput] = useState("")
  const [status, setStatus] = useState(
    `session ${agent.sessionId.slice(0, 12)} ready · enter to send · ctrl-c to quit`,
  )
  const [working, setWorking] = useState(false)

  useInput((ch, key) => {
    if (key.ctrl && ch === "c") {
      exit()
      return
    }
    if (working) return
    if (key.return) {
      const prompt = input.trim()
      if (!prompt) return
      setInput("")
      setEntries((prev) => [...prev, { kind: "user", text: prompt }])
      setStatus("thinking…")
      setWorking(true)
      void (async () => {
        try {
          const steps = await agent.turn(prompt)
          setEntries((prev) => [...prev, ...steps.map(stepToEntry)])
          setStatus("ready")
        } catch (e) {
          setEntries((prev) => [...prev, { kind: "error", text: (e as Error).message }])
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

  return (
    <Box flexDirection="column">
      <Log entries={entries} />
      <Box borderStyle="round" paddingX={1}>
        <Text color="cyan">{"> "}</Text>
        <Text dimColor={working}>{input}</Text>
      </Box>
      <Text color="gray" dimColor>
        {status}
      </Text>
    </Box>
  )
}

function stepToEntry(s: Step): Entry {
  if (s.kind === "assistant") return { kind: "assistant", text: s.text }
  return { kind: "tool", name: s.name, arguments: s.arguments, output: s.output }
}

export function run(agent: Agent): void {
  render(<App agent={agent} />)
}
