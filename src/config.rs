use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Folder of the app under the user's config dir.
const APP_DIR: &str = "deep_seek_cli_free";
/// Its name in the first versions; moved to `APP_DIR` when found.
const OLD_APP_DIR: &str = "dsc";

/// File locations under the home dir: `$DSC_HOME`, else `~/.config/deep_seek_cli_free`.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let home = match std::env::var_os("DSC_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => {
                let base = directories::BaseDirs::new()
                    .context("no home directory")?
                    .config_dir()
                    .to_path_buf();
                let home = base.join(APP_DIR);
                if let Some(note) = migrate(&base.join(OLD_APP_DIR), &home) {
                    eprintln!("dsc: {note}");
                }
                home
            }
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

    /// Earlier chat inputs (JSON list)
    pub fn history(&self) -> PathBuf {
        self.home.join("history.json")
    }

    /// The conversations of this app that can be continued (JSON list)
    pub fn sessions(&self) -> PathBuf {
        self.home.join("sessions.json")
    }

    /// The saved screen output of one conversation, by its session id.
    pub fn transcript(&self, session_id: &str) -> PathBuf {
        // The id comes from the server: only harmless characters reach the file name.
        let name: String = session_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        self.home.join("sessions").join(format!("{name}.log"))
    }

    pub fn cache(&self) -> PathBuf {
        self.home.join("cache")
    }
}

/// Moves the folder of the first versions to the current place, once: only when
/// the new one does not exist yet. Returns a line for the user when something happened.
fn migrate(old: &std::path::Path, new: &std::path::Path) -> Option<String> {
    if new.exists() || !old.is_dir() {
        return None;
    }
    Some(match fs::rename(old, new) {
        Ok(()) => format!("settings moved from {} to {}", old.display(), new.display()),
        Err(err) => format!(
            "could not move {} to {}: {err}; starting with new settings",
            old.display(),
            new.display()
        ),
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub deepseek: Deepseek,
    pub throttle: Throttle,
    pub agent: Agent,
    pub ui: Ui,
    pub mock: MockSpeed,
    pub policy: Policy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Deepseek {
    /// What answers: the real account or the built-in mock; `/model` switches and saves it
    pub api: Api,
    /// `default` (fast) or `expert`
    pub model_type: String,
    pub thinking: bool,
    pub search: bool,
    pub locale: String,
    pub timeout_secs: u64,
}

/// Who answers the prompts.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Api {
    /// chat.deepseek.com with the user's token
    #[default]
    Deepseek,
    /// The fake model inside the app (`src/mock.rs`): no network, no account
    Mock,
}

/// Request rate limits towards DeepSeek, see `deepseek::throttle`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Throttle {
    /// Minimum gap between two requests, also across separate `dsc` runs
    pub min_interval_ms: u64,
    /// Random extra gap, 0..=this
    pub jitter_ms: u64,
    /// Requests allowed in any 60 seconds; 0 = no cap
    pub max_per_minute: u32,
    /// Requests allowed in any 60 minutes; 0 = no cap
    pub max_per_hour: u32,
    /// Retries after HTTP 429
    pub retries_on_429: u32,
    /// Wait before the first such retry when the server sent no `Retry-After`;
    /// doubled for each further one, and kept as a pause for later runs
    pub wait_on_429_secs: u64,
}

impl Throttle {
    /// Defaults of earlier versions. They proved too fast: the free account was
    /// blocked at about 25 requests an hour with 7 to 35 s between them (2026-11-11).
    fn earlier_defaults() -> [Self; 2] {
        [
            Self {
                min_interval_ms: 3000,
                jitter_ms: 2000,
                max_per_hour: 120,
                retries_on_429: 2,
                wait_on_429_secs: 60,
                ..Self::default()
            },
            Self {
                min_interval_ms: 6000,
                jitter_ms: 4000,
                max_per_minute: 5,
                max_per_hour: 60,
                retries_on_429: 2,
                wait_on_429_secs: 120,
            },
        ]
    }
}

impl Default for Throttle {
    fn default() -> Self {
        Self {
            min_interval_ms: 30_000,
            jitter_ms: 15_000,
            max_per_minute: 2,
            max_per_hour: 12,
            retries_on_429: 2,
            wait_on_429_secs: 300,
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

/// How fast the built-in mock model answers: slow enough to watch the UI work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MockSpeed {
    /// Pause before the answer starts (the "waiting" phase)
    pub wait_ms: u64,
    /// Pause between two words of an answer; the `slow` task takes four times as long
    pub word_ms: u64,
}

impl Default for MockSpeed {
    fn default() -> Self {
        Self {
            wait_ms: 2000,
            word_ms: 150,
        }
    }
}

/// Look and feel of the chat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// The mouse wheel scrolls the output. The terminal then gives mouse clicks
    /// to the app: hold Shift to select text. Off: scroll with PgUp / PgDn only.
    pub mouse_scroll: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self { mouse_scroll: true }
    }
}

/// How actions outside the allow list are handled.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The user confirms each one
    #[default]
    Ask,
    /// They run without a question; the deny list still applies
    AllowAll,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// The approval mode the chat starts in; `dsc` saves the last one chosen
    pub mode: Mode,
    /// Programs that run without asking
    pub allow: Vec<String>,
    /// Patterns that never run, even with `--yes`
    pub deny: Vec<String>,
}

