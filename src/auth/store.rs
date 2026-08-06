//! On-disk token store: `$XDG_CONFIG_HOME/oli/auth.json`, mode 0600.
//!
//! Sits next to `config.toml` (same directory helper,
//! [`crate::config::config_dir`]) but in a separate file, because the
//! two have different lifetimes and different sensitivity: config is
//! hand-edited and worth committing to a dotfiles repo, auth.json is
//! machine-written, secret, and must never be.
//!
//! Writes go through a temp file plus a rename so a crash mid-write
//! can't leave a truncated bundle where a valid one used to be. The
//! temp file is created 0600 from the start — never 0644-then-chmod,
//! which would leave a window where the refresh token is world-readable.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::auth::token::Tokens;
use crate::error::{AgentError, Result};

/// Filename inside the oli config directory.
pub const AUTH_FILE: &str = "auth.json";

/// Permission bits the store writes and expects. Owner read/write only.
#[cfg(unix)]
pub const AUTH_FILE_MODE: u32 = 0o600;

/// Handle to the token file. Cheap; holds only a path.
#[derive(Clone, Debug)]
pub struct AuthStore {
    path: PathBuf,
}

impl AuthStore {
    /// Store at an explicit path. The test seam — every filesystem
    /// test points this at a tempdir rather than the real config dir.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Store at `$XDG_CONFIG_HOME/oli/auth.json`.
    pub fn default_location() -> Result<Self> {
        Ok(Self::at(default_auth_path().ok_or_else(|| {
            AgentError::Auth(
                "cannot locate a config directory (neither XDG_CONFIG_HOME nor HOME is set); \
                 set one, or use API-key auth instead"
                    .to_string(),
            )
        })?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether a token bundle exists on disk. Does not validate it.
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Read the stored bundle. `Ok(None)` means "not logged in" — an
    /// absent file is a normal state, not an error. A *present but
    /// unreadable* file is an error: silently treating corruption as
    /// logged-out would send the user round the login loop with no
    /// explanation.
    pub fn load(&self) -> Result<Option<Tokens>> {
        let body = match std::fs::read_to_string(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(AgentError::Auth(format!(
                    "cannot read {}: {e}",
                    self.path.display()
                )));
            }
        };
        #[cfg(unix)]
        self.warn_if_permissions_are_loose();
        let tokens: Tokens = serde_json::from_str(&body).map_err(|e| {
            AgentError::Auth(format!(
                "{} is corrupt ({e}); delete it and run `oli login` again, \
                 or switch the provider back to API-key auth",
                self.path.display()
            ))
        })?;
        Ok(Some(tokens))
    }

    /// Write the bundle, creating the parent directory if needed.
    /// Replaces any existing file atomically.
    pub fn save(&self, tokens: &Tokens) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Auth(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        let body = serde_json::to_string_pretty(tokens)?;

        // Same directory as the target so the rename stays on one
        // filesystem, and per-process so two concurrent logins don't
        // scribble over each other's temp file.
        let tmp = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));
        let mut file = open_private(&tmp)?;
        let write = file
            .write_all(body.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all());
        if let Err(e) = write {
            let _ = std::fs::remove_file(&tmp);
            return Err(AgentError::Auth(format!(
                "cannot write {}: {e}",
                tmp.display()
            )));
        }
        drop(file);

        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            AgentError::Auth(format!("cannot replace {}: {e}", self.path.display()))
        })
    }

    /// Remove the stored bundle. Absent file is success — `oli logout`
    /// twice should not be an error.
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AgentError::Auth(format!(
                "cannot remove {}: {e}",
                self.path.display()
            ))),
        }
    }

    /// Warn (don't fail) when the file is readable by group or other.
    /// Failing would strand a user whose umask predates this code; a
    /// warning tells them to fix it without blocking work.
    #[cfg(unix)]
    fn warn_if_permissions_are_loose(&self) {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&self.path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                crate::log_warn!(
                    "[auth] {} is mode {:o}; it holds a refresh token and should be 600. \
                     Run: chmod 600 {}",
                    self.path.display(),
                    mode,
                    self.path.display()
                );
            }
        }
    }
}

