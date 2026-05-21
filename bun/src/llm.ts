export const DEFAULT_BASE_URL = "http://127.0.0.1:11434/v1"

export type ToolCall = {
  id: string
  type: "function"
  function: { name: string; arguments: string }
}

export type Message =
  | { role: "system"; content: string }
  | { role: "user"; content: string }
  | { role: "assistant"; content: string | null; tool_calls?: ToolCall[] }
  | { role: "tool"; tool_call_id: string; content: string }

export type ToolDef = {
  type: "function"
  function: {
    name: string
    description: string
    parameters: Record<string, unknown>
  }
}

export type StreamEvent =
  | { kind: "content"; text: string }
  | { kind: "done"; tool_calls: ToolCall[] }

type ChunkDelta = {
  content?: string
  tool_calls?: Array<{
    index: number
    id?: string
    type?: string
    function?: { name?: string; arguments?: string }
  }>
}

type StreamChunk = {
  choices?: Array<{ delta?: ChunkDelta }>
}

export class LlmClient {
  baseUrl: string
  model: string

  constructor(baseUrl: string, model: string) {
    this.baseUrl = baseUrl.replace(/\/+$/, "")
    this.model = model
  }

  async *stream(messages: Message[], tools?: ToolDef[]): AsyncGenerator<StreamEvent> {
    const body: Record<string, unknown> = {
      model: this.model,
      messages,
      stream: true,
    }
    if (tools && tools.length > 0) body.tools = tools

    const resp = await fetch(`${this.baseUrl}/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    })
    if (!resp.ok) {
      const text = await resp.text()
      throw new Error(`ollama ${resp.status}: ${text}`)
    }
    if (!resp.body) {
      throw new Error("ollama returned no body")
    }

    const reader = resp.body.getReader()
    const decoder = new TextDecoder()
    let buf = ""
    const acc = new Map<
      number,
      { id: string; type: string; name: string; arguments: string }
    >()

    try {
      while (true) {
        const { value, done } = await reader.read()
        if (done) break
        buf += decoder.decode(value, { stream: true })
        let nl = buf.indexOf("\n")
        while (nl !== -1) {
          const line = buf.slice(0, nl).trim()
          buf = buf.slice(nl + 1)
          if (line.startsWith("data:")) {
            const payload = line.slice(5).trim()
            if (payload && payload !== "[DONE]") {
              let chunk: StreamChunk
              try {
                chunk = JSON.parse(payload) as StreamChunk
              } catch {
                nl = buf.indexOf("\n")
                continue
              }
              for (const choice of chunk.choices ?? []) {
                const delta = choice.delta ?? {}
                if (delta.content) {
                  yield { kind: "content", text: delta.content }
                }
                for (const tcd of delta.tool_calls ?? []) {
                  accumulate(acc, tcd)
                }
              }
            }
          }
          nl = buf.indexOf("\n")
        }
      }
    } finally {
      reader.releaseLock()
    }

    const tool_calls: ToolCall[] = [...acc.entries()]
      .sort((a, b) => a[0] - b[0])
      .map(([, v]) => ({
        id: v.id,
        type: "function" as const,
        function: { name: v.name, arguments: v.arguments },
      }))
    yield { kind: "done", tool_calls }
  }
}

function accumulate(
  acc: Map<number, { id: string; type: string; name: string; arguments: string }>,
  delta: {
    index: number
    id?: string
    type?: string
    function?: { name?: string; arguments?: string }
  },
): void {
  let slot = acc.get(delta.index)
  if (!slot) {
    slot = { id: "", type: "function", name: "", arguments: "" }
    acc.set(delta.index, slot)
  }
  if (delta.id) slot.id = delta.id
  if (delta.type) slot.type = delta.type
  if (delta.function?.name) slot.name = delta.function.name
  if (delta.function?.arguments) slot.arguments += delta.function.arguments
}
