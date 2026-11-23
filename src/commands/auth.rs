use std::io::{self, Write};

use serde_json::json;

use crate::api::ApiClient;
use crate::config::{
    delete_stored_auth, global_config_path, mask_api_key, resolve_auth, save_stored_auth,
    AuthSource, ResolvedAuth, StoredAuthConfig, DEFAULT_PORTAL_URL,
};
use crate::error::{CliError, Result};

pub struct LoginArgs {
    pub api_key: Option<String>,
    pub portal_url: Option<String>,
    pub no_verify: bool,
    pub json: bool,
}

pub async fn login(args: LoginArgs) -> Result<()> {
    let portal_url = match args.portal_url {
        Some(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => prompt_with_default("Portal URL", DEFAULT_PORTAL_URL)?,
    };

    let api_key = match args.api_key {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => prompt_required("Studio API key (Settings -> API keys)")?,
    };

    let mut titles_count: Option<usize> = None;
    if !args.no_verify {
        let test_auth = ResolvedAuth {
            api_key: api_key.clone(),
            portal_url: portal_url.clone(),
            api_key_source: AuthSource::ConfigFile,
            portal_url_source: AuthSource::ConfigFile,
        };
        let api = ApiClient::new(&test_auth)?;
        let titles = api.list_titles().await.map_err(|e| {
            CliError::msg(format!(
                "Authentication check failed against {portal_url}: {e}"
            ))
        })?;
        titles_count = Some(titles.len());
    }

    let path = global_config_path();
    save_stored_auth(
        &path,
        &StoredAuthConfig {
            api_key: Some(api_key.clone()),
            portal_url: Some(portal_url.clone()),
        },
    )?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "portal_url": portal_url,
                "api_key": mask_api_key(&api_key),
                "config_path": path,
                "titles_count": titles_count,
            }))?
        );
    } else {
        println!("Logged in to {portal_url}");
        println!("  API key: {}", mask_api_key(&api_key));
        if let Some(n) = titles_count {
            println!("  Access verified ({n} title(s) visible)");
        }
        println!("  Saved to {}", path.display());
    }

    Ok(())
}

pub fn logout(json_mode: bool) -> Result<()> {
    let path = global_config_path();
    let removed = delete_stored_auth(&path)?;
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "removed": removed,
                "config_path": path,
            }))?
        );
    } else if removed {
        println!("Logged out (removed {}).", path.display());
    } else {
        println!("Not logged in (no credentials file at {}).", path.display());
    }
    Ok(())
}

pub async fn whoami(json_mode: bool) -> Result<()> {
    let auth = resolve_auth()?;
    let api = ApiClient::new(&auth)?;
    let titles = api.list_titles().await?;

    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "portal_url": auth.portal_url,
                "portal_url_source": auth.portal_url_source,
                "api_key": auth.masked_key(),
                "api_key_source": auth.api_key_source,
                "titles_count": titles.len(),
                "config_path": global_config_path(),
            }))?
        );
    } else {
        println!(
            "Portal:  {} ({:?})",
            auth.portal_url, auth.portal_url_source
        );
        println!("API key: {} ({:?})", auth.masked_key(), auth.api_key_source);
        println!("Titles:  {} accessible", titles.len());
    }
    Ok(())
}

fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    eprint!("{label} [{default}]: ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        Ok(default.trim_end_matches('/').to_string())
    } else {
        Ok(trimmed.trim_end_matches('/').to_string())
    }
}

fn prompt_required(label: &str) -> Result<String> {
    eprint!("{label}: ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(CliError::msg(format!("{label} cannot be empty")));
    }
    Ok(trimmed.to_string())
}
