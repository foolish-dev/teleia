import { LlmClient, type Message, type ToolDef } from "./llm"
import { Store } from "./store"
import { definitions, dispatch } from "./tools"

const SYSTEM_PROMPT =
  "You are Teleia, a terse coding assistant running in a terminal. " +
  "Use the provided tools (read, write, edit, bash) to do real work. " +
  "Default to brief replies. When you finish a turn, stop — do not narrate."

const MAX_TOOL_HOPS = 16

export type TurnEvent =
  | { kind: "assistant_start" }
  | { kind: "assistant_delta"; text: string }
  | { kind: "assistant_end" }
  | { kind: "tool_start"; name: string; arguments: string }
  | { kind: "tool_end"; name: string; output: string }
  | { kind: "turn_end" }

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

  reset(): void {
    this.sessionId = this.store.createSession(this.llm.model)
    this.messages = []
    this.seq = 0
    this.push({ role: "system", content: SYSTEM_PROMPT })
  }

  saveAlias(name: string): void {
    this.store.saveAlias(name, this.sessionId)
  }

  loadAlias(name: string): string {
    const id = this.store.resolveAlias(name)
    this.sessionId = id
    this.messages = this.store.load(id)
    this.seq = this.messages.length
    return id
  }

  async *turn(userInput: string): AsyncGenerator<TurnEvent> {
    this.push({ role: "user", content: userInput })

    for (let hop = 0; hop < MAX_TOOL_HOPS; hop++) {
      yield { kind: "assistant_start" }
      let contentBuf = ""
      let toolCalls: import("./llm").ToolCall[] = []

      for await (const ev of this.llm.stream(this.messages, this.tools)) {
        if (ev.kind === "content") {
          contentBuf += ev.text
          yield { kind: "assistant_delta", text: ev.text }
        } else {
          toolCalls = ev.tool_calls
        }
      }

      yield { kind: "assistant_end" }
      this.push({
        role: "assistant",
        content: contentBuf || null,
        tool_calls: toolCalls,
      })

      if (toolCalls.length === 0) {
        yield { kind: "turn_end" }
        return
      }

      for (const call of toolCalls) {
        yield { kind: "tool_start", name: call.function.name, arguments: call.function.arguments }
        let output: string
        try {
          output = await dispatch(call.function.name, call.function.arguments)
        } catch (e) {
          output = `error: ${(e as Error).message}`
        }
        yield { kind: "tool_end", name: call.function.name, output }
        this.push({ role: "tool", tool_call_id: call.id, content: output })
      }
    }

    yield {
      kind: "assistant_delta",
      text: `[stopped: hit tool-hop limit of ${MAX_TOOL_HOPS}]`,
    }
    yield { kind: "turn_end" }
  }
}
