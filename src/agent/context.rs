use std::path::{Path, PathBuf};
use tokio::process::Command;

const SYSTEM_PREAMBLE: &str = "\
You are an interactive coding agent operating against the user's local repository.

You have these tools: Read, Write, Edit, Bash, Grep, Glob.

Guidelines:
- Use Glob/Grep to navigate before reading whole files.
- Pass `offset`/`limit` to Read on large files.
- Edit requires the file to have been Read first this session.
- Prefer minimal, focused changes over broad refactors.
- Report concisely; don't restate the diff back to the user.";

const MAX_DIR_LISTING_ENTRIES: usize = 50;
const MAX_PORCELAIN_LINES: usize = 20;
const MAX_AGENT_CONTEXT_BYTES: usize = 16_000;

// Project-level context files, read at every level walking up from cwd.
// Order is the on-disk inclusion order at a given directory: AGENTS.md
// (Codex/Cursor convention) first, then CLAUDE.md (Claude Code convention).
const AGENT_CONTEXT_FILENAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// Discovers per-session environment context (cwd, OS, git state, project
/// files, AGENTS.md/CLAUDE.md content) and renders it as a system prompt.
///
/// Splitting each section into a method keeps the unit tests targeted —
/// a tempdir + a constructed builder can exercise any one piece without
/// shelling out to the real cwd.
pub struct SystemPromptBuilder {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
}

