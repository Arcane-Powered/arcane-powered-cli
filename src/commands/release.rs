use serde_json::json;

use crate::api::ApiClient;
use crate::config::{load_project_config, resolve_auth};
use crate::error::{CliError, Result};

pub struct ReleaseArgs {
    pub channel: String,
    pub version: String,
    pub title: Option<String>,
    pub platform: Option<String>,
    pub json: bool,
}

pub struct RollbackArgs {
    pub version: String,
    pub title: Option<String>,
    pub json: bool,
}

fn resolve_title_id(explicit: Option<String>, cmd: &str) -> Result<String> {
    if let Some(id) = explicit {
        return Ok(id);
    }
    let cwd = std::env::current_dir()?;
    if let Some((_, cfg)) = load_project_config(&cwd)? {
        if let Some(id) = cfg.title {
            return Ok(id);
        }
    }
    Err(CliError::msg(format!(
        "{cmd} needs --title <id> (or set \"title\" in arcane.json)"
    )))
}

pub async fn release(args: ReleaseArgs) -> Result<()> {
    let title_id = resolve_title_id(args.title, "release")?;
    let channel = args.channel.trim().to_lowercase();
    if channel.is_empty() {
        return Err(CliError::msg("channel cannot be empty (e.g. live, beta)"));
    }

    let auth = resolve_auth()?;
    let api = ApiClient::new(&auth)?;
    let builds = api.list_builds(&title_id).await?;

    let platform_filter = args.platform.as_deref().map(str::to_lowercase);
    let matches: Vec<_> = builds
        .into_iter()
        .filter(|b| b.version == args.version && b.status != "deprecated")
        .filter(|b| match &platform_filter {
            Some(p) => b.platform.to_lowercase() == *p,
            None => true,
        })
        .collect();

    if matches.is_empty() {
        let scope = match &platform_filter {
            Some(p) => format!("version {} on {}", args.version, p),
            None => format!("version {}", args.version),
        };
        return Err(CliError::msg(format!(
            "No active build found matching {scope} for title {title_id}"
        )));
    }

    let mut activations = Vec::with_capacity(matches.len());
    for b in &matches {
        let res = api.set_build_channel(&title_id, &channel, &b.id).await?;
        activations.push(res);
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "title_id": title_id,
                "channel": channel,
                "version": args.version,
                "activations": activations,
            }))?
        );
    } else {
        for act in &activations {
            println!(
                "Channel '{}' on {} -> version {} (build {})",
                act.channel, act.platform, args.version, act.build_id
            );
        }
    }

    Ok(())
}

pub async fn rollback(args: RollbackArgs) -> Result<()> {
    let title_id = resolve_title_id(args.title, "rollback")?;
    let auth = resolve_auth()?;
    let api = ApiClient::new(&auth)?;
    let builds = api.rollback_version(&title_id, &args.version).await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "title_id": title_id,
                "version": args.version,
                "builds": builds,
            }))?
        );
    } else if builds.is_empty() {
        println!(
            "Rolled back to version {} (no builds returned).",
            args.version
        );
    } else {
        println!("Rolled back live channel to version {}:", args.version);
        for b in &builds {
            println!("  {} -> build {}", b.platform, b.id);
        }
    }

    Ok(())
}
