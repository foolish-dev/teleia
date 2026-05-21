local llm_mod = require("teleia.llm")
local tools_mod = require("teleia.tools")
local store_mod = require("teleia.store")

local SYSTEM_PROMPT = "You are Teleia, a terse coding assistant running in a terminal. " ..
  "Use the provided tools (read, write, edit, bash) to do real work. " ..
  "Default to brief replies. When you finish a turn, stop — do not narrate."

local MAX_TOOL_HOPS = 16

local M = {}

local function push(agent, message)
  store_mod.append(agent.store, agent.session_id, agent.seq, message)
  agent.seq = agent.seq + 1
  table.insert(agent.messages, message)
end

function M.new(llm_client, store)
  local session_id = store_mod.create_session(store, llm_client.model)
  local agent = {
    llm = llm_client,
    tools = tools_mod.definitions(),
    store = store,
    session_id = session_id,
    messages = {},
    seq = 0,
  }
  push(agent, { role = "system", content = SYSTEM_PROMPT })
  return agent
end

function M.reset(agent)
  agent.session_id = store_mod.create_session(agent.store, agent.llm.model)
  agent.messages = {}
  agent.seq = 0
  push(agent, { role = "system", content = SYSTEM_PROMPT })
end

function M.save_alias(agent, name)
  store_mod.save_alias(agent.store, name, agent.session_id)
end

function M.load_alias(agent, name)
  local session_id = store_mod.resolve_alias(agent.store, name)
  agent.session_id = session_id
  agent.messages = store_mod.load(agent.store, session_id)
  agent.seq = #agent.messages
  return session_id
end

-- Iterator yielding events:
--   { kind = "assistant_start" }
--   { kind = "assistant_delta", text = "..." }
--   { kind = "assistant_end" }
--   { kind = "tool_start", name = "...", arguments = "..." }
--   { kind = "tool_end", name = "...", output = "..." }
--   { kind = "turn_end" }
function M.turn(agent, user_input)
  push(agent, { role = "user", content = user_input })

  local hop = 0
  local stream_iter = nil
  local content_buf = ""
  local tool_calls = {}
  local turn_done = false
  local pending = {}

  table.insert(pending, { kind = "assistant_start" })

  local function start_stream()
    stream_iter = llm_mod.stream(agent.llm, agent.messages, agent.tools)
    content_buf = ""
    tool_calls = {}
  end

  start_stream()

  return function()
    if turn_done then return nil end
    if #pending > 0 then
      return table.remove(pending, 1)
    end

    -- Pull from current stream
    while stream_iter do
      local ev = stream_iter()
      if ev == nil then
        stream_iter = nil
        break
      end
      if ev.kind == "content" then
        content_buf = content_buf .. ev.text
        return { kind = "assistant_delta", text = ev.text }
      elseif ev.kind == "done" then
        tool_calls = ev.tool_calls or {}
        stream_iter = nil
        break
      end
    end

    -- Stream ended for this hop. Emit assistant_end + persist + decide next hop.
    table.insert(pending, { kind = "assistant_end" })
    local assistant = { role = "assistant" }
    if content_buf ~= "" then assistant.content = content_buf end
    if #tool_calls > 0 then assistant.tool_calls = tool_calls end
    push(agent, assistant)

    if #tool_calls == 0 then
      turn_done = true
      table.insert(pending, { kind = "turn_end" })
      return table.remove(pending, 1)
    end

    -- For each tool call: tool_start, dispatch, tool_end.
    for _, call in ipairs(tool_calls) do
      local fn = call["function"] or {}
      table.insert(pending, { kind = "tool_start", name = fn.name, arguments = fn.arguments })
      local ok, output = pcall(tools_mod.dispatch, fn.name, fn.arguments)
      if not ok then output = "error: " .. tostring(output) end
      push(agent, { role = "tool", tool_call_id = call.id, content = output })
      table.insert(pending, { kind = "tool_end", name = fn.name, output = output })
    end

    hop = hop + 1
    if hop >= MAX_TOOL_HOPS then
      turn_done = true
      table.insert(pending, { kind = "assistant_delta",
        text = "[stopped: hit tool-hop limit of " .. MAX_TOOL_HOPS .. "]" })
      table.insert(pending, { kind = "turn_end" })
    else
      table.insert(pending, { kind = "assistant_start" })
      start_stream()
    end
    return table.remove(pending, 1)
  end
end

return M
