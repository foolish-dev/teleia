-- Ollama OpenAI-compat client. Streaming via `curl -N` piped through io.popen.
local json = require("teleia.json")

local M = {}

M.DEFAULT_BASE_URL = "http://127.0.0.1:11434/v1"

local function shell_escape(s)
  return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

function M.new(base_url, model)
  return {
    base_url = (base_url:gsub("/+$", "")),
    model = model,
  }
end

-- Iterator returning stream events:
--   { kind = "content", text = "..." }
--   { kind = "done", tool_calls = {...} }
function M.stream(client, messages, tools)
  local body = {
    model = client.model,
    messages = messages,
    stream = true,
  }
  if tools and #tools > 0 then
    body.tools = tools
  end
  local payload = json.encode(body)

  local tmp_in = os.tmpname()
  local fin = assert(io.open(tmp_in, "w"))
  fin:write(payload)
  fin:close()

  local cmd = string.format(
    "curl -fsS -N -X POST -H 'content-type: application/json' --data-binary @%s %s 2>/dev/null",
    shell_escape(tmp_in),
    shell_escape(client.base_url .. "/chat/completions")
  )
  local pipe = assert(io.popen(cmd, "r"))

  local acc = {}
  local indices = {}
  local done = false
  local queue = {}

  local function flush_done()
    table.sort(indices)
    local tool_calls = {}
    for _, idx in ipairs(indices) do
      local slot = acc[idx]
      table.insert(tool_calls, {
        id = slot.id or "",
        ["type"] = "function",
        ["function"] = { name = slot.name or "", arguments = slot.arguments or "" },
      })
    end
    return { kind = "done", tool_calls = tool_calls }
  end

  local function ingest_line(line)
    if not line:match("^data:") then return end
    local raw = line:sub(6):gsub("^%s+", "")
    if raw == "" or raw == "[DONE]" then return end
    local ok, chunk = pcall(json.decode, raw)
    if not ok or not chunk then return end
    for _, choice in ipairs(chunk.choices or {}) do
      local delta = choice.delta or {}
      if delta.content and delta.content ~= "" then
        table.insert(queue, { kind = "content", text = delta.content })
      end
      for _, tcd in ipairs(delta.tool_calls or {}) do
        local idx = tcd.index or 0
        if not acc[idx] then
          acc[idx] = { id = "", name = "", arguments = "" }
          table.insert(indices, idx)
        end
        local slot = acc[idx]
        if tcd.id and tcd.id ~= "" then slot.id = tcd.id end
        local fn = tcd["function"] or {}
        if fn.name and fn.name ~= "" then slot.name = fn.name end
        if fn.arguments then slot.arguments = slot.arguments .. fn.arguments end
      end
    end
  end

  return function()
    while #queue == 0 and not done do
      local line = pipe:read("*l")
      if line == nil then
        done = true
        local close_ok, what, code = pipe:close()
        os.remove(tmp_in)
        if not close_ok then
          table.insert(queue, {
            kind = "content",
            text = string.format("[error: ollama request failed (curl %s %s)]",
              tostring(what or "?"), tostring(code or "?"))
          })
        end
        table.insert(queue, flush_done())
        break
      end
      ingest_line(line)
    end
    if #queue > 0 then
      return table.remove(queue, 1)
    end
    return nil
  end
end

return M