/// Create `path` for writing, truncating, with owner-only permissions
/// applied at creation time rather than after.
#[cfg(unix)]
fn open_private(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(AUTH_FILE_MODE)
        .open(path)
        .map_err(|e| AgentError::Auth(format!("cannot create {}: {e}", path.display())))
}

/// Non-unix fallback. Oli targets macOS and Linux; this exists so the
/// crate still compiles elsewhere, without pretending to protect the
/// file.
#[cfg(not(unix))]
fn open_private(path: &Path) -> Result<std::fs::File> {
    std::fs::File::create(path)
        .map_err(|e| AgentError::Auth(format!("cannot create {}: {e}", path.display())))
}

/// `$XDG_CONFIG_HOME/oli/auth.json`, or `None` when no config
/// directory can be determined.
pub fn default_auth_path() -> Option<PathBuf> {
    Some(crate::config::config_dir()?.join(AUTH_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tokens {
        Tokens {
            access_token: "access-1".into(),
            refresh_token: "refresh-1".into(),
            id_token: "header.payload.sig".into(),
            account_id: Some("acct-7".into()),
            last_refresh: Some(1_700_000_000),
        }
    }

    fn store_in(dir: &tempfile::TempDir) -> AuthStore {
        AuthStore::at(dir.path().join("oli").join(AUTH_FILE))
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn round_trips_a_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.save(&sample()).unwrap();
        assert_eq!(store.load().unwrap(), Some(sample()));
    }

    #[test]
    fn load_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        assert!(!store.exists());
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn save_creates_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthStore::at(dir.path().join("a").join("b").join(AUTH_FILE));
        store.save(&sample()).unwrap();
        assert!(store.exists());
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_mode_600() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.save(&sample()).unwrap();
        assert_eq!(mode_of(store.path()), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn overwriting_a_world_readable_file_restores_mode_600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.save(&sample()).unwrap();
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();

        store.save(&sample()).unwrap();

        // The rename brings the temp file's 0600 with it, so a
        // previously-loose file is tightened rather than inherited.
        assert_eq!(mode_of(store.path()), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_are_warned_about_but_still_load() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.save(&sample()).unwrap();
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();

        // Warning, not failure: a bad umask must not strand the user.
        assert_eq!(store.load().unwrap(), Some(sample()));
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.save(&sample()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().contains("tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn corrupt_file_is_an_error_naming_the_path_and_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), "{ this is not json").unwrap();

        let err = store.load().unwrap_err().to_string();
        assert!(err.contains("auth.json"), "error lost the path: {err}");
        assert!(err.contains("oli login"), "error lost the remedy: {err}");
        assert!(
            err.contains("API-key"),
            "error must name the API-key fallback: {err}"
        );
    }

    #[test]
    fn clear_removes_the_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.save(&sample()).unwrap();
        store.clear().unwrap();
        assert!(!store.exists());
        store.clear().unwrap();
    }

    #[test]
    fn saving_twice_replaces_rather_than_appends() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.save(&sample()).unwrap();
        let mut second = sample();
        second.access_token = "access-2".into();
        store.save(&second).unwrap();
        assert_eq!(store.load().unwrap().unwrap().access_token, "access-2");
    }

    #[test]
    fn default_auth_path_sits_next_to_the_config_file() {
        // Both derive from `config::config_dir`, so whatever the
        // environment says, they share a parent.
        if let (Some(auth), Some(cfg)) = (default_auth_path(), crate::config::default_config_path())
        {
            assert_eq!(auth.parent(), cfg.parent());
            assert_eq!(auth.file_name().unwrap(), AUTH_FILE);
        }
    }
}
