#!/usr/bin/env bun
import { parseArgs } from "node:util"

import { Agent } from "./agent"
import { DEFAULT_BASE_URL, LlmClient } from "./llm"
import { Store } from "./store"
import { run } from "./tui"

function main(): void {
  const { values } = parseArgs({
    args: process.argv.slice(2),
    options: {
      model: { type: "string", default: "hf.co/FoolDev/Thanatos-27B:Q4_K_M" },
      "base-url": { type: "string", default: DEFAULT_BASE_URL },
      help: { type: "boolean", short: "h", default: false },
    },
    allowPositionals: false,
  })

  if (values.help) {
    console.log("usage: teleia [--model MODEL] [--base-url URL]")
    console.log(`  --model     default: ${values.model}`)
    console.log(`  --base-url  default: ${values["base-url"]}`)
    process.exit(0)
  }

  const llm = new LlmClient(values["base-url"] as string, values.model as string)
  const store = new Store()
  const agent = new Agent(llm, store)
  run(agent)
}

main()
