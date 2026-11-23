use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CliError, Result};
use crate::fastcdc::ChunkingMode;

pub const PROJECT_CONFIG_FILENAME: &str = "arcane.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredAuthConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedAuth {
    pub api_key: String,
    pub portal_url: String,
    pub api_key_source: AuthSource,
    pub portal_url_source: AuthSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthSource {
    Env,
    ConfigFile,
}

impl ResolvedAuth {
    pub fn ci_base_url(&self) -> String {
        let base = self.portal_url.trim_end_matches('/');
        format!("{base}/api/ci")
    }

    pub fn masked_key(&self) -> String {
        mask_api_key(&self.api_key)
    }
}

pub fn mask_api_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 10 {
        return "****".to_string();
    }
    let prefix: String = chars[..6].iter().collect();
    let suffix: String = chars[chars.len() - 4..].iter().collect();
    format!("{prefix}...{suffix}")
}

/// Returns `~/.config/arcane/config.json` on Unix/macOS (respecting `XDG_CONFIG_HOME`)
/// or `%APPDATA%\arcane\config.json` on Windows.
pub fn global_config_path() -> PathBuf {
    #[cfg(unix)]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.trim().is_empty() {
                return PathBuf::from(xdg).join("arcane").join("config.json");
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".config").join("arcane").join("config.json");
        }
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("arcane")
        .join("config.json")
}

pub fn load_stored_auth(path: &Path) -> Result<StoredAuthConfig> {
    if !path.exists() {
        return Ok(StoredAuthConfig::default());
    }
    let raw = fs::read_to_string(path)?;
    let parsed: StoredAuthConfig = serde_json::from_str(&raw)?;
    Ok(parsed)
}

pub fn save_stored_auth(path: &Path, cfg: &StoredAuthConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(cfg)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, format!("{json}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn delete_stored_auth(path: &Path) -> Result<bool> {
    if path.exists() {
        fs::remove_file(path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Resolve auth credentials: environment variables always override `~/.config/arcane/config.json`.
pub fn resolve_auth() -> Result<ResolvedAuth> {
    let config_path = global_config_path();
    let stored = load_stored_auth(&config_path).unwrap_or_default();

    let env_key = std::env::var("ARCANE_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let env_url = std::env::var("ARCANE_PORTAL_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let (api_key, api_key_source) = match env_key {
        Some(k) => (k, AuthSource::Env),
        None => match stored.api_key.filter(|s| !s.trim().is_empty()) {
            Some(k) => (k, AuthSource::ConfigFile),
            None => {
                return Err(CliError::msg(
                    "Not authenticated. Run `arcane login` or set ARCANE_API_KEY.",
                ));
            }
        },
    };

    let (portal_url, portal_url_source) = match env_url {
        Some(u) => (u, AuthSource::Env),
        None => match stored.portal_url.filter(|s| !s.trim().is_empty()) {
            Some(u) => (u, AuthSource::ConfigFile),
            None => {
                return Err(CliError::msg(
                    "Portal URL is not configured. Run `arcane login` or set ARCANE_PORTAL_URL.",
                ));
            }
        },
    };

    Ok(ResolvedAuth {
        api_key,
        portal_url: portal_url.trim_end_matches('/').to_string(),
        api_key_source,
        portal_url_source,
    })
}

/// Project-level configuration (`arcane.json`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunking: Option<ChunkingMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compress: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub platforms: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web: Option<ProjectWebConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectWebConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_fullscreen: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsafe_eval: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_bridge: Option<bool>,
}

/// Walk up from `start_dir` looking for `arcane.json`.
pub fn find_project_config(start_dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(start_dir);
    while let Some(dir) = cur {
        let candidate = dir.join(PROJECT_CONFIG_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    None
}

pub fn load_project_config(start_dir: &Path) -> Result<Option<(PathBuf, ProjectConfig)>> {
    let Some(path) = find_project_config(start_dir) else {
        return Ok(None);
    };
    let text = fs::read_to_string(&path)?;
    let cfg: ProjectConfig = serde_json::from_str(&text)
        .map_err(|e| CliError::msg(format!("Invalid {}: {e}", path.display())))?;
    Ok(Some((path, cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn stored_auth_roundtrip_sets_strict_permissions() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("arcane").join("config.json");
        let cfg = StoredAuthConfig {
            api_key: Some("arc_test_1234567890".to_string()),
            portal_url: Some("https://portal.example.com".to_string()),
        };
        save_stored_auth(&path, &cfg).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let loaded = load_stored_auth(&path).unwrap();
        assert_eq!(loaded.api_key.as_deref(), Some("arc_test_1234567890"));
        assert_eq!(
            loaded.portal_url.as_deref(),
            Some("https://portal.example.com")
        );
        assert!(delete_stored_auth(&path).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn masks_api_key() {
        assert_eq!(mask_api_key("arc_live_1234567890abcd"), "arc_li...abcd");
        assert_eq!(mask_api_key("short"), "****");
    }
}
