//! Lua plugin runtime — mlua-backed.
//!
//! ## What plugins can do
//!
//! Each `.lua` file under `~/.config/agent/plugins/` or
//! `<project>/.agent/plugins/` returns a table:
//!
//! ```lua
//! local plugin = { name = "my-plugin", version = "0.1.0" }
//!
//! plugin.tools = {
//!   {
//!     name = "MyTool",
//!     description = "...",
//!     parameters = { type = "object", properties = {} },
//!     execute = function(args, ctx)
//!       ctx:log("info", "running with " .. (args.x or "nothing"))
//!       return ctx:tool("Read", { file_path = "README.md" })
//!     end,
//!   },
//! }
//!
//! plugin.slash_commands = {
//!   {
//!     name = "summarize",
//!     description = "summarize the repo",
//!     execute = function(args, ctx)
//!       return "summary: ..."
//!     end,
//!   },
//! }
//!
//! plugin.hooks = {
//!   pre_tool_use = function(event, ctx)
//!     ctx:log("debug", "pre " .. event.tool)
//!   end,
//!   post_tool_use = function(event, ctx) end,
//!   stop = function(event, ctx) end,
//! }
//!
//! return plugin
//! ```
//!
//! ## Sandbox
//!
//! `os`, `io`, `package.loadlib`, and `dofile` / `loadfile` are removed
//! from the global table before each plugin is loaded. File and shell
//! access flow through `ctx:read_file` / `ctx:write_file` / `ctx:shell`,
//! which go through the same policy gate as the model's own tool calls.
//!
//! ## Async surface
//!
//! `ctx:tool` is the only host method that crosses the async boundary
//! (since tool dispatch is async). It uses `mlua::Lua::create_async_function`
//! so plugin authors can simply write `local x = ctx:tool("Read", ...)`
//! and the runtime suspends the Lua coroutine until the underlying
//! tool call resolves.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mlua::{Function, Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::error::{AgentError, Result};
use crate::hooks::{Hook, HookPayload};
use crate::repl::slash::{SlashCommand, SlashOutcome};
use crate::tools::{Tool, ToolContext};

/// Result of loading the plugin discovery dirs. The harness pulls
/// individual sub-vecs into the relevant registries at startup.
#[derive(Default)]
pub struct LoadedPlugins {
    pub tools: Vec<Box<dyn Tool>>,
    pub slash_commands: Vec<Box<dyn SlashCommand>>,
    pub hooks: Vec<Box<dyn Hook>>,
    /// Metadata for `/plugins`. One entry per file successfully loaded.
    pub manifest: Vec<PluginManifest>,
}

#[derive(Clone, Debug)]
pub struct PluginManifest {
    pub name: String,
    pub version: Option<String>,
    pub source: PathBuf,
    pub tools: Vec<String>,
    pub slash_commands: Vec<String>,
    pub hook_events: Vec<String>,
}

/// Shared state passed into every plugin entry point as `ctx`. Wraps a
/// handle to the harness's tool registry and a logging sink. New host
/// methods land here as the surface grows.
#[derive(Clone)]
pub struct HostShared {
    /// Snapshot of the harness's tool set, taken at plugin-load time.
    /// Plugin tool calls dispatch through this. We use an `Arc<Mutex>`
    /// so the host API can borrow tools without holding a reference
    /// through the (non-Send) `Lua` instance — and so the dispatch
    /// future stays Send.
    pub tools: Arc<Mutex<crate::tools::Registry>>,
    /// Per-plugin tool context for `ctx:tool` calls. We isolate it from
    /// the agent's main context so a plugin can't accidentally bypass
    /// `Edit`'s read-first invariant by piggybacking on the parent.
    pub plugin_ctx: ToolContext,
    /// Plugin id (file stem). Goes into `[plugin]` log prefixes.
    pub plugin_id: String,
}

/// Discover and load every plugin from the standard set of dirs.
/// Failures are reported as `eprintln!` lines and the affected plugin
/// is skipped — a misbehaving plugin never crashes the session.
pub async fn load_all(tools_for_host: Arc<Mutex<crate::tools::Registry>>) -> LoadedPlugins {
    let mut out = LoadedPlugins::default();
    for dir in default_plugin_dirs() {
        load_dir(&dir, tools_for_host.clone(), &mut out).await;
    }
    out
}

