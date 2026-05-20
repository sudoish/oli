-- redact-secrets: scrub common API-key shapes out of Bash output before
-- the model sees them. Returning { replace = "..." } from a post_tool_use
-- hook substitutes the tool's result. Use this when a tool (e.g. Bash
-- dumping `env`) might surface secrets you'd rather not feed back into
-- the prompt.
--
-- Scoped to Bash on purpose: if the model is reading source code with
-- legitimate API-key-shaped strings (test fixtures, sample configs),
-- redacting them would corrupt the data the model needs. Drop the
-- `event.tool == "Bash"` guard if you want broader coverage.

local plugin = { name = "redact-secrets", version = "0.1.0" }

local PATTERNS = {
  "sk%-[%w_%-]+",            -- OpenAI / Anthropic style: sk-...
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
    if event.tool ~= "Bash" then return end

    local cleaned, n = redact(event.result)
    if n == 0 then return end  -- nothing to do; let the result pass through

    ctx:log("info", string.format("redacted %d secret(s) from %s output", n, event.tool))
    return { replace = cleaned }
  end,
}

return plugin
