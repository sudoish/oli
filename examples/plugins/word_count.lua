-- WordCount: a tool that reads a file via the host's built-in Read
-- tool and reports word/line/character counts (wc -w / -l / -c).
--
-- Demonstrates:
--   * registering a plugin tool with a JSON-Schema parameter spec
--   * composing built-in tools via ctx:tool(name, args)
--   * returning a Lua table — the harness JSON-encodes it for the model

local plugin = { name = "word-count", version = "0.1.0" }

plugin.tools = {
  {
    name = "WordCount",
    description = "Count words, lines, and characters in a text file.",
    parameters = {
      type = "object",
      properties = {
        file_path = { type = "string", description = "Path to the file to count." },
      },
      required = { "file_path" },
    },
    execute = function(args, ctx)
      local path = args.file_path
      if type(path) ~= "string" or path == "" then
        return { error = "file_path is required" }
      end

      -- ctx:tool returns the host tool's result as a string. Read returns
      -- the file body on success; on failure it returns a string starting
      -- with "Error reading ..." rather than throwing — we have to detect
      -- that ourselves or we'll happily count words in the error message.
      local body = ctx:tool("Read", { file_path = path })
      if body:sub(1, 14) == "Error reading " then
        return { error = body }
      end

      local words = 0
      for _ in string.gmatch(body, "%S+") do  -- maximal runs of non-whitespace
        words = words + 1
      end

      local lines = 0
      for _ in string.gmatch(body, "\n") do   -- wc -l counts newlines
        lines = lines + 1
      end

      return {
        file = path,
        words = words,
        lines = lines,
        characters = #body,  -- # on a Lua string returns byte length
      }
    end,
  },
}

return plugin
