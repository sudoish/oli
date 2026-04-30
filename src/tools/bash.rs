use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::error::{Result, ToolError};
use crate::tools::util::{DEFAULT_MAX_OUTPUT_BYTES, truncate};
use crate::tools::{Tool, ToolContext};

pub struct Bash;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its combined stdout and stderr. \
         Optional `cwd` (working directory; sticky across calls) and \
         `timeout_ms` (default 120000, max 600000). On timeout the child \
         process is killed and the model sees a timeout marker."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the command. If set, future Bash calls inherit it until another `cwd` is supplied."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Hard timeout in milliseconds (default 120000, max 600000). The child is killed on timeout."
                }
            },
            "required": ["command"]
        })
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: "Bash".into(),
                detail: "missing or non-string `command`".into(),
            })?;

        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);

        let cwd_arg = args.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from);
        if let Some(c) = &cwd_arg {
            ctx.set_cwd(c.clone()).await;
        }
        let cwd = match cwd_arg {
            Some(c) => Some(c),
            None => ctx.cwd().await,
        };

        Ok(truncate(
            &run_bash(command, cwd.as_deref(), Duration::from_millis(timeout_ms)).await,
            DEFAULT_MAX_OUTPUT_BYTES,
        ))
    }
}

async fn run_bash(command: &str, cwd: Option<&Path>, timeout: Duration) -> String {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(command);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // On Unix we own kill via a ProcessGroupKillGuard whose Drop
    // sends SIGKILL to the whole group — strictly more powerful
    // than kill_on_drop, which only reaps the immediate child
    // and leaves grandchildren reparented to PID 1. On Windows
    // (no setsid / killpg) we keep kill_on_drop as the
    // best-effort cleanup.
    #[cfg(not(unix))]
    cmd.kill_on_drop(true);
    // Make the spawned shell its own session + process-group
    // leader so we can SIGKILL the whole tree on cancel /
    // timeout. Without this, killing `sh` leaves any
    // grandchildren (`sleep`, `cargo`, ...) reparented to PID 1
    // and running in the background.
    #[cfg(unix)]
    {
        // tokio::process::Command's pre_exec is a direct method
        // under cfg(unix); no `use std::os::unix::process::CommandExt`
        // needed.
        unsafe {
            cmd.pre_exec(|| {
                // setpgid(0, 0) makes this process a new
                // process group leader (pgid == pid). Killing
                // the group via killpg(pid, SIGKILL) reaches
                // every descendant, including grandchildren
                // reparented to PID 1 after we kill the
                // immediate `sh`. setsid() would also give us
                // a new session, which we don't need.
                if libc::setpgid(0, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("Error executing command: {}", e),
    };

    // Snapshot the pid for the process-group kill BEFORE handing
    // the child to wait_with_output (which consumes it). Naming
    // matters: an `_pg_guard` underscore-prefix can be optimized
    // out of the state machine by some Rust versions on locals
    // unused after an await (the leading underscore signals
    // "I'm not using this" to the compiler). Use a real name and
    // touch it after the await to guarantee state-machine
    // capture.
    // Snapshot the pid for the process-group kill BEFORE handing
    // the child to wait_with_output (which consumes it). The
    // explicit non-underscore name + the explicit drop after the
    // await keep the guard captured by the async state machine
    // (a leading-underscore binding can be optimized out for
    // unused-after-await locals on some Rust versions).
    #[cfg(unix)]
    let pg_guard = child.id().map(|p| ProcessGroupKillGuard(p as i32));

    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;
    #[cfg(unix)]
    drop(pg_guard);

    match result {
        Ok(Ok(output)) => format_output(&output),
        Ok(Err(e)) => format!("Error waiting for command: {}", e),
        // On timeout / cancel the future drops, taking
        // `_pg_guard` and `child` with it. The guard's Drop
        // sends SIGKILL to the negated pid (the process group)
        // — that's how grandchildren get cleaned up.
        Err(_) => format!(
            "Command timed out after {}ms (child killed)",
            timeout.as_millis()
        ),
    }
}

/// On Drop, SIGKILL the entire process group. `killpg` on a
/// non-existent group is a no-op (`ESRCH`), so this is safe to
/// fire even when the bash future completed normally.
#[cfg(unix)]
struct ProcessGroupKillGuard(i32);

#[cfg(unix)]
impl Drop for ProcessGroupKillGuard {
    fn drop(&mut self) {
        unsafe {
            // killpg on a non-existent group returns ESRCH —
            // harmless. On cancel this is the only kill that
            // fires, so the group has to actually exist
            // (set up via pre_exec setpgid).
            libc::killpg(self.0, libc::SIGKILL);
        }
    }
}

fn format_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut result = String::new();
    result.push_str(&stdout);
    if !stderr.is_empty() {
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(&stderr);
    }
    if !output.status.success() {
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".into());
        result.push_str(&format!("Command exited with status: {}", code));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn captures_stdout() {
        let ctx = ToolContext::new();
        let out = Bash
            .run(json!({ "command": "printf hello" }), &ctx)
            .await
            .unwrap();
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn captures_stderr_and_exit_code_for_failure() {
        let ctx = ToolContext::new();
        let out = Bash
            .run(json!({ "command": "echo oops 1>&2 && exit 3" }), &ctx)
            .await
            .unwrap();
        assert!(out.contains("oops"));
        assert!(out.contains("Command exited with status: 3"));
    }

    #[tokio::test]
    async fn missing_arg_is_invalid_args_error() {
        let ctx = ToolContext::new();
        let err = Bash.run(json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("invalid arguments for Bash"));
    }

    #[tokio::test]
    async fn truncates_oversized_output() {
        let ctx = ToolContext::new();
        // Generate 50k bytes of output, which is above DEFAULT_MAX_OUTPUT_BYTES.
        let cmd = format!("printf 'x%.0s' $(seq 1 50000)");
        let out = Bash.run(json!({ "command": cmd }), &ctx).await.unwrap();
        assert!(out.contains("[... output truncated"));
    }

    #[tokio::test]
    async fn cwd_arg_runs_command_in_directory() {
        let dir = tempdir().unwrap();
        let target = dir.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();
        let out = Bash
            .run(
                json!({ "command": "pwd", "cwd": target.clone() }),
                &ctx,
            )
            .await
            .unwrap();
        // macOS canonicalizes /var/folders/... → /private/var/folders/... so we just
        // check that the resolved tempdir basename is in the output rather than
        // string-equality.
        let leaf = dir.path().file_name().unwrap().to_string_lossy().to_string();
        assert!(
            out.contains(&leaf),
            "expected pwd output to contain {}, got {}",
            leaf,
            out
        );
    }

    #[tokio::test]
    async fn cwd_persists_across_subsequent_calls_until_overridden() {
        let dir = tempdir().unwrap();
        let target = dir.path().to_str().unwrap().to_string();
        let ctx = ToolContext::new();

        // Set cwd via first call.
        Bash.run(
            json!({ "command": "true", "cwd": target.clone() }),
            &ctx,
        )
        .await
        .unwrap();

        // Second call without cwd should inherit it.
        let out = Bash
            .run(json!({ "command": "pwd" }), &ctx)
            .await
            .unwrap();
        let leaf = dir.path().file_name().unwrap().to_string_lossy().to_string();
        assert!(
            out.contains(&leaf),
            "second call should inherit cwd, got {}",
            out
        );
    }

    /// Dropping the in-flight Bash future (what the TUI's
    /// Ctrl+C path does when the cancel oneshot fires) returns
    /// promptly *and* takes the whole shell-spawned tree with
    /// it via the process-group SIGKILL. We verify by having
    /// the shell's grandchild (`sleep`) try to write a sentinel
    /// file *after* its sleep; with the process-group kill
    /// active, the grandchild dies and the file never appears.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_future_kills_the_grandchild_via_process_group() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let sentinel = dir.path().join("sentinel");
        let cmd = format!("sleep 1 && touch {}", sentinel.to_str().unwrap());
        let ctx = ToolContext::new();

        let started = std::time::Instant::now();

        // Scope the future so it drops at block exit — NOT at
        // function exit. `tokio::pin!` shadows the binding into a
        // Pin<&mut F> but the underlying F lives on the original
        // local; we need the original to go out of scope before
        // we can verify the kill. Otherwise pg_guard.drop fires
        // after the assertion has already run.
        {
            let bash_fut = Bash.run(
                json!({"command": cmd, "timeout_ms": 30_000}),
                &ctx,
            );
            tokio::pin!(bash_fut);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                _ = &mut bash_fut => panic!("bash returned before cancel"),
            }
        } // bash_fut + the future drop here → pg_guard drop → killpg

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "drop took {:?}",
            started.elapsed()
        );

        // Wait past the original sleep duration. With process-
        // group kill, the grandchild died and the touch never
        // ran — sentinel must not exist.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !sentinel.exists(),
            "sentinel exists at {:?} — process-group kill didn't reach the grandchild",
            sentinel
        );
    }

    #[tokio::test]
    async fn timeout_kills_long_running_command() {
        let ctx = ToolContext::new();
        let started = std::time::Instant::now();
        let out = Bash
            .run(
                json!({ "command": "sleep 10", "timeout_ms": 200 }),
                &ctx,
            )
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert!(out.contains("timed out"), "expected timeout marker: {}", out);
        assert!(
            elapsed < Duration::from_secs(2),
            "timeout should fire within ~2s, took {:?}",
            elapsed
        );
    }
}
