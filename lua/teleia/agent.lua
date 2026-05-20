local llm_mod = require("teleia.llm")
local tools_mod = require("teleia.tools")
local store_mod = require("teleia.store")

local SYSTEM_PROMPT = "You are Teleia, a terse coding assistant running in a terminal. " ..
  "Use the provided tools (read, write, edit, bash) to do real work. " ..
  "Default to brief replies. When you finish a turn, stop -- do not narrate."

local MAX_TOOL_HOPS = 16

local M = {}

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
  M.push(agent, { role = "system", content = SYSTEM_PROMPT })
  return agent
end

function M.push(agent, message)
  store_mod.append(agent.store, agent.session_id, agent.seq, message)
  agent.seq = agent.seq + 1
  table.insert(agent.messages, message)
end

function M.turn(agent, user_input)
  M.push(agent, { role = "user", content = user_input })
  local steps = {}

  for _ = 1, MAX_TOOL_HOPS do
    local reply = llm_mod.chat(agent.llm, agent.messages, agent.tools)
    M.push(agent, reply)

    if reply.content and reply.content ~= "" then
      table.insert(steps, { kind = "assistant", text = reply.content })
    end

    if not reply.tool_calls or #reply.tool_calls == 0 then
      return steps
    end

    for _, call in ipairs(reply.tool_calls) do
      local ok, out = pcall(tools_mod.dispatch, call["function"].name, call["function"].arguments)
      if not ok then out = "error: " .. tostring(out) end
      table.insert(steps, {
        kind = "tool",
        name = call["function"].name,
        arguments = call["function"].arguments,
        output = out,
      })
      M.push(agent, { role = "tool", tool_call_id = call.id, content = out })
    end
  end

  table.insert(steps, { kind = "assistant", text = "[stopped: hit tool-hop limit of " .. MAX_TOOL_HOPS .. "]" })
  return steps
end

return M