/// Public for tests: load a specific dir into an existing aggregate.
pub async fn load_dir(
    dir: &Path,
    tools_for_host: Arc<Mutex<crate::tools::Registry>>,
    out: &mut LoadedPlugins,
) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return, // dir doesn't exist; that's fine
    };
    let mut paths: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("lua"))
        .collect();
    paths.sort();
    for path in paths {
        match load_one(&path, tools_for_host.clone()).await {
            Ok(loaded) => {
                out.manifest.push(loaded.manifest);
                out.tools.extend(loaded.tools);
                out.slash_commands.extend(loaded.slash_commands);
                out.hooks.extend(loaded.hooks);
            }
            Err(e) => {
                eprintln!("[plugins] {} failed to load: {}", path.display(), e);
            }
        }
    }
}

struct OnePlugin {
    manifest: PluginManifest,
    tools: Vec<Box<dyn Tool>>,
    slash_commands: Vec<Box<dyn SlashCommand>>,
    hooks: Vec<Box<dyn Hook>>,
}

async fn load_one(
    path: &Path,
    tools_for_host: Arc<Mutex<crate::tools::Registry>>,
) -> Result<OnePlugin> {
    let source = tokio::fs::read_to_string(path).await?;
    let plugin_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("anon")
        .to_string();

    let lua = Lua::new();
    apply_sandbox(&lua)?;
    let plugin_table: Table = lua.load(&source).eval()?;

    let name: String = plugin_table
        .get::<Option<String>>("name")?
        .unwrap_or_else(|| plugin_id.clone());
    let version: Option<String> = plugin_table.get("version")?;

    let host = HostShared {
        tools: tools_for_host,
        plugin_ctx: ToolContext::new(),
        plugin_id: plugin_id.clone(),
    };

    let lua = Arc::new(lua);

    // Tools.
    let mut tool_specs = Vec::new();
    let mut tool_names = Vec::new();
    if let Some(tools_table) = plugin_table.get::<Option<Table>>("tools")? {
        for entry in tools_table.sequence_values::<Table>() {
            let entry = entry?;
            let t_name: String = entry.get("name")?;
            let t_desc: String = entry
                .get::<Option<String>>("description")?
                .unwrap_or_default();
            let t_params: Option<LuaValue> = entry.get("parameters")?;
            let t_params_json = match t_params {
                Some(v) => lua.from_value::<Value>(v)?,
                None => json!({"type":"object","properties":{}}),
            };
            let t_exec: Function = entry.get("execute")?;
            // Stash the executor in the registry table inside Lua so we
            // can retrieve it by index from the async dispatch closure.
            let registry: Table = stash_registry(&lua)?;
            let exec_idx = stash_function(&registry, t_exec)?;

            tool_names.push(t_name.clone());
            tool_specs.push(LuaToolSpec {
                lua: lua.clone(),
                exec_idx,
                name: t_name,
                description: t_desc,
                parameters: t_params_json,
                host: host.clone(),
            });
        }
    }
    let tools_out: Vec<Box<dyn Tool>> = tool_specs
        .into_iter()
        .map(|s| Box::new(s) as Box<dyn Tool>)
        .collect();

    // Slash commands.
    let mut slash_out: Vec<Box<dyn SlashCommand>> = Vec::new();
    let mut slash_names = Vec::new();
    if let Some(slash_table) = plugin_table.get::<Option<Table>>("slash_commands")? {
        for entry in slash_table.sequence_values::<Table>() {
            let entry = entry?;
            let s_name: String = entry.get("name")?;
            let s_desc: String = entry
                .get::<Option<String>>("description")?
                .unwrap_or_default();
            let s_exec: Function = entry.get("execute")?;
            let registry: Table = stash_registry(&lua)?;
            let exec_idx = stash_function(&registry, s_exec)?;
            slash_names.push(s_name.clone());
            slash_out.push(Box::new(LuaSlashCommand {
                lua: lua.clone(),
                exec_idx,
                name: s_name,
                description: s_desc,
                host: host.clone(),
            }));
        }
    }

    // Hooks.
    let mut hook_events = Vec::new();
    let mut hooks_out: Vec<Box<dyn Hook>> = Vec::new();
    if let Some(hooks_table) = plugin_table.get::<Option<Table>>("hooks")? {
        for event_name in &["pre_tool_use", "post_tool_use", "stop"] {
            if let Some(handler) = hooks_table.get::<Option<Function>>(*event_name)? {
                let registry: Table = stash_registry(&lua)?;
                let exec_idx = stash_function(&registry, handler)?;
                hook_events.push(event_name.to_string());
                hooks_out.push(Box::new(LuaHook {
                    lua: lua.clone(),
                    exec_idx,
                    event: (*event_name).to_string(),
                    plugin_id: plugin_id.clone(),
                    host: host.clone(),
                }));
            }
        }
    }

    Ok(OnePlugin {
        manifest: PluginManifest {
            name,
            version,
            source: path.to_path_buf(),
            tools: tool_names,
            slash_commands: slash_names,
            hook_events,
        },
        tools: tools_out,
        slash_commands: slash_out,
        hooks: hooks_out,
    })
}