impl Default for Deepseek {
    fn default() -> Self {
        Self {
            api: Api::Deepseek,
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
            mode: Mode::Ask,
            // No program that prints file content: what it prints goes to DeepSeek.
            allow: list(&[
                "ls",
                "pwd",
                "df",
                "du",
                "uname",
                "whoami",
                "date",
                "which",
                "stat",
                "wc",
                "file",
                "git status",
            ]),
            deny: list(&[
                "rm -rf /",
                "rm -rf ~",
                "mkfs",
                "dd of=/dev/",
                "shutdown",
                "reboot",
                ":(){",
                ".ssh/id_",
                ".aws/credentials",
                "deep_seek_cli_free/auth.json",
                "dsc/auth.json",
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
            settings.save(paths)?;
            return Ok(settings);
        }
        let text = fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        let mut settings: Self =
            toml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
        // Both run: the first `|` is not short-circuit on purpose.
        if settings.guard_auth_file() | settings.slow_down() {
            settings.save(paths)?;
        }
        Ok(settings)
    }

    /// Writes the whole `config.toml` (comments in the file are not kept).
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let file = paths.config();
        fs::write(&file, toml::to_string_pretty(self)?)
            .with_context(|| format!("write {}", file.display()))
    }

    /// A config still holding the throttle defaults of an earlier version gets the
    /// current, slower ones; values the user changed are kept. Returns true when something changed.
    fn slow_down(&mut self) -> bool {
        if Throttle::earlier_defaults().contains(&self.throttle) {
            self.throttle = Throttle::default();
            return true;
        }
        false
    }

    /// A deny list written before the folder was renamed protects only the old
    /// token path; the new one is added. Returns true when the list changed.
    fn guard_auth_file(&mut self) -> bool {
        let (old, new) = (
            format!("{OLD_APP_DIR}/auth.json"),
            format!("{APP_DIR}/auth.json"),
        );
        let deny = &mut self.policy.deny;
        if deny.contains(&old) && !deny.contains(&new) {
            deny.push(new);
            return true;
        }
        false
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

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsc-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn old_folder_moves_once_and_never_over_a_new_one() {
        let base = temp_dir("migrate");
        let (old, new) = (base.join("dsc"), base.join("deep_seek_cli_free"));
        assert_eq!(migrate(&old, &new), None);
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("auth.json"), "x").unwrap();
        assert!(migrate(&old, &new).unwrap().starts_with("settings moved"));
        assert_eq!(fs::read_to_string(new.join("auth.json")).unwrap(), "x");
        assert!(!old.exists());
        // Both exist: nothing is touched.
        fs::create_dir_all(&old).unwrap();
        assert_eq!(migrate(&old, &new), None);
        assert!(old.exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn mode_is_saved_and_read_back() {
        let paths = Paths {
            home: temp_dir("mode"),
        };
        let mut settings = Settings::load_or_create(&paths).unwrap();
        assert_eq!(settings.policy.mode, Mode::Ask);
        settings.policy.mode = Mode::AllowAll;
        settings.save(&paths).unwrap();
        assert!(
            fs::read_to_string(paths.config())
                .unwrap()
                .contains("mode = \"allow_all\"")
        );
        assert_eq!(
            Settings::load_or_create(&paths).unwrap().policy.mode,
            Mode::AllowAll
        );
        fs::remove_dir_all(paths.home).unwrap();
    }

    #[test]
    fn old_deny_list_gets_the_new_token_path() {
        let paths = Paths {
            home: temp_dir("deny"),
        };
        fs::write(paths.config(), "[policy]\ndeny = [\"dsc/auth.json\"]\n").unwrap();
        let settings = Settings::load_or_create(&paths).unwrap();
        assert_eq!(
            settings.policy.deny,
            ["dsc/auth.json", "deep_seek_cli_free/auth.json"]
        );
        assert!(
            fs::read_to_string(paths.config())
                .unwrap()
                .contains("deep_seek_cli_free/auth.json")
        );
        fs::remove_dir_all(paths.home).unwrap();
    }

    #[test]
    fn first_throttle_defaults_are_replaced_and_own_values_kept() {
        let paths = Paths {
            home: temp_dir("slow"),
        };
        let old = "[throttle]\nmin_interval_ms = 3000\njitter_ms = 2000\nmax_per_hour = 120\nretries_on_429 = 2\nwait_on_429_secs = 60\n";
        fs::write(paths.config(), old).unwrap();
        assert_eq!(
            Settings::load_or_create(&paths).unwrap().throttle,
            Throttle::default()
        );
        assert!(
            fs::read_to_string(paths.config())
                .unwrap()
                .contains("min_interval_ms = 30000")
        );

        let second = "[throttle]\nmin_interval_ms = 6000\njitter_ms = 4000\nmax_per_minute = 5\nmax_per_hour = 60\nretries_on_429 = 2\nwait_on_429_secs = 120\n";
        fs::write(paths.config(), second).unwrap();
        let upgraded = Settings::load_or_create(&paths).unwrap().throttle;
        assert_eq!(
            (
                upgraded.min_interval_ms,
                upgraded.jitter_ms,
                upgraded.max_per_minute,
                upgraded.max_per_hour
            ),
            (30_000, 15_000, 2, 12)
        );

        fs::write(paths.config(), old.replace("3000", "1000")).unwrap();
        let own = Settings::load_or_create(&paths).unwrap().throttle;
        assert_eq!((own.min_interval_ms, own.max_per_hour), (1000, 120));
        fs::remove_dir_all(paths.home).unwrap();
    }
}
