-- Tool definitions + dispatch via the shared rust binary `teleia-tools-bin`.
-- Lua doesn't have a way to pipe both stdin and stdout through io.popen, so
-- we use tempfiles for both — matches the impl's other shell-out patterns.
local json = require("teleia.json")

local M = {}

local BINARY = "teleia-tools-bin"

local function shell_escape(s)
  return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

local function read_file(path)
  local f = assert(io.open(path, "r"))
  local data = f:read("*a")
  f:close()
  return data
end

local function write_file(path, data)
  local f = assert(io.open(path, "w"))
  f:write(data)
  f:close()
end

function M.definitions()
  local tmp = os.tmpname()
  os.execute(BINARY .. " defs > " .. shell_escape(tmp))
  local data = read_file(tmp)
  os.remove(tmp)
  return json.decode(data)
end

-- Run a tool via teleia-tools-bin. The binary always exits 0 and encodes
-- tool failures as "error: ..." in stdout. Non-zero exit here means the
-- binary itself failed (e.g., not on PATH).
function M.dispatch(name, arguments)
  local tmp_in = os.tmpname()
  local tmp_out = os.tmpname()
  local tmp_err = os.tmpname()
  write_file(tmp_in, arguments or "")
  local cmd = string.format(
    "%s run %s < %s > %s 2> %s",
    BINARY,
    shell_escape(name),
    shell_escape(tmp_in),
    shell_escape(tmp_out),
    shell_escape(tmp_err)
  )
  local ok = os.execute(cmd)
  local out = read_file(tmp_out)
  local err = read_file(tmp_err)
  os.remove(tmp_in)
  os.remove(tmp_out)
  os.remove(tmp_err)
  if not ok then
    return "error: " .. (err:gsub("%s+$", ""))
  end
  return out
end

return M
