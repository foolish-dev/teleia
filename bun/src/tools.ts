// Tool definitions + dispatch via the shared rust binary `teleia-tools-bin`.
import type { ToolDef } from "./llm"

const BINARY = "teleia-tools-bin"

export function definitions(): ToolDef[] {
  const result = Bun.spawnSync([BINARY, "defs"], { stdout: "pipe", stderr: "pipe" })
  if (result.exitCode !== 0) {
    throw new Error(`${BINARY} defs: ${result.stderr.toString().trim()}`)
  }
  return JSON.parse(result.stdout.toString()) as ToolDef[]
}

// Run a tool via teleia-tools-bin. The binary always exits 0 and encodes
// tool failures as "error: ..." in stdout. A non-zero exit here means the
// binary itself failed (e.g., not on PATH).
export function dispatch(name: string, argumentsJson: string): string {
  const result = Bun.spawnSync([BINARY, "run", name], {
    stdin: new TextEncoder().encode(argumentsJson),
    stdout: "pipe",
    stderr: "pipe",
  })
  if (result.exitCode !== 0) {
    return `error: ${result.stderr.toString().trim()}`
  }
  return result.stdout.toString()
}
