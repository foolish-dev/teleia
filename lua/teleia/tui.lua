-- Line-mode UI with incremental delta printing + slash commands.
-- Lua's TUI ecosystem is sparse: no scrollback (terminal handles it), no
-- proper ratatui-style render. Streaming tokens print live to stdout.

local agent_mod = require("teleia.agent")

local M = {}

-- Tokyo Night via 24-bit ANSI (CSI 38;2;R;G;Bm). Falls back to nothing on
-- pre-truecolor terminals (escape sequences are ignored or printed literally;
-- modern terminals on Linux/macOS support truecolor).
local function fg(r, g, b) return string.format("\27[38;2;%d;%d;%dm", r, g, b) end
local C = {
  reset = "\27[0m",
  user = "\27[1m" .. fg(125, 207, 255),  -- #7dcfff bold
  assistant = "\27[1m" .. fg(187, 154, 247), -- #bb9af7 bold
  tool = fg(224, 175, 104),               -- #e0af68
  err = fg(247, 118, 142),                -- #f7768e
  info = fg(122, 162, 247),               -- #7aa2f7
  status = fg(86, 95, 137),               -- #565f89
  dim = fg(86, 95, 137),                  -- #565f89
  prompt = fg(125, 207, 255),             -- #7dcfff
}

local function short_id(s)
  if #s <= 12 then return s end
  return s:sub(1, 12)
end

local function handle_slash(agent, raw)
  local name, arg = raw:match("^(%S+)%s*(.-)$")
  name = name or ""
  arg = (arg or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if name == "reset" then
    agent_mod.reset(agent)
    io.write(C.info .. "· started new session " .. short_id(agent.session_id) .. C.reset .. "\n\n")
  elseif name == "save" then
    if arg == "" then
      io.write(C.err .. "error: usage: /save NAME" .. C.reset .. "\n\n")
      return
    end
    local ok, err = pcall(agent_mod.save_alias, agent, arg)
    if not ok then
      io.write(C.err .. "error: save: " .. tostring(err) .. C.reset .. "\n\n")
    else
      io.write(C.info .. "· saved current session as '" .. arg .. "'" .. C.reset .. "\n\n")
    end
  elseif name == "load" then
    if arg == "" then
      io.write(C.err .. "error: usage: /load NAME" .. C.reset .. "\n\n")
      return
    end
    local ok, id_or_err = pcall(agent_mod.load_alias, agent, arg)
    if not ok then
      io.write(C.err .. "error: load: " .. tostring(id_or_err) .. C.reset .. "\n\n")
    else
      io.write(C.info .. "· loaded '" .. arg .. "' → session " .. short_id(id_or_err) .. C.reset .. "\n\n")
    end
  elseif name == "help" or name == "?" then
    io.write(C.info .. "· commands: /reset · /save NAME · /load NAME · /help" .. C.reset .. "\n\n")
  else
    io.write(C.err .. "error: unknown command: /" .. name .. C.reset .. "\n\n")
  end
end

function M.run(agent)
  io.write(C.status ..
    ("session %s · ready · enter to send · /help cmds · ctrl-d / ctrl-c to quit"):format(short_id(agent.session_id)) ..
    C.reset .. "\n\n")
  io.stdout:setvbuf("no")

  while true do
    io.write(C.prompt .. "> " .. C.reset)
    local line = io.read("*l")
    if not line then break end
    if line ~= "" and line:match("%S") then
      if line:sub(1, 1) == "/" then
        handle_slash(agent, line:sub(2))
      else
        io.write(C.user .. "you" .. C.reset .. "\n" .. line .. "\n\n")
        local iter = agent_mod.turn(agent, line)
        local in_assistant = false
        local in_tool = false
        for ev in iter do
          if ev.kind == "assistant_start" then
            io.write(C.assistant .. "teleia ▌" .. C.reset .. "\n")
            in_assistant = true
          elseif ev.kind == "assistant_delta" then
            io.write(ev.text)
          elseif ev.kind == "assistant_end" then
            if in_assistant then io.write("\n\n") end
            in_assistant = false
          elseif ev.kind == "tool_start" then
            io.write(C.tool .. ("⚙ … %s(%s)"):format(ev.name, ev.arguments) .. C.reset .. "\n")
            in_tool = true
          elseif ev.kind == "tool_end" then
            local count = 0
            for output_line in (ev.output or ""):gmatch("([^\n]*)\n?") do
              if count >= 20 then break end
              io.write(C.dim .. "  " .. output_line .. C.reset .. "\n")
              count = count + 1
            end
            io.write("\n")
            in_tool = false
          elseif ev.kind == "turn_end" then
            break
          end
        end
        if in_assistant then io.write("\n\n") end
      end
    end
  end
end

return M
