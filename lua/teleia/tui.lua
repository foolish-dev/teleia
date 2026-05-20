-- Line-based UI. Lua's TUI ecosystem is sparse; we use ANSI colors + readline-style stdio
-- instead of pulling notcurses or a luarocks build. Acknowledged tradeoff vs the other impls.

local agent_mod = require("teleia.agent")

local M = {}

local C = {
  reset = "\27[0m",
  bold = "\27[1m",
  dim = "\27[2m",
  user = "\27[1;36m",   -- bold cyan
  assistant = "\27[1;35m", -- bold magenta
  tool = "\27[33m",     -- yellow
  err = "\27[31m",      -- red
  status = "\27[90m",   -- bright black / grey
  prompt = "\27[36m",   -- cyan
}

local function print_step(step)
  if step.kind == "assistant" then
    io.write(C.assistant .. "teleia" .. C.reset .. "\n")
    io.write(step.text .. "\n\n")
  elseif step.kind == "tool" then
    io.write(C.tool .. ("⚙ %s(%s)"):format(step.name, step.arguments) .. C.reset .. "\n")
    local n = 0
    for line in (step.output or ""):gmatch("([^\n]*)\n?") do
      if n >= 20 then break end
      io.write(C.dim .. "  " .. line .. C.reset .. "\n")
      n = n + 1
    end
    io.write("\n")
  end
end

function M.run(agent)
  io.write(C.status ..
    ("session %s ready · enter to send · ctrl-d / ctrl-c to quit"):format(agent.session_id:sub(1, 12)) ..
    C.reset .. "\n\n")
  io.stdout:setvbuf("no")

  while true do
    io.write(C.prompt .. "> " .. C.reset)
    local line = io.read("*l")
    if not line then break end
    if line ~= "" and line:match("%S") then
      io.write(C.user .. "you" .. C.reset .. "\n" .. line .. "\n\n")
      io.write(C.status .. "thinking…" .. C.reset .. "\n")
      local ok, steps_or_err = pcall(agent_mod.turn, agent, line)
      if not ok then
        io.write(C.err .. ("error: %s"):format(steps_or_err) .. C.reset .. "\n\n")
      else
        for _, s in ipairs(steps_or_err) do
          print_step(s)
        end
      end
    end
  end
end

return M