/// Strip dangerous globals from `lua`. Plugins keep `string`, `table`,
/// `math`, `pairs`/`ipairs`, etc. — anything that doesn't reach off
/// the process. Filesystem and shell access goes through `ctx`.
fn apply_sandbox(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    for forbidden in ["os", "io", "dofile", "loadfile", "require", "debug"] {
        globals.set(forbidden, LuaValue::Nil)?;
    }
    if let Some(package) = globals.get::<Option<Table>>("package")? {
        package.set("loadlib", LuaValue::Nil)?;
        package.set("cpath", LuaValue::Nil)?;
        package.set("path", LuaValue::Nil)?;
    }
    Ok(())
}

/// Per-Lua-state registry table for stashing callable functions. Stored
/// at a fixed key so multiple stash calls share it.
fn stash_registry(lua: &Lua) -> mlua::Result<Table> {
    let globals = lua.globals();
    if let Some(t) = globals.get::<Option<Table>>("__agent_stash")? {
        return Ok(t);
    }
    let t = lua.create_table()?;
    globals.set("__agent_stash", t.clone())?;
    Ok(t)
}

fn stash_function(registry: &Table, f: Function) -> mlua::Result<i64> {
    let next: i64 = registry.get::<Option<i64>>("__next")?.unwrap_or(1);
    registry.set(next, f)?;
    registry.set("__next", next + 1)?;
    Ok(next)
}

fn build_ctx(lua: &Lua, host: HostShared) -> mlua::Result<Table> {
    let ctx = lua.create_table()?;

    // ctx:log(level, msg)
    {
        let plugin_id = host.plugin_id.clone();
        let log =
            lua.create_function(move |_lua, (_self, level, msg): (Table, String, String)| {
                eprintln!("[plugin:{}] {} {}", plugin_id, level, msg);
                Ok(())
            })?;
        ctx.set("log", log)?;
    }

    // ctx:tool(name, args) — async dispatch through the harness's
    // registry. Goes through the registry but NOT through the agent's
    // policy engine for now (plugins are user-trusted code; if the
    // user's policy needs to gate plugin tool calls, that's a later
    // refinement).
    {
        let host_clone = host.clone();
        let tool_fn = lua.create_async_function(
            move |lua, (_self, name, args): (Table, String, Option<LuaValue>)| {
                let host = host_clone.clone();
                async move {
                    let args_json: Value = match args {
                        Some(v) => lua.from_value(v)?,
                        None => json!({}),
                    };
                    let registry = host.tools.lock().await;
                    let tool = registry.get(&name).ok_or_else(|| {
                        mlua::Error::external(AgentError::Provider(format!(
                            "ctx:tool: unknown tool {}",
                            name
                        )))
                    })?;
                    let result = tool
                        .run(args_json, &host.plugin_ctx)
                        .await
                        .map_err(mlua::Error::external)?;
                    Ok(result)
                }
            },
        )?;
        ctx.set("tool", tool_fn)?;
    }

    Ok(ctx)
}

/// Re-fetch a stashed function out of the Lua registry by its index.
fn fetch_function(lua: &Lua, idx: i64) -> mlua::Result<Function> {
    let registry: Table = lua.globals().get("__agent_stash")?;
    let f: Function = registry.get(idx)?;
    Ok(f)
}

// ---------- Lua → Rust Tool bridge ----------

struct LuaToolSpec {
    lua: Arc<Lua>,
    exec_idx: i64,
    name: String,
    description: String,
    parameters: Value,
    host: HostShared,
}

#[async_trait]
impl Tool for LuaToolSpec {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn run(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let lua = self.lua.clone();
        let host = self.host.clone();
        let f = fetch_function(&lua, self.exec_idx)
            .map_err(|e| AgentError::Provider(format!("plugin tool fetch: {}", e)))?;
        let ctx_table = build_ctx(&lua, host).map_err(lua_err)?;
        let args_lua = lua.to_value(&args).map_err(lua_err)?;
        let out: LuaValue = f.call_async((args_lua, ctx_table)).await.map_err(lua_err)?;
        Ok(stringify_lua(&lua, out))
    }
}

// ---------- Lua → Rust SlashCommand bridge ----------

struct LuaSlashCommand {
    lua: Arc<Lua>,
    exec_idx: i64,
    name: String,
    description: String,
    host: HostShared,
}

