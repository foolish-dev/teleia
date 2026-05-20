-- Ollama OpenAI-compat client via shell-out to curl.
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

function M.chat(client, messages, tools)
  local body = {
    model = client.model,
    messages = messages,
    stream = false,
  }
  if tools and #tools > 0 then
    body.tools = tools
  end
  local payload = json.encode(body)

  local cmd = string.format(
    "curl -fsS -X POST -H 'content-type: application/json' --data-binary @- %s",
    shell_escape(client.base_url .. "/chat/completions")
  )
  -- write payload via stdin to avoid argv length and quoting hazards.
  local pipe = io.popen(cmd, "w")
  if not pipe then error("failed to spawn curl") end
  -- io.popen in mode "w" loses stdout; instead use a temp-file pattern:
  pipe:close()

  local tmp_in = os.tmpname()
  local tmp_out = os.tmpname()
  local fin = assert(io.open(tmp_in, "w"))
  fin:write(payload)
  fin:close()

  local rc = os.execute(string.format(
    "curl -fsS -X POST -H 'content-type: application/json' --data-binary @%s %s > %s 2>&1",
    shell_escape(tmp_in), shell_escape(client.base_url .. "/chat/completions"), shell_escape(tmp_out)
  ))
  local fout = assert(io.open(tmp_out, "r"))
  local raw = fout:read("*a")
  fout:close()
  os.remove(tmp_in)
  os.remove(tmp_out)

  if rc ~= true and rc ~= 0 then
    error("ollama call failed: " .. raw)
  end

  local ok, parsed = pcall(json.decode, raw)
  if not ok then error("decode response: " .. raw) end

  local choice = parsed.choices and parsed.choices[1] or {}
  local msg = choice.message or {}
  local tool_calls = {}
  for _, raw_tc in ipairs(msg.tool_calls or {}) do
    table.insert(tool_calls, {
      id = raw_tc.id or "",
      ["type"] = raw_tc["type"] or "function",
      ["function"] = {
        name = (raw_tc["function"] or {}).name or "",
        arguments = (raw_tc["function"] or {}).arguments or "{}",
      },
    })
  end
  local content = msg.content
  if content == "" then content = nil end

  return {
    role = "assistant",
    content = content,
    tool_calls = tool_calls,
  }
end

return M
