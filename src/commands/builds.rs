use std::collections::HashMap;

use serde_json::json;

use crate::api::{ApiClient, BuildStorageEntry};
use crate::config::{load_project_config, resolve_auth};
use crate::error::{CliError, Result};
use crate::progress::format_bytes;

pub struct BuildsListArgs {
    pub title: Option<String>,
    pub platform: Option<String>,
    pub json: bool,
}

pub struct BuildActionArgs {
    pub title: Option<String>,
    pub build_id: String,
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

pub async fn list(args: BuildsListArgs) -> Result<()> {
    let title_id = resolve_title_id(args.title, "builds list")?;
    let auth = resolve_auth()?;
    let api = ApiClient::new(&auth)?;

    let (builds_res, storage_res, channels_res) = tokio::join!(
        api.list_builds(&title_id),
        api.get_build_storage(&title_id),
        api.list_build_channels(&title_id),
    );

    let mut builds = builds_res?;
    let storage = storage_res.ok();
    let channels = channels_res.unwrap_or_default();

    if let Some(ref plat) = args.platform {
        let p = plat.to_lowercase();
        builds.retain(|b| b.platform.to_lowercase() == p);
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "title_id": title_id,
                "builds": builds,
                "channels": channels,
                "storage": storage,
            }))?
        );
        return Ok(());
    }

    if builds.is_empty() {
        println!("No builds found for title {title_id}.");
        return Ok(());
    }

    let mut channels_by_build: HashMap<&str, Vec<&str>> = HashMap::new();
    for ch in &channels {
        channels_by_build
            .entry(ch.build_id.as_str())
            .or_default()
            .push(ch.channel.as_str());
    }

    let storage_by_build: HashMap<&str, &BuildStorageEntry> = storage
        .as_ref()
        .map(|s| s.builds.iter().map(|e| (e.build_id.as_str(), e)).collect())
        .unwrap_or_default();

    println!(
        "{:<14} {:<10} {:<12} {:<14} {:<6} {:<10} {:<12} BUILD ID",
        "VERSION", "PLATFORM", "STATUS", "CHANNELS", "PIN", "SIZE", "RECLAIMABLE"
    );
    for b in &builds {
        let ch_list = channels_by_build
            .get(b.id.as_str())
            .map(|v| v.join(","))
            .unwrap_or_else(|| "-".to_string());
        let pin_str = if b.pinned { "yes" } else { "-" };
        let status = b.deploy_status.as_deref().unwrap_or(&b.status);
        let reclaimable = storage_by_build
            .get(b.id.as_str())
            .map(|s| format_bytes(s.reclaimable_bytes))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<14} {:<10} {:<12} {:<14} {:<6} {:<10} {:<12} {}",
            b.version,
            b.platform,
            status,
            ch_list,
            pin_str,
            format_bytes(b.total_bytes),
            reclaimable,
            b.id
        );
    }

    if let Some(ref s) = storage {
        println!(
            "\nDeduplicated CAS storage: {} stored ({} raw, {} unique blobs)",
            format_bytes(s.deduped_stored_bytes),
            format_bytes(s.deduped_raw_bytes),
            s.unique_blobs
        );
    }

    Ok(())
}

pub async fn set_pinned(args: BuildActionArgs, pinned: bool) -> Result<()> {
    let cmd = if pinned { "builds pin" } else { "builds unpin" };
    let title_id = resolve_title_id(args.title, cmd)?;
    let auth = resolve_auth()?;
    let api = ApiClient::new(&auth)?;
    let updated = api
        .set_build_pinned(&title_id, &args.build_id, pinned)
        .await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&updated)?);
    } else {
        let verb = if pinned { "Pinned" } else { "Unpinned" };
        println!(
            "{verb} build {} ({} on {})",
            updated.id, updated.version, updated.platform
        );
    }
    Ok(())
}

pub async fn delete(args: BuildActionArgs) -> Result<()> {
    let title_id = resolve_title_id(args.title, "builds delete")?;
    let auth = resolve_auth()?;
    let api = ApiClient::new(&auth)?;
    let result = api.delete_build(&title_id, &args.build_id).await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "title_id": title_id,
                "build_id": args.build_id,
                "result": result,
            }))?
        );
    } else {
        println!("Deleted build {}", args.build_id);
    }
    Ok(())
}
