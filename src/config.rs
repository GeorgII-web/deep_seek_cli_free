use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// File locations under the home dir: `$DSC_HOME`, else `~/.config/dsc`.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let home = match std::env::var_os("DSC_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => directories::BaseDirs::new()
                .context("no home directory")?
                .config_dir()
                .join("dsc"),
        };
        fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;
        Ok(Self { home })
    }

    pub fn config(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    pub fn auth(&self) -> PathBuf {
        self.home.join("auth.json")
    }

    pub fn cache(&self) -> PathBuf {
        self.home.join("cache")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub deepseek: Deepseek,
    pub throttle: Throttle,
    pub agent: Agent,
    pub policy: Policy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Deepseek {
    /// `default` (fast) or `expert`
    pub model_type: String,
    pub thinking: bool,
    pub search: bool,
    pub locale: String,
    pub timeout_secs: u64,
}

/// Request rate limits towards DeepSeek, see `deepseek::throttle`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Throttle {
    /// Minimum gap between two requests, also across separate `dsc` runs
    pub min_interval_ms: u64,
    /// Random extra gap, 0..=this
    pub jitter_ms: u64,
    /// Requests allowed in any 60 minutes; 0 = no cap
    pub max_per_hour: u32,
    /// Retries after HTTP 429
    pub retries_on_429: u32,
    /// Wait before such a retry when the server sent no `Retry-After`
    pub wait_on_429_secs: u64,
}

impl Default for Throttle {
    fn default() -> Self {
        Self {
            min_interval_ms: 3000,
            jitter_ms: 2000,
            max_per_hour: 120,
            retries_on_429: 2,
            wait_on_429_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Agent {
    pub max_steps: u32,
    /// Cap of stdout and of stderr sent back to the model (head + tail kept)
    pub max_output_bytes: usize,
    pub command_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// Programs that run without asking
    pub allow: Vec<String>,
    /// Patterns that never run, even with `--yes`
    pub deny: Vec<String>,
}

impl Default for Deepseek {
    fn default() -> Self {
        Self {
            model_type: "default".into(),
            thinking: false,
            search: false,
            locale: "en".into(),
            timeout_secs: 180,
        }
    }
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            max_steps: 25,
            max_output_bytes: 8000,
            command_timeout_secs: 120,
        }
    }
}

impl Default for Policy {
    fn default() -> Self {
        let list = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
        Self {
            allow: list(&[
                "ls", "pwd", "cat", "head", "tail", "wc", "rg", "grep", "find", "df", "du",
                "uname", "whoami", "date", "which", "file", "stat",
            ]),
            deny: list(&[
                "rm -rf /",
                "mkfs",
                "dd of=/dev/",
                "shutdown",
                "reboot",
                ":(){",
            ]),
        }
    }
}

impl Settings {
    /// Reads `config.toml`; a missing file is written with the defaults.
    pub fn load_or_create(paths: &Paths) -> Result<Self> {
        let file = paths.config();
        if !file.exists() {
            let settings = Self::default();
            fs::write(&file, toml::to_string_pretty(&settings)?)
                .with_context(|| format!("write {}", file.display()))?;
            return Ok(settings);
        }
        let text = fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        toml::from_str(&text).with_context(|| format!("parse {}", file.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_survive_a_toml_round_trip() {
        let text = toml::to_string_pretty(&Settings::default()).unwrap();
        assert_eq!(
            toml::from_str::<Settings>(&text).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn missing_keys_take_defaults_and_unknown_keys_fail() {
        let settings: Settings = toml::from_str("[agent]\nmax_steps = 3\n").unwrap();
        assert_eq!(settings.agent.max_steps, 3);
        assert_eq!(settings.deepseek.model_type, "default");
        assert!(toml::from_str::<Settings>("[agent]\nnope = 1\n").is_err());
    }
}
