-- safety-net: refuse obviously destructive Bash commands before they
-- reach the shell. Returning { skip = "..." } from a pre_tool_use hook
-- short-circuits dispatch; the model sees the skip reason as the tool
-- result and can recover by trying a different approach.
--
-- This is illustrative, not exhaustive — real safety belongs in the
-- policy engine (auto_allow / bash_allowlist in config.toml). Use the
-- pattern here for project-specific guards that policy doesn't cover
-- ("don't touch /etc in this repo", "block git push --force", etc.).

local plugin = { name = "safety-net", version = "0.1.0" }

-- Lua patterns (not regex). Escape literal `.` with `%.`, `$` with `%$`,
-- and inside `[...]` escape `-` with `%-`. See https://www.lua.org/pil/20.2.html
local DENY = {
  { pattern = "rm%s+%-[rRf]+%s+/",      reason = "refusing recursive delete from filesystem root" },
  { pattern = "rm%s+%-[rRf]+%s+~",      reason = "refusing recursive delete of home directory" },
  { pattern = "rm%s+%-[rRf]+%s+%$HOME", reason = "refusing recursive delete of $HOME" },
  { pattern = "mkfs%.",                 reason = "refusing filesystem format" },
  { pattern = "dd%s+.-of=/dev/",        reason = "refusing raw write to a block device" },
  -- Classic fork bomb literal: :(){ :|:& };:
  { pattern = ":%(%)%{%s*:|:&%s*%};:",  reason = "refusing fork bomb" },
}

plugin.hooks = {
  pre_tool_use = function(event, ctx)
    -- Hooks fire on every tool. Bail fast for anything that isn't Bash.
    if event.tool ~= "Bash" then return end
    local cmd = event.args and event.args.command
    if type(cmd) ~= "string" then return end

    for _, rule in ipairs(DENY) do
      if string.find(cmd, rule.pattern) then
        -- ctx:log surfaces in /diagnostics. Useful for "did my hook fire?"
        ctx:log("warn", "blocked Bash command: " .. cmd)

        -- ctx:set_state persists across the session, scoped to this plugin.
        ctx:set_state("blocked", (ctx:get_state("blocked") or 0) + 1)

        -- Returning a `skip` table short-circuits dispatch entirely.
        -- The string is what the model sees as the "tool result".
        return { skip = "safety-net: " .. rule.reason }
      end
    end
    -- Implicit nil return = continue. Bash runs as normal.
  end,
}

plugin.slash_commands = {
  {
    name = "safety-net-stats",
    description = "Show how many commands safety-net has blocked this session.",
    execute = function(_args, ctx)
      local n = ctx:get_state("blocked") or 0
      return string.format("safety-net has blocked %d command(s) this session", n)
    end,
  },
}

return plugin
