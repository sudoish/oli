use async_trait::async_trait;
use std::collections::HashMap;

use super::{
    Clear, Compact, ConfigCmd, Cost, Diagnostics, Exit, Help, MemoryCmd, Model, Paths, Plugins,
    Provider, Sessions, System, Tools,
};
use crate::agent::Agent;

const BUILTIN_NAMES: &[&str] = &[
    "clear",
    "help",
    "cost",
    "tools",
    "system",
    "memory",
    "compact",
    "provider",
    "model",
    "sessions",
    "plugins",
    "config",
    "diagnostics",
    "paths",
    "exit",
];

pub(crate) fn is_builtin_name(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}

#[async_trait]
pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    async fn run(&self, args: &str, agent: &mut Agent) -> SlashOutcome;
}

pub enum SlashOutcome {
    /// Continue the REPL. Optional message is printed to stdout.
    Continue(Option<String>),
    /// Tear down the REPL.
    Exit,
    /// Reload triggered. The slash command has already mutated the
    /// agent's tool/hook registries in place. The REPL drops every
    /// slash whose name appears in `removed_names` and registers
    /// each of `added_slashes` in their place.
    Rebuild {
        removed_names: Vec<String>,
        added_slashes: Vec<Box<dyn SlashCommand>>,
        message: String,
    },
}

// Manual PartialEq for SlashOutcome — Box<dyn SlashCommand> doesn't
// implement Eq, so we compare the variant + the comparable fields.
// Tests only care about the `Continue(_)` and `Exit` shapes anyway.
impl PartialEq for SlashOutcome {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Continue(a), Self::Continue(b)) => a == b,
            (Self::Exit, Self::Exit) => true,
            (
                Self::Rebuild {
                    removed_names: a,
                    message: ma,
                    ..
                },
                Self::Rebuild {
                    removed_names: b,
                    message: mb,
                    ..
                },
            ) => a == b && ma == mb,
            _ => false,
        }
    }
}

impl std::fmt::Debug for SlashOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Continue(s) => write!(f, "Continue({:?})", s),
            Self::Exit => write!(f, "Exit"),
            Self::Rebuild {
                removed_names,
                added_slashes,
                message,
            } => f
                .debug_struct("Rebuild")
                .field("removed_names", removed_names)
                .field("added_slashes_count", &added_slashes.len())
                .field("message", message)
                .finish(),
        }
    }
}

#[derive(Default)]
pub struct SlashRegistry {
    commands: HashMap<String, Box<dyn SlashCommand>>,
    order: Vec<String>,
}

impl SlashRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<C: SlashCommand + 'static>(&mut self, cmd: C) {
        self.register_box(Box::new(cmd));
    }

    /// Insert an already-boxed slash command. Plugin loader uses this.
    pub fn register_box(&mut self, cmd: Box<dyn SlashCommand>) {
        let name = cmd.name().to_string();
        if !self.commands.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.commands.insert(name, cmd);
    }

    pub fn get(&self, name: &str) -> Option<&dyn SlashCommand> {
        self.commands.get(name).map(|b| b.as_ref())
    }

    /// Drop a registered command by name. Used by `/plugins reload`
    /// to clear stale plugin slashes before adding the fresh batch.
    /// Returns `true` if a command with that name was present.
    pub fn remove(&mut self, name: &str) -> bool {
        let had = self.commands.remove(name).is_some();
        if had {
            self.order.retain(|n| n != name);
        }
        had
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn SlashCommand> {
        self.order
            .iter()
            .filter_map(|n| self.commands.get(n).map(|b| b.as_ref()))
    }

    /// Parse a line that begins with `/` (caller strips the slash) into a
    /// `(command, rest)` pair and dispatch. Returns `None` if the command is
    /// unknown — caller decides how to surface that.
    pub async fn dispatch(&self, line: &str, agent: &mut Agent) -> Option<SlashOutcome> {
        let line = line.trim_start();
        let (name, rest) = match line.find(char::is_whitespace) {
            Some(i) => (&line[..i], line[i..].trim_start()),
            None => (line, ""),
        };
        let cmd = self.get(name)?;
        Some(cmd.run(rest, agent).await)
    }

    /// Default REPL command set without a `/plugins` reloader.
    /// Tests reach for this since they don't exercise plugin
    /// reloading; the binary calls `default_set_with_reloader`.
    /// Order is the order they show up in `/help`.
    #[cfg(test)]
    pub fn default_set() -> Self {
        Self::default_set_with_reloader(None)
    }

    /// Same as `default_set`, but `Plugins` carries the supplied
    /// reloader so `/plugins reload` re-scans the plugin dirs and
    /// swaps registrations in place.
    pub fn default_set_with_reloader(
        reloader: Option<std::sync::Arc<crate::plugins::PluginReloader>>,
    ) -> Self {
        let mut r = Self::new();
        r.register(Clear);
        r.register(Help);
        r.register(Cost);
        r.register(Tools);
        r.register(System);
        r.register(MemoryCmd);
        r.register(Compact);
        r.register(Provider);
        r.register(Model);
        r.register(Sessions);
        r.register(Plugins::new(reloader));
        r.register(ConfigCmd);
        r.register(Diagnostics);
        r.register(Paths);
        r.register(Exit);
        debug_assert_eq!(
            r.order.iter().map(String::as_str).collect::<Vec<_>>(),
            BUILTIN_NAMES
        );
        r
    }
}

/// Render a `/help` listing for a registry. Kept here so the REPL can call it
/// directly without re-implementing trait introspection.
pub fn render_help(reg: &SlashRegistry) -> String {
    let mut out = String::from("Available commands:\n");
    for cmd in reg.iter() {
        out.push_str(&format!("  /{:<10} {}\n", cmd.name(), cmd.description()));
    }
    out.push_str("Ctrl-C cancels the current turn; Ctrl-D exits.");
    out
}
