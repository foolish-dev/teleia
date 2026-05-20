-- Minimal pure-Lua JSON encode/decode. Embedded so no luarocks deps are needed.
-- Adapted from rxi/json.lua (MIT). Trimmed; supports nested tables, strings,
-- numbers, true/false/null. Encodes arrays as JSON arrays when keys are 1..N.

local json = {}

-- ------------------------------------------------------------------ encode --

local encode

local escape_char_map = {
  ["\\"] = "\\\\", ["\""] = "\\\"", ["\b"] = "\\b",
  ["\f"] = "\\f", ["\n"] = "\\n", ["\r"] = "\\r", ["\t"] = "\\t",
}

local function escape_char(c)
  return escape_char_map[c] or string.format("\\u%04x", c:byte())
end

local function encode_nil() return "null" end

local function encode_string(val)
  return '"' .. val:gsub('[%z\1-\31\\"]', escape_char) .. '"'
end

local function encode_number(val)
  if val ~= val or val <= -math.huge or val >= math.huge then
    error("unrepresentable number: " .. tostring(val))
  end
  if math.type and math.type(val) == "integer" then
    return string.format("%d", val)
  end
  return string.format("%.14g", val)
end

local function is_array(val)
  local n = 0
  for k in pairs(val) do
    if type(k) ~= "number" or k <= 0 or k % 1 ~= 0 then
      return false
    end
    n = n + 1
  end
  for i = 1, n do
    if val[i] == nil then return false end
  end
  return true, n
end

local function encode_table(val, seen)
  if seen[val] then error("circular reference") end
  seen[val] = true

  local array, n = is_array(val)
  if array then
    if n == 0 then seen[val] = nil; return "[]" end
    local parts = {}
    for i = 1, n do parts[i] = encode(val[i], seen) end
    seen[val] = nil
    return "[" .. table.concat(parts, ",") .. "]"
  end

  local parts = {}
  for k, v in pairs(val) do
    if type(k) ~= "string" then error("non-string table key: " .. type(k)) end
    parts[#parts + 1] = encode_string(k) .. ":" .. encode(v, seen)
  end
  seen[val] = nil
  return "{" .. table.concat(parts, ",") .. "}"
end

encode = function(val, seen)
  seen = seen or {}
  local t = type(val)
  if val == nil then return encode_nil() end
  if t == "string" then return encode_string(val) end
  if t == "number" then return encode_number(val) end
  if t == "boolean" then return val and "true" or "false" end
  if t == "table" then return encode_table(val, seen) end
  error("cannot encode type: " .. t)
end

function json.encode(val) return encode(val) end

-- ------------------------------------------------------------------ decode --

local parse

local function skip_ws(s, i)
  return s:find("[^ \t\r\n]", i) or (#s + 1)
end

local function decode_error(s, i, msg)
  local line = 1
  for _ in s:sub(1, i):gmatch("\n") do line = line + 1 end
  error(string.format("json: %s at line %d", msg, line))
end

local literals = { ["true"] = true, ["false"] = false, ["null"] = nil }
local literal_map = { ["true"] = true, ["false"] = false }

local function parse_literal(s, i)
  for lit, val in pairs({ ["true"] = true, ["false"] = false, ["null"] = "__null__" }) do
    if s:sub(i, i + #lit - 1) == lit then
      if val == "__null__" then return nil, i + #lit end
      return val, i + #lit
    end
  end
  decode_error(s, i, "invalid literal")
end

local function parse_number(s, i)
  local x = s:find("[^0-9eE.+-]", i) or (#s + 1)
  local sub = s:sub(i, x - 1)
  local n = tonumber(sub)
  if not n then decode_error(s, i, "invalid number") end
  return n, x
end

local escape_map = { ['"']='"', ['\\']='\\', ['/']='/', ['b']='\b',
                     ['f']='\f', ['n']='\n', ['r']='\r', ['t']='\t' }

local function parse_string(s, i)
  i = i + 1 -- skip opening quote
  local buf = {}
  while i <= #s do
    local c = s:sub(i, i)
    if c == '"' then return table.concat(buf), i + 1 end
    if c == '\\' then
      local nxt = s:sub(i + 1, i + 1)
      if escape_map[nxt] then
        buf[#buf + 1] = escape_map[nxt]
        i = i + 2
      elseif nxt == 'u' then
        local hex = s:sub(i + 2, i + 5)
        local cp = tonumber(hex, 16) or decode_error(s, i, "bad unicode")
        if cp < 128 then
          buf[#buf + 1] = string.char(cp)
        elseif cp < 2048 then
          buf[#buf + 1] = string.char(0xC0 + math.floor(cp / 64), 0x80 + (cp % 64))
        else
          buf[#buf + 1] = string.char(
            0xE0 + math.floor(cp / 4096),
            0x80 + (math.floor(cp / 64) % 64),
            0x80 + (cp % 64)
          )
        end
        i = i + 6
      else
        decode_error(s, i, "bad escape")
      end
    else
      buf[#buf + 1] = c
      i = i + 1
    end
  end
  decode_error(s, i, "unterminated string")
end

local function parse_array(s, i)
  i = i + 1
  local arr = {}
  i = skip_ws(s, i)
  if s:sub(i, i) == ']' then return arr, i + 1 end
  while true do
    local v
    v, i = parse(s, i)
    arr[#arr + 1] = v
    i = skip_ws(s, i)
    local c = s:sub(i, i)
    if c == ',' then i = skip_ws(s, i + 1)
    elseif c == ']' then return arr, i + 1
    else decode_error(s, i, "expected ',' or ']'") end
  end
end

local function parse_object(s, i)
  i = i + 1
  local obj = {}
  i = skip_ws(s, i)
  if s:sub(i, i) == '}' then return obj, i + 1 end
  while true do
    if s:sub(i, i) ~= '"' then decode_error(s, i, "expected string key") end
    local k; k, i = parse_string(s, i)
    i = skip_ws(s, i)
    if s:sub(i, i) ~= ':' then decode_error(s, i, "expected ':'") end
    i = skip_ws(s, i + 1)
    local v; v, i = parse(s, i)
    obj[k] = v
    i = skip_ws(s, i)
    local c = s:sub(i, i)
    if c == ',' then i = skip_ws(s, i + 1)
    elseif c == '}' then return obj, i + 1
    else decode_error(s, i, "expected ',' or '}'") end
  end
end

parse = function(s, i)
  i = skip_ws(s, i)
  local c = s:sub(i, i)
  if c == '{' then return parse_object(s, i) end
  if c == '[' then return parse_array(s, i) end
  if c == '"' then return parse_string(s, i) end
  if c == 't' or c == 'f' or c == 'n' then return parse_literal(s, i) end
  if c == '-' or c:match("%d") then return parse_number(s, i) end
  decode_error(s, i, "unexpected character: " .. c)
end

function json.decode(s)
  local val = parse(s, 1)
  return val
end

return json