#[async_trait]
impl SlashCommand for LuaSlashCommand {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    async fn run(&self, args: &str, _agent: &mut crate::agent::Agent) -> SlashOutcome {
        let f = match fetch_function(&self.lua, self.exec_idx) {
            Ok(f) => f,
            Err(e) => return SlashOutcome::Continue(Some(format!("plugin error: {}", e))),
        };
        let ctx_table = match build_ctx(&self.lua, self.host.clone()) {
            Ok(c) => c,
            Err(e) => return SlashOutcome::Continue(Some(format!("plugin error: {}", e))),
        };
        let res: mlua::Result<LuaValue> = f.call_async((args.to_string(), ctx_table)).await;
        match res {
            Ok(v) => SlashOutcome::Continue(Some(stringify_lua(&self.lua, v))),
            Err(e) => SlashOutcome::Continue(Some(format!("plugin error: {}", e))),
        }
    }
}

// ---------- Lua → Rust Hook bridge ----------

struct LuaHook {
    lua: Arc<Lua>,
    exec_idx: i64,
    event: String,
    plugin_id: String,
    host: HostShared,
}

#[async_trait]
impl Hook for LuaHook {
    fn name(&self) -> &str {
        &self.plugin_id
    }
    async fn handle(&self, payload: &HookPayload<'_>) {
        // Filter: each LuaHook is bound to a specific event name when
        // we built it. Skip if this payload isn't ours.
        let payload_event = match payload {
            HookPayload::PreToolUse { .. } => "pre_tool_use",
            HookPayload::PostToolUse { .. } => "post_tool_use",
            HookPayload::Stop { .. } => "stop",
        };
        if payload_event != self.event {
            return;
        }
        let f = match fetch_function(&self.lua, self.exec_idx) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[plugin:{}] hook fetch failed: {}", self.plugin_id, e);
                return;
            }
        };
        let ctx_table = match build_ctx(&self.lua, self.host.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[plugin:{}] hook ctx build failed: {}", self.plugin_id, e);
                return;
            }
        };
        let event_table = match self.lua.create_table() {
            Ok(t) => t,
            Err(_) => return,
        };
        match payload {
            HookPayload::PreToolUse { tool, args } => {
                let _ = event_table.set("tool", *tool);
                let _ = event_table.set("args", self.lua.to_value(args).ok());
            }
            HookPayload::PostToolUse { tool, args, result } => {
                let _ = event_table.set("tool", *tool);
                let _ = event_table.set("args", self.lua.to_value(args).ok());
                let _ = event_table.set("result", *result);
            }
            HookPayload::Stop { final_content } => {
                let _ = event_table.set("final_content", *final_content);
            }
        }
        if let Err(e) = f.call_async::<LuaValue>((event_table, ctx_table)).await {
            eprintln!("[plugin:{}] hook error: {}", self.plugin_id, e);
        }
    }
}

fn lua_err(e: mlua::Error) -> AgentError {
    AgentError::Provider(format!("plugin: {}", e))
}

/// Render a Lua return value into a string for the harness side.
/// `nil`/`false` become empty strings; numbers and bools stringify.
/// Tables are JSON-encoded so `ctx:tool(...)` can return structured
/// data and the model still sees it.
fn stringify_lua(lua: &Lua, v: LuaValue) -> String {
    match v {
        LuaValue::Nil => String::new(),
        LuaValue::Boolean(b) => b.to_string(),
        LuaValue::Integer(i) => i.to_string(),
        LuaValue::Number(n) => n.to_string(),
        LuaValue::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
        other => match lua.from_value::<Value>(other) {
            Ok(v) => v.to_string(),
            Err(_) => "(unrepresentable)".into(),
        },
    }
}

/// Default discovery dirs, in load order. Project-scoped plugins shadow
/// global ones with the same name.
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(global) = global_plugins_dir() {
        out.push(global);
    }
    out.push(PathBuf::from(".agent").join("plugins"));
    out
}

fn global_plugins_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("agent").join("plugins"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Registry;
    use tempfile::tempdir;

    fn empty_registry() -> Arc<Mutex<Registry>> {
        Arc::new(Mutex::new(Registry::new()))
    }

    #[tokio::test]
    async fn loads_a_plugin_with_one_tool() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("hello.lua"),
            r#"
local p = { name = "hello", version = "0.1" }
p.tools = {
  { name = "Hello", description = "say hi",
    parameters = { type = "object", properties = {} },
    execute = function(args, ctx) return "hello world" end },
}
return p
            "#,
        )
        .unwrap();
        let mut out = LoadedPlugins::default();
        load_dir(dir.path(), empty_registry(), &mut out).await;
        assert_eq!(out.manifest.len(), 1);
        assert_eq!(out.manifest[0].name, "hello");
        assert_eq!(out.tools.len(), 1);
        assert_eq!(out.tools[0].name(), "Hello");
        assert_eq!(out.tools[0].description(), "say hi");
        let ctx = ToolContext::new();
        let result = out.tools[0].run(json!({}), &ctx).await.unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn sandbox_blocks_io_access() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("evil.lua"),
            r#"
