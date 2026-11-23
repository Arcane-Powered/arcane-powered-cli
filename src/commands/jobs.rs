use serde_json::json;

use crate::api::ApiClient;
use crate::config::{load_project_config, resolve_auth};
use crate::error::{CliError, Result};

pub struct JobsArgs {
    pub title: Option<String>,
    pub json: bool,
}

pub async fn run(args: JobsArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project = load_project_config(&cwd)?.map(|(_, cfg)| cfg);

    let title_id = args
        .title
        .or_else(|| project.and_then(|p| p.title))
        .ok_or_else(|| {
            CliError::msg("jobs needs --title <id> (or set \"title\" in arcane.json)")
        })?;

    let auth = resolve_auth()?;
    let api = ApiClient::new(&auth)?;
    let jobs = api.list_publish_jobs(&title_id).await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "title_id": title_id,
                "jobs": jobs,
            }))?
        );
        return Ok(());
    }

    if jobs.is_empty() {
        println!("No recent publish jobs for title {title_id}.");
        return Ok(());
    }

    println!(
        "{:<10} {:<10} {:<14} {:<10} {:<24} DETAILS",
        "STATUS", "PLATFORM", "VERSION", "KIND", "UPDATED"
    );
    for job in &jobs {
        let platform = job.platform.as_deref().unwrap_or("-");
        let detail = if let Some(ref err) = job.error {
            format!("error: {err}")
        } else if let Some(ref bid) = job.build_id {
            format!("build {bid}")
        } else {
            job.phase.clone()
        };
        println!(
            "{:<10} {:<10} {:<14} {:<10} {:<24} {}",
            job.status, platform, job.version, job.kind, job.updated_at, detail
        );
    }

    Ok(())
}
