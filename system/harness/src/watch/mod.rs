//! `hex watch` — the general watcher: "when X happens, do Y once, loudly,
//! and never wait forever" (Standing Order S10: hex owns the wait).
//!
//! Canonical interface and behavior spec: `docs/hex-watch.md`. Change the
//! spec first, then this code and its tests.
//!
//! Layering (same shape as `hitl`):
//! - `store`   owns the on-disk records (`$HEX_DIR/.hex/watch/`)
//! - `sources` turns a source + match into hits (`gmail` command, `event` state)
//! - `tick`    the loop: expiry, poll, since guard, claim, action, outcome, emit
//! - `notify`  session inbox delivery (`hex watch notify`, `hex watch inbox`)
//!
//! The tick talks to the outside world only through the [`Env`] traits, so
//! the worker passes real iii/ops/alert implementations and tests pass fakes.
//! Runs as the `hex-watch` harness worker (`modules/watch.worker.rs`) on a
//! 5-minute cron; `hex watch tick` runs one pass by hand.

pub mod notify;
pub mod sources;
pub mod store;
pub mod tick;

use std::path::Path;

use serde::Deserialize;

pub use sources::Hit;
pub use store::{Status, Watch};
pub use tick::{Env, TickReport};

/// `$HEX_DIR/.hex/config/watch.toml`. Missing file = defaults; malformed = loud Err.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Default `--expires` for `hex watch add`.
    pub default_expires: String,
    /// Kill the action after this many seconds (exit 124, watch `failed`).
    pub action_timeout_secs: u64,
    pub sources: SourcesConfig,
    pub action: ActionConfig,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SourcesConfig {
    pub gmail: GmailConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GmailConfig {
    /// Shell command that prints one JSON object per hit
    /// (`{id, internal_ms, date, from, subject, account}`), run with
    /// `{query}` and `{account}` substituted as shell-quoted values.
    /// The default is the instance's `gmail-search --json` wrapper.
    pub command: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ActionConfig {
    /// Extra parent-env variables the action may see, on top of the built-in
    /// allowlist (`PATH`, `HOME`, `USER`, `LANG`, `TMPDIR`, `HEX_DIR`,
    /// `GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND`, `WATCH_*`, `MAIL_*`).
    pub env_passthrough: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_expires: "14d".to_string(),
            action_timeout_secs: 600,
            sources: SourcesConfig::default(),
            action: ActionConfig::default(),
        }
    }
}
impl Default for GmailConfig {
    fn default() -> Self {
        GmailConfig {
            command: "\"$HEX_DIR/.hex/bin/gmail-search\" --account {account} --json {query} 5"
                .to_string(),
        }
    }
}

pub fn config_path(hex_dir: &Path) -> std::path::PathBuf {
    hex_dir.join(".hex").join("config").join("watch.toml")
}

pub fn load_config(hex_dir: &Path) -> Result<Config, String> {
    let p = config_path(hex_dir);
    match std::fs::read_to_string(&p) {
        Ok(raw) => {
            toml::from_str(&raw).map_err(|e| format!("watch: malformed {}: {e}", p.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("watch: read {}: {e}", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_is_defaults_and_malformed_is_loud() {
        let t = tempfile::TempDir::new().unwrap();
        assert_eq!(load_config(t.path()).unwrap(), Config::default());
        let p = config_path(t.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "default_expires = 14\n").unwrap();
        assert!(load_config(t.path()).unwrap_err().contains("malformed"));
        std::fs::write(&p, "unknown_key = 1\n").unwrap();
        assert!(load_config(t.path()).unwrap_err().contains("malformed"));
        std::fs::write(&p, "action_timeout_secs = 5\n[sources.gmail]\ncommand = \"stub {query}\"\n[action]\nenv_passthrough = [\"FOO\"]\n").unwrap();
        let c = load_config(t.path()).unwrap();
        assert_eq!(c.action_timeout_secs, 5);
        assert_eq!(c.sources.gmail.command, "stub {query}");
        assert_eq!(c.action.env_passthrough, vec!["FOO"]);
        assert_eq!(c.default_expires, "14d");
    }
}
