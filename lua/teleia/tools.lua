-- Tool registry: read / write / edit / bash
local json = require("teleia.json")

local M = {}

function M.definitions()
  return {
    {
      ["type"] = "function",
      ["function"] = {
        name = "read",
        description = "Read a file from disk. Returns the file contents as text.",
        parameters = {
          ["type"] = "object",
          properties = { path = { ["type"] = "string" } },
          required = { "path" },
        },
      },
    },
    {
      ["type"] = "function",
      ["function"] = {
        name = "write",
        description = "Write contents to a file, creating or overwriting it.",
        parameters = {
          ["type"] = "object",
          properties = {
            path = { ["type"] = "string" },
            content = { ["type"] = "string" },
          },
          required = { "path", "content" },
        },
      },
    },
    {
      ["type"] = "function",
      ["function"] = {
        name = "edit",
        description = "Replace a unique substring in a file. Fails if old_string is missing or non-unique.",
        parameters = {
          ["type"] = "object",
          properties = {
            path = { ["type"] = "string" },
            old_string = { ["type"] = "string" },
            new_string = { ["type"] = "string" },
          },
          required = { "path", "old_string", "new_string" },
        },
      },
    },
    {
      ["type"] = "function",
      ["function"] = {
        name = "bash",
        description = "Run a shell command and return its combined stdout/stderr. 30s timeout.",
        parameters = {
          ["type"] = "object",
          properties = { command = { ["type"] = "string" } },
          required = { "command" },
        },
      },
    },
  }
end

local function read_file(path)
  local f, err = io.open(path, "rb")
  if not f then error(err or ("cannot open " .. path)) end
  local data = f:read("*a")
  f:close()
  return data
end

local function write_file(path, content)
  local dir = path:match("^(.*)/[^/]+$")
  if dir and dir ~= "" then
    os.execute("mkdir -p " .. ("'" .. dir:gsub("'", "'\\''") .. "'"))
  end
  local f, err = io.open(path, "wb")
  if not f then error(err or ("cannot write " .. path)) end
  f:write(content)
  f:close()
end

local function count_occurrences(s, needle)
  local i, n = 1, 0
  while true do
    local found = s:find(needle, i, true)
    if not found then return n end
    n = n + 1
    i = found + #needle
  end
end

local function replace_one(s, old, new)
  local found = s:find(old, 1, true)
  if not found then return s end
  return s:sub(1, found - 1) .. new .. s:sub(found + #old)
end

function M.dispatch(name, arguments)
  local args = json.decode(arguments or "{}") or {}

  if name == "read" then
    return read_file(args.path)
  elseif name == "write" then
    write_file(args.path, args.content)
    return string.format("wrote %d bytes to %s", #args.content, args.path)
  elseif name == "edit" then
    local text = read_file(args.path)
    local n = count_occurrences(text, args.old_string)
    if n == 0 then error("old_string not found in " .. args.path) end
    if n > 1 then error(string.format("old_string matches %d times in %s; needs to be unique", n, args.path)) end
    write_file(args.path, replace_one(text, args.old_string, args.new_string))
    return "edited " .. args.path
  elseif name == "bash" then
    local tmp = os.tmpname()
    local rc = os.execute(string.format(
      "timeout 30 bash -lc %s > %s 2>&1",
      "'" .. args.command:gsub("'", "'\\''") .. "'",
      tmp
    ))
    local f = assert(io.open(tmp, "r"))
    local out = f:read("*a")
    f:close()
    os.remove(tmp)
    if rc == 124 or rc == true and false then
      out = out .. "\n[bash timed out after 30s]"
    end
    return out
  end
  error("unknown tool: " .. tostring(name))
end

return M
