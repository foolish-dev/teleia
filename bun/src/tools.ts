import { mkdir, readFile, writeFile } from "node:fs/promises"
import { dirname } from "node:path"
import { $ } from "bun"

import type { ToolDef } from "./llm"

export function definitions(): ToolDef[] {
  return [
    {
      type: "function",
      function: {
        name: "read",
        description: "Read a file from disk. Returns the file contents as text.",
        parameters: {
          type: "object",
          properties: { path: { type: "string" } },
          required: ["path"],
        },
      },
    },
    {
      type: "function",
      function: {
        name: "write",
        description: "Write contents to a file, creating or overwriting it.",
        parameters: {
          type: "object",
          properties: { path: { type: "string" }, content: { type: "string" } },
          required: ["path", "content"],
        },
      },
    },
    {
      type: "function",
      function: {
        name: "edit",
        description:
          "Replace a unique substring in a file. Fails if old_string is missing or non-unique.",
        parameters: {
          type: "object",
          properties: {
            path: { type: "string" },
            old_string: { type: "string" },
            new_string: { type: "string" },
          },
          required: ["path", "old_string", "new_string"],
        },
      },
    },
    {
      type: "function",
      function: {
        name: "bash",
        description: "Run a shell command and return its combined stdout/stderr. 30s timeout.",
        parameters: {
          type: "object",
          properties: { command: { type: "string" } },
          required: ["command"],
        },
      },
    },
  ]
}

export async function dispatch(name: string, argumentsJson: string): Promise<string> {
  const args = JSON.parse(argumentsJson || "{}") as Record<string, string>

  switch (name) {
    case "read": {
      return await readFile(args.path!, "utf8")
    }
    case "write": {
      const parent = dirname(args.path!)
      if (parent && parent !== "." && parent !== "") {
        await mkdir(parent, { recursive: true }).catch(() => {})
      }
      await writeFile(args.path!, args.content!)
      return `wrote ${args.content!.length} bytes to ${args.path}`
    }
    case "edit": {
      const text = await readFile(args.path!, "utf8")
      const old = args.old_string!
      const occurrences = text.split(old).length - 1
      if (occurrences === 0) throw new Error(`old_string not found in ${args.path}`)
      if (occurrences > 1) {
        throw new Error(`old_string matches ${occurrences} times in ${args.path}; needs to be unique`)
      }
      const updated = text.replace(old, args.new_string!)
      await writeFile(args.path!, updated)
      return `edited ${args.path}`
    }
    case "bash": {
      const result = await $`timeout 30 bash -lc ${args.command!} 2>&1`.quiet().nothrow()
      let out = result.stdout.toString()
      if (result.exitCode === 124) {
        out += `\n[bash timed out after 30s]`
      } else if (result.exitCode !== 0) {
        out += `\n[exit ${result.exitCode}]`
      }
      return out
    }
    default:
      throw new Error(`unknown tool: ${name}`)
  }
}
