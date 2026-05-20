#!/usr/bin/env lua
-- Entry point: lua teleia.lua [--model M] [--base-url URL]

package.path = package.path .. ";" .. (arg[0]:match("(.*/)") or "./") .. "?.lua"
package.path = package.path .. ";" .. (arg[0]:match("(.*/)") or "./") .. "?/init.lua"

local llm_mod = require("teleia.llm")
local store_mod = require("teleia.store")
local agent_mod = require("teleia.agent")
local tui = require("teleia.tui")

local opts = {
  model = "hf.co/FoolDev/Thanatos-27B:Q4_K_M",
  base_url = llm_mod.DEFAULT_BASE_URL,
}

local i = 1
while i <= #arg do
  local a = arg[i]
  if a == "--model" then
    i = i + 1
    opts.model = arg[i] or opts.model
  elseif a == "--base-url" then
    i = i + 1
    opts.base_url = arg[i] or opts.base_url
  elseif a == "-h" or a == "--help" then
    print("usage: teleia.lua [--model MODEL] [--base-url URL]")
    print("  --model     default: " .. opts.model)
    print("  --base-url  default: " .. opts.base_url)
    os.exit(0)
  end
  i = i + 1
end

local llm_client = llm_mod.new(opts.base_url, opts.model)
local store = store_mod.open()
local agent = agent_mod.new(llm_client, store)
tui.run(agent)
