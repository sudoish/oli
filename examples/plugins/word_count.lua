-- WordCount: a tool that reads a file via the host's built-in Read
-- tool and reports word/line/character counts.
--
-- Demonstrates:
--   * registering a plugin tool with a JSON-Schema parameter spec
--   * composing built-in tools via ctx:tool(name, args)
--   * returning a Lua table so the harness JSON-encodes the result
--     for the model

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

      local body = ctx:tool("Read", { file_path = path })

      local words = 0
      for _ in string.gmatch(body, "%S+") do
        words = words + 1
      end

      -- `wc -l` semantics: count newline characters. A trailing
      -- newline-less line isn't counted, matching the shell tool.
      local lines = 0
      for _ in string.gmatch(body, "\n") do
        lines = lines + 1
      end

      return {
        file = path,
        words = words,
        lines = lines,
        characters = #body,
      }
    end,
  },
}

return plugin
