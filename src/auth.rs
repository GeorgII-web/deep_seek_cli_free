use crate::config::Paths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Login data of the DeepSeek web account, stored in `<home>/auth.json` (mode 0600).
#[derive(Clone, Serialize, Deserialize)]
pub struct Auth {
    /// `localStorage.userToken.value` of chat.deepseek.com
    pub token: String,
    #[serde(default)]
    pub cookie: String,
    /// URL of the proof-of-work wasm module, when the login source knew it
    #[serde(default)]
    pub wasm_url: Option<String>,
}

/// FreeDeepseekAPI's `deepseek-auth.json`.
#[derive(Deserialize)]
struct Imported {
    token: String,
    #[serde(default)]
    cookie: String,
    #[serde(default, rename = "wasmUrl")]
    wasm_url: Option<String>,
}

impl Auth {
    pub fn load(paths: &Paths) -> Result<Option<Self>> {
        let file = paths.auth();
        if !file.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        let auth = serde_json::from_str(&text)
            .map_err(|_| anyhow::anyhow!("{} is not a valid auth file", file.display()))?;
        Ok(Some(auth))
    }

    pub fn require(paths: &Paths) -> Result<Self> {
        Self::load(paths)?.context("not logged in; run `dsc login`")
    }

    pub fn import(file: &Path) -> Result<Self> {
        let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        // The parse error would quote the secret, so it is not passed on.
        let imported: Imported = serde_json::from_str(&text)
            .map_err(|_| anyhow::anyhow!("{} has no `token` field", file.display()))?;
        Self::new(
            imported.token,
            imported.cookie,
            imported.wasm_url.filter(|url| !url.is_empty()),
        )
    }

    pub fn new(token: String, cookie: String, wasm_url: Option<String>) -> Result<Self> {
        let token = token.trim().to_string();
        if token.is_empty() {
            bail!("the token is empty");
        }
        Ok(Self {
            token,
            cookie: cookie.trim().to_string(),
            wasm_url,
        })
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let file = paths.auth();
        let _ = fs::remove_file(&file);
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .with_context(|| format!("write {}", file.display()))?;
        out.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        Ok(())
    }

    /// Returns false when there was nothing to remove.
    pub fn remove(paths: &Paths) -> Result<bool> {
        let file = paths.auth();
        if !file.exists() {
            return Ok(false);
        }
        fs::remove_file(&file).with_context(|| format!("remove {}", file.display()))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_paths(name: &str) -> Paths {
        let home = std::env::temp_dir().join(format!("dsc-test-{name}-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        Paths { home }
    }

    #[test]
    fn import_save_load_remove() {
        let paths = temp_paths("auth");
        let source = paths.home.join("src.json");
        fs::write(
            &source,
            r#"{"token":" abc ","cookie":"a=b","wasmUrl":"","baseUrl":"x"}"#,
        )
        .unwrap();
        let auth = Auth::import(&source).unwrap();
        assert_eq!(auth.token, "abc");
        assert_eq!(auth.wasm_url, None);
        auth.save(&paths).unwrap();
        let mode = fs::metadata(paths.auth()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(Auth::load(&paths).unwrap().unwrap().cookie, "a=b");
        assert!(Auth::remove(&paths).unwrap());
        assert!(!Auth::remove(&paths).unwrap());
        assert!(Auth::load(&paths).unwrap().is_none());
        fs::remove_dir_all(&paths.home).unwrap();
    }

    #[test]
    fn empty_token_is_refused() {
        assert!(Auth::new("  ".into(), String::new(), None).is_err());
    }
}
