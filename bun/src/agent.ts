import { LlmClient, type Message, type ToolDef } from "./llm"
import { Store } from "./store"
import { definitions, dispatch } from "./tools"

const SYSTEM_PROMPT =
  "You are Teleia, a terse coding assistant running in a terminal. " +
  "Use the provided tools (read, write, edit, bash) to do real work. " +
  "Default to brief replies. When you finish a turn, stop — do not narrate."

const MAX_TOOL_HOPS = 16

export type Step =
  | { kind: "assistant"; text: string }
  | { kind: "tool"; name: string; arguments: string; output: string }

export class Agent {
  llm: LlmClient
  store: Store
  tools: ToolDef[]
  sessionId: string
  messages: Message[] = []
  seq = 0

  constructor(llm: LlmClient, store: Store) {
    this.llm = llm
    this.store = store
    this.tools = definitions()
    this.sessionId = store.createSession(llm.model)
    this.push({ role: "system", content: SYSTEM_PROMPT })
  }

  private push(m: Message): void {
    this.store.append(this.sessionId, this.seq, m)
    this.seq++
    this.messages.push(m)
  }

  async turn(userInput: string): Promise<Step[]> {
    this.push({ role: "user", content: userInput })
    const steps: Step[] = []

    for (let hop = 0; hop < MAX_TOOL_HOPS; hop++) {
      const reply = await this.llm.chat(this.messages, this.tools)
      this.push(reply)

      if (reply.role !== "assistant") continue

      if (reply.content) {
        steps.push({ kind: "assistant", text: reply.content })
      }

      const calls = reply.tool_calls ?? []
      if (calls.length === 0) return steps

      for (const call of calls) {
        let output: string
        try {
          output = await dispatch(call.function.name, call.function.arguments)
        } catch (e) {
          output = `error: ${(e as Error).message}`
        }
        steps.push({
          kind: "tool",
          name: call.function.name,
          arguments: call.function.arguments,
          output,
        })
        this.push({ role: "tool", tool_call_id: call.id, content: output })
      }
    }

    steps.push({
      kind: "assistant",
      text: `[stopped: hit tool-hop limit of ${MAX_TOOL_HOPS}]`,
    })
    return steps
  }
}
