-- redact-secrets: scrub common API-key shapes out of tool results
-- before the model sees them. Returning { replace = "..." } from a
-- post_tool_use hook substitutes the tool's result. Use this when a
-- tool (e.g. Bash dumping `env`) might surface secrets you'd rather
-- not feed back into the prompt.
--
-- Patterns here are intentionally conservative; tune for your repo.

local plugin = { name = "redact-secrets", version = "0.1.0" }

local PATTERNS = {
  "sk%-[%w_%-]+",            -- OpenAI / Anthropic style
  "ghp_[%w]+",               -- GitHub personal access token
  "gho_[%w]+",               -- GitHub OAuth token
  "xoxb%-[%w%-]+",           -- Slack bot token
  "AKIA[%u%d]+",             -- AWS access key id
  "AIza[%w%-_]+",            -- Google API key
}

local function redact(s)
  if type(s) ~= "string" or s == "" then return s, 0 end
  local total = 0
  for _, pat in ipairs(PATTERNS) do
    local replaced, n = string.gsub(s, pat, "[REDACTED]")
    s = replaced
    total = total + n
  end
  return s, total
end

plugin.hooks = {
  post_tool_use = function(event, ctx)
    local cleaned, n = redact(event.result)
    if n == 0 then return end

    ctx:log("info", string.format("redacted %d secret(s) from %s output", n, event.tool))
    return { replace = cleaned }
  end,
}

return plugin
