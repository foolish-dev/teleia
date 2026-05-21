-- Session/message persistence via shell-out to sqlite3.
local json = require("teleia.json")

local M = {}

local function shell_escape(s)
  return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

local function data_path()
  local base = os.getenv("XDG_DATA_HOME")
  if not base or base == "" then
    local home = os.getenv("HOME") or "."
    base = home .. "/.local/share"
  end
  return base .. "/teleia/teleia.sqlite"
end

local function run_sqlite_write(path, sql)
  local tmp_sql = os.tmpname()
  local f = assert(io.open(tmp_sql, "w"))
  f:write(sql)
  f:close()
  os.execute(string.format("sqlite3 %s < %s > /dev/null 2>&1", shell_escape(path), shell_escape(tmp_sql)))
  os.remove(tmp_sql)
end

local function run_sqlite_query(path, sql)
  local tmp_sql = os.tmpname()
  local tmp_out = os.tmpname()
  local f = assert(io.open(tmp_sql, "w"))
  f:write(sql)
  f:close()
  os.execute(string.format("sqlite3 -separator '|' %s < %s > %s 2>/dev/null",
    shell_escape(path), shell_escape(tmp_sql), shell_escape(tmp_out)))
  local fo = assert(io.open(tmp_out, "r"))
  local out = fo:read("*a")
  fo:close()
  os.remove(tmp_sql)
  os.remove(tmp_out)
  return out
end

function M.open()
  local path = data_path()
  local dir = path:match("^(.*)/[^/]+$")
  if dir then os.execute("mkdir -p " .. shell_escape(dir)) end
  run_sqlite_write(path, [[
    CREATE TABLE IF NOT EXISTS sessions (
      id TEXT PRIMARY KEY,
      model TEXT NOT NULL,
      created_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS messages (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      session_id TEXT NOT NULL,
      seq INTEGER NOT NULL,
      payload TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
    CREATE TABLE IF NOT EXISTS aliases (
      name TEXT PRIMARY KEY,
      session_id TEXT NOT NULL,
      created_at INTEGER NOT NULL
    );
  ]])
  return { path = path }
end

local function rand_id()
  math.randomseed(os.time() + math.floor(os.clock() * 1e6))
  local hex = "0123456789abcdef"
  local out = {}
  for i = 1, 32 do
    local idx = math.random(1, 16)
    out[i] = hex:sub(idx, idx)
  end
  return table.concat(out)
end

local function sql_quote(s)
  return "'" .. tostring(s):gsub("'", "''") .. "'"
end

function M.create_session(store, model)
  local id = rand_id()
  run_sqlite_write(store.path, string.format(
    "INSERT INTO sessions (id, model, created_at) VALUES (%s, %s, %d);",
    sql_quote(id), sql_quote(model), os.time()
  ))
  return id
end

function M.append(store, session_id, seq, message)
  local payload = json.encode(message)
  run_sqlite_write(store.path, string.format(
    "INSERT INTO messages (session_id, seq, payload) VALUES (%s, %d, %s);",
    sql_quote(session_id), seq, sql_quote(payload)
  ))
end

function M.load(store, session_id)
  local out = run_sqlite_query(store.path, string.format(
    "SELECT payload FROM messages WHERE session_id = %s ORDER BY seq ASC;",
    sql_quote(session_id)
  ))
  local msgs = {}
  for line in out:gmatch("([^\n]+)") do
    local ok, m = pcall(json.decode, line)
    if ok then table.insert(msgs, m) end
  end
  return msgs
end

function M.save_alias(store, name, session_id)
  run_sqlite_write(store.path, string.format(
    "INSERT OR REPLACE INTO aliases (name, session_id, created_at) VALUES (%s, %s, %d);",
    sql_quote(name), sql_quote(session_id), os.time()
  ))
end

function M.resolve_alias(store, name)
  local out = run_sqlite_query(store.path, string.format(
    "SELECT session_id FROM aliases WHERE name = %s;", sql_quote(name)
  ))
  out = out:gsub("%s+$", "")
  if out == "" then
    error("no session saved as '" .. name .. "'")
  end
  return out
end

return M