impl SystemPromptBuilder {
    pub fn from_env() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    }

    pub async fn build(&self) -> String {
        let mut out = String::from(SYSTEM_PREAMBLE);
        out.push_str("\n\n");
        out.push_str(&self.environment_section());

        if let Some(g) = self.git_section().await {
            out.push_str("\n\n");
            out.push_str(&g);
        }
        if let Some(d) = self.dir_listing_section().await {
            out.push_str("\n\n");
            out.push_str(&d);
        }
        if let Some(c) = self.agent_context_section().await {
            out.push_str("\n\n");
            out.push_str(&c);
        }
        out
    }

    fn environment_section(&self) -> String {
        format!(
            "# Environment\nWorking directory: {}\nPlatform: {} ({})",
            self.cwd.display(),
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    }

    async fn git_section(&self) -> Option<String> {
        if !is_git_repo(&self.cwd).await {
            return None;
        }

        let branch = run_git(&self.cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap_or_else(|| "(unknown)".into());

        let porcelain = run_git(&self.cwd, &["status", "--porcelain=v1"])
            .await
            .unwrap_or_default();

        let status_summary = if porcelain.trim().is_empty() {
            "clean".to_string()
        } else {
            let total = porcelain.lines().count();
            let head: Vec<&str> = porcelain.lines().take(MAX_PORCELAIN_LINES).collect();
            let mut s = head.join("\n");
            if total > head.len() {
                s.push_str(&format!("\n... and {} more", total - head.len()));
            }
            s
        };

        let log = run_git(&self.cwd, &["log", "-3", "--oneline"])
            .await
            .unwrap_or_default();

        let mut out = format!("# Git\nBranch: {}\nStatus: {}", branch, status_summary);
        if !log.trim().is_empty() {
            out.push_str("\n\nRecent commits:\n");
            out.push_str(log.trim());
        }
        Some(out)
    }

    async fn dir_listing_section(&self) -> Option<String> {
        let mut rd = tokio::fs::read_dir(&self.cwd).await.ok()?;
        let mut names = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let raw = entry.file_name().to_string_lossy().to_string();
            // Skip hidden entries except a couple of useful ones.
            if raw.starts_with('.') && raw != ".claude" && raw != ".agent" {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            names.push(if is_dir { format!("{}/", raw) } else { raw });
        }
        if names.is_empty() {
            return None;
        }
        names.sort();
        let total = names.len();
        names.truncate(MAX_DIR_LISTING_ENTRIES);
        let mut body = format!("# Project files\n{}", names.join("\n"));
        if total > MAX_DIR_LISTING_ENTRIES {
            body.push_str(&format!(
                "\n... and {} more",
                total - MAX_DIR_LISTING_ENTRIES
            ));
        }
        Some(body)
    }

    async fn agent_context_section(&self) -> Option<String> {
        let mut sources: Vec<(PathBuf, String)> = Vec::new();

        // Walk-up from cwd to root, collecting AGENTS.md/CLAUDE.md at each
        // level. Reverse so root-first (broader) appears before cwd-level
        // (narrower) — model reads scope from outermost to innermost.
        let mut walk: Vec<PathBuf> = Vec::new();
        let mut p: &Path = self.cwd.as_path();
        loop {
            walk.push(p.to_path_buf());
            match p.parent() {
                Some(parent) if parent != p => p = parent,
                _ => break,
            }
        }
        walk.reverse();

        for dir in walk {
            for name in AGENT_CONTEXT_FILENAMES {
                let candidate = dir.join(name);
                if let Ok(body) = tokio::fs::read_to_string(&candidate).await {
                    sources.push((candidate, body));
                }
            }
        }

        // User-level overlays last. Mirror the conventions of each tool:
        // Claude Code reads ~/.claude/CLAUDE.md, Codex reads ~/.codex/AGENTS.md.
        if let Some(home) = &self.home {
            for (subdir, name) in [(".claude", "CLAUDE.md"), (".codex", "AGENTS.md")] {
                let global = home.join(subdir).join(name);
                if let Ok(body) = tokio::fs::read_to_string(&global).await {
                    sources.push((global, body));
                }
            }
        }

        if sources.is_empty() {
            return None;
        }

        let mut out = String::from("# Agent context");
        let mut total = 0usize;
        for (path, body) in sources {
            let header = format!("\n\n## {}\n", path.display());
            let body_trim = body.trim();
            let chunk_len = header.len() + body_trim.len();

            if total + chunk_len <= MAX_AGENT_CONTEXT_BYTES {
                out.push_str(&header);
                out.push_str(body_trim);
                total += chunk_len;
                continue;
            }

            // Partial fit, then stop.
            let remaining = MAX_AGENT_CONTEXT_BYTES.saturating_sub(total);
            if remaining > header.len() + 32 {
                out.push_str(&header);
                let body_room = remaining - header.len() - 32;
                let mut cut = body_room.min(body_trim.len());
                while cut > 0 && !body_trim.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.push_str(&body_trim[..cut]);
                out.push_str("\n[... truncated ...]");
            }
            break;
        }
        Some(out)
    }
}

async fn is_git_repo(cwd: &Path) -> bool {
    Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn builder_at(cwd: PathBuf, home: Option<PathBuf>) -> SystemPromptBuilder {
        SystemPromptBuilder { cwd, home }
    }

    #[tokio::test]
    async fn environment_section_includes_cwd_and_platform() {
        let dir = tempdir().unwrap();
        let b = builder_at(dir.path().to_path_buf(), None);
        let s = b.environment_section();
        assert!(s.contains("Working directory:"));
        assert!(s.contains(&dir.path().display().to_string()));
        assert!(s.contains(std::env::consts::OS));
    }

    #[tokio::test]
    async fn dir_listing_lists_files_and_dirs_skipping_hidden() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("README.md"), "")
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join("src")).await.unwrap();
        tokio::fs::create_dir(dir.path().join(".git"))
            .await
            .unwrap();

        let b = builder_at(dir.path().to_path_buf(), None);
        let s = b.dir_listing_section().await.unwrap();
        assert!(s.contains("README.md"));
        assert!(s.contains("src/"));
        assert!(!s.contains(".git"));
    }

    #[tokio::test]
    async fn dir_listing_returns_none_for_missing_dir() {
        let b = builder_at(PathBuf::from("/tmp/__nope_dir_for_listing__"), None);
        assert!(b.dir_listing_section().await.is_none());
    }

    #[tokio::test]
    async fn agent_context_reads_claude_md_from_cwd() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("CLAUDE.md"), "# Project\nbe terse")
            .await
            .unwrap();

        let b = builder_at(dir.path().to_path_buf(), None);
        let s = b.agent_context_section().await.unwrap();
        assert!(s.contains("# Agent context"));
        assert!(s.contains("be terse"));
        assert!(s.contains("CLAUDE.md"));
    }

    #[tokio::test]
    async fn agent_context_reads_agents_md_from_cwd() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("AGENTS.md"), "# Agents\nuse pnpm")
            .await
            .unwrap();

        let b = builder_at(dir.path().to_path_buf(), None);
        let s = b.agent_context_section().await.unwrap();
        assert!(s.contains("# Agent context"));
        assert!(s.contains("use pnpm"));
        assert!(s.contains("AGENTS.md"));
    }

    #[tokio::test]
    async fn agent_context_includes_both_files_when_present_agents_first() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("AGENTS.md"), "AGENTS-BODY")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("CLAUDE.md"), "CLAUDE-BODY")
            .await
            .unwrap();

        let b = builder_at(dir.path().to_path_buf(), None);
        let s = b.agent_context_section().await.unwrap();
        let a_pos = s.find("AGENTS-BODY").unwrap();
        let c_pos = s.find("CLAUDE-BODY").unwrap();
        assert!(
            a_pos < c_pos,
            "AGENTS.md should appear before CLAUDE.md at the same level (alphabetical)"
        );
    }

    #[tokio::test]
    async fn agent_context_walks_up_to_parent_dirs_root_first() {
        let root = tempdir().unwrap();
        let nested = root.path().join("a/b");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(root.path().join("CLAUDE.md"), "ROOT-LEVEL")
            .await
            .unwrap();
        tokio::fs::write(nested.join("CLAUDE.md"), "NESTED-LEVEL")
            .await
            .unwrap();

        let b = builder_at(nested.clone(), None);
        let s = b.agent_context_section().await.unwrap();
        let root_pos = s.find("ROOT-LEVEL").unwrap();
        let nested_pos = s.find("NESTED-LEVEL").unwrap();
        assert!(
            root_pos < nested_pos,
            "root-level file should appear before nested one"
        );
    }

    #[tokio::test]
    async fn agent_context_walks_up_for_agents_md() {
        let root = tempdir().unwrap();
        let nested = root.path().join("a/b");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(root.path().join("AGENTS.md"), "ROOT-AGENTS")
            .await
            .unwrap();

        let b = builder_at(nested, None);
        let s = b.agent_context_section().await.unwrap();
        assert!(s.contains("ROOT-AGENTS"));
    }

    #[tokio::test]
    async fn agent_context_reads_global_claude_md_via_home() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        tokio::fs::create_dir(home.path().join(".claude"))
            .await
            .unwrap();
        tokio::fs::write(
            home.path().join(".claude").join("CLAUDE.md"),
            "GLOBAL-USER-RULES",
        )
        .await
        .unwrap();

        let b = builder_at(dir.path().to_path_buf(), Some(home.path().to_path_buf()));
        let s = b.agent_context_section().await.unwrap();
        assert!(s.contains("GLOBAL-USER-RULES"));
    }

    #[tokio::test]
    async fn agent_context_reads_global_codex_agents_md_via_home() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        tokio::fs::create_dir(home.path().join(".codex"))
            .await
            .unwrap();
        tokio::fs::write(
            home.path().join(".codex").join("AGENTS.md"),
            "GLOBAL-AGENT-RULES",
        )
        .await
        .unwrap();

        let b = builder_at(dir.path().to_path_buf(), Some(home.path().to_path_buf()));
        let s = b.agent_context_section().await.unwrap();
        assert!(s.contains("GLOBAL-AGENT-RULES"));
    }

    #[tokio::test]
    async fn agent_context_section_is_none_when_nothing_found() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        let b = builder_at(dir.path().to_path_buf(), Some(home.path().to_path_buf()));
        assert!(b.agent_context_section().await.is_none());
    }

    #[tokio::test]
    async fn git_section_is_none_outside_a_repo() {
        let dir = tempdir().unwrap();
        let b = builder_at(dir.path().to_path_buf(), None);
        assert!(b.git_section().await.is_none());
    }

    #[tokio::test]
    async fn git_section_reports_branch_status_and_recent_commits() {
        let dir = tempdir().unwrap();
        // Real git init + commit.
        let init_ok = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-b", "main"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !init_ok {
            eprintln!("skipping: git unavailable");
            return;
        }
        // Configure identity locally so commit succeeds.
        std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["config", "user.email", "t@example.com"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["config", "user.name", "Tester"])
            .output()
            .unwrap();
        tokio::fs::write(dir.path().join("hi.txt"), "hello")
            .await
            .unwrap();
        std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["add", "hi.txt"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["commit", "-m", "initial"])
            .output()
            .unwrap();

        let b = builder_at(dir.path().to_path_buf(), None);
        let s = b.git_section().await.unwrap();
        assert!(s.contains("Branch: main"));
        assert!(s.contains("Status: clean"));
        assert!(s.contains("Recent commits:"));
        assert!(s.contains("initial"));
    }

    #[tokio::test]
    async fn git_section_reports_dirty_status_summary() {
        let dir = tempdir().unwrap();
        if std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-b", "main"])
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping: git unavailable");
            return;
        }
        tokio::fs::write(dir.path().join("untracked.txt"), "x")
            .await
            .unwrap();

        let b = builder_at(dir.path().to_path_buf(), None);
        let s = b.git_section().await.unwrap();
        assert!(s.contains("untracked.txt"));
        assert!(!s.contains("Status: clean"));
    }
}
