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

export class LlmClient {
  baseUrl: string
  model: string

  constructor(baseUrl: string, model: string) {
    this.baseUrl = baseUrl.replace(/\/+$/, "")
    this.model = model
  }

  async chat(messages: Message[], tools?: ToolDef[]): Promise<Message> {
    const body: Record<string, unknown> = {
      model: this.model,
      messages,
      stream: false,
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
    const parsed = (await resp.json()) as {
      choices?: { message?: { content?: string | null; tool_calls?: ToolCall[] } }[]
    }
    const msg = parsed.choices?.[0]?.message
    if (!msg) throw new Error("no choices in response")
    const tool_calls = (msg.tool_calls ?? []).map((tc) => ({
      id: tc.id ?? "",
      type: "function" as const,
      function: {
        name: tc.function?.name ?? "",
        arguments: tc.function?.arguments ?? "{}",
      },
    }))
    return {
      role: "assistant",
      content: msg.content || null,
      tool_calls,
    }
  }
}
