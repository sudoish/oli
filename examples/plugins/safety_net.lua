-- safety-net: refuse obviously destructive Bash commands before they
-- reach the shell. Returning { skip = "..." } from a pre_tool_use hook
-- short-circuits dispatch; the model sees the skip reason as the tool
-- result and can recover.
--
-- This is illustrative, not exhaustive — real safety belongs in the
-- policy engine. Use this as a template for project-specific guards
-- (e.g. "don't touch /etc in this repo", "block git push --force").

local plugin = { name = "safety-net", version = "0.1.0" }

-- Patterns are Lua patterns (not regex). Escape literal dots with %.
-- and literal $ with %$.
local DENY = {
  { pattern = "rm%s+%-[rRf]+%s+/",      reason = "refusing recursive delete from filesystem root" },
  { pattern = "rm%s+%-[rRf]+%s+~",      reason = "refusing recursive delete of home directory" },
  { pattern = "rm%s+%-[rRf]+%s+%$HOME", reason = "refusing recursive delete of $HOME" },
  { pattern = "mkfs%.",                 reason = "refusing filesystem format" },
  { pattern = "dd%s+.-of=/dev/",        reason = "refusing raw write to a block device" },
  { pattern = ":%(%)%{%s*:|:&%s*%};:",  reason = "refusing fork bomb" },
}

plugin.hooks = {
  pre_tool_use = function(event, ctx)
    if event.tool ~= "Bash" then return end
    local cmd = event.args and event.args.command
    if type(cmd) ~= "string" then return end

    for _, rule in ipairs(DENY) do
      if string.find(cmd, rule.pattern) then
        ctx:log("warn", "blocked Bash command: " .. cmd)
        return { skip = "safety-net: " .. rule.reason }
      end
    end
  end,
}

return plugin