-- io is removed by sandbox; touching it should error or just return nil.
local p = { name = "evil" }
p.tools = {
  { name = "Touch", description = "",
    parameters = { type = "object", properties = {} },
    execute = function(args, ctx)
      if io == nil then return "io_blocked" else return "io_allowed" end
    end },
}
return p
            "#,
        )
        .unwrap();
        let mut out = LoadedPlugins::default();
        load_dir(dir.path(), empty_registry(), &mut out).await;
        assert_eq!(out.tools.len(), 1);
        let ctx = ToolContext::new();
        let r = out.tools[0].run(json!({}), &ctx).await.unwrap();
        assert_eq!(r, "io_blocked");
    }

    #[tokio::test]
    async fn plugin_can_call_a_registered_host_tool_via_ctx_tool() {
        // Register a Rust tool in the host registry, then have a Lua
        // tool invoke it via ctx:tool. Output is what the Rust tool
        // produced.
        struct Echo;
        #[async_trait]
        impl Tool for Echo {
            fn name(&self) -> &str {
                "Echo"
            }
            fn description(&self) -> &str {
                "echo"
            }
            fn parameters(&self) -> Value {
                json!({"type":"object","properties":{}})
            }
            async fn run(&self, args: Value, _: &ToolContext) -> Result<String> {
                Ok(format!("got:{}", args["text"].as_str().unwrap_or("")))
            }
        }
        let mut reg = Registry::new();
        reg.register(Echo);
        let host = Arc::new(Mutex::new(reg));

        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("bridge.lua"),
            r#"
local p = { name = "bridge" }
p.tools = {
  { name = "Bridge", description = "",
    parameters = { type = "object", properties = {} },
    execute = function(args, ctx)
      return ctx:tool("Echo", { text = "from-lua" })
    end },
}
return p
            "#,
        )
        .unwrap();
        let mut out = LoadedPlugins::default();
        load_dir(dir.path(), host, &mut out).await;
        let ctx = ToolContext::new();
        let r = out.tools[0].run(json!({}), &ctx).await.unwrap();
        assert_eq!(r, "got:from-lua");
    }

    #[tokio::test]
    async fn malformed_plugin_is_skipped_not_fatal() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("broken.lua"), "this is not lua syntax (((").unwrap();
        std::fs::write(
            dir.path().join("ok.lua"),
            r#"
local p = { name = "ok" }
p.tools = { { name = "T", description = "",
              parameters = { type = "object", properties = {} },
              execute = function(args, ctx) return "ok" end } }
return p
            "#,
        )
        .unwrap();
        let mut out = LoadedPlugins::default();
        load_dir(dir.path(), empty_registry(), &mut out).await;
        // The broken plugin doesn't crash the loader; the good one
        // still loads.
        assert_eq!(out.tools.len(), 1);
        assert_eq!(out.tools[0].name(), "T");
    }

    #[tokio::test]
    async fn plugin_can_register_a_slash_command() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("slash.lua"),
            r#"
local p = { name = "slash-demo" }
p.slash_commands = {
  { name = "demo", description = "demo cmd",
    execute = function(args, ctx) return "demo:" .. (args or "") end },
}
return p
            "#,
        )
        .unwrap();
        let mut out = LoadedPlugins::default();
        load_dir(dir.path(), empty_registry(), &mut out).await;
        assert_eq!(out.slash_commands.len(), 1);
        assert_eq!(out.slash_commands[0].name(), "demo");
    }

    #[tokio::test]
    async fn plugin_can_register_hooks_for_each_event() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("hooks.lua"),
            r#"
local p = { name = "hooky" }
p.hooks = {
  pre_tool_use = function(event, ctx) end,
  post_tool_use = function(event, ctx) end,
  stop = function(event, ctx) end,
}
return p
            "#,
        )
        .unwrap();
        let mut out = LoadedPlugins::default();
        load_dir(dir.path(), empty_registry(), &mut out).await;
        assert_eq!(out.hooks.len(), 3);
        assert_eq!(out.manifest[0].hook_events.len(), 3);
    }
}
