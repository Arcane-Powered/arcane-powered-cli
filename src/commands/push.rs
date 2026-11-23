use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Map, Value};

use crate::api::ApiClient;
use crate::config::{load_project_config, resolve_auth, ProjectWebConfig};
use crate::error::{CliError, Result};
use crate::fastcdc::{ChunkingMode, DEFAULT_FIXED_CHUNK_SIZE};
use crate::progress::{format_bytes, is_interactive, make_spinner};
use crate::upload::{upload_all_cas, upload_all_multipart};
use crate::walker::list_files;

const VALID_PLATFORMS: &[&str] = &["windows", "macos", "linux", "web", "android"];
const DEFAULT_CONCURRENCY: usize = 6;

pub struct PushArgs {
    pub folder: Option<PathBuf>,
    pub title: Option<String>,
    pub platform: String,
    pub version: Option<String>,
    pub draft: bool,
    pub scheduled_at: Option<String>,
    pub create_notes: bool,
    pub notes_file: Option<PathBuf>,
    pub chunking: Option<ChunkingMode>,
    pub no_compress: bool,
    pub concurrency: Option<usize>,
    pub no_cache: bool,
    pub ignore: Vec<String>,
    pub entry: Option<String>,
    pub viewport: Option<String>,
    pub no_fullscreen: bool,
    pub origins: Option<String>,
    pub unsafe_eval: bool,
    pub inputs: Option<String>,
    pub sdk_bridge: bool,
    pub json: bool,
}

pub async fn run(args: PushArgs) -> Result<()> {
    let platform = args.platform.to_lowercase();
    if !VALID_PLATFORMS.contains(&platform.as_str()) {
        return Err(CliError::msg(
            "--platform must be windows, macos, linux, web or android",
        ));
    }

    let cwd = std::env::current_dir()?;
    let project_pair = load_project_config(&cwd)?;
    let (project_dir, project_cfg) = match project_pair {
        Some((p, cfg)) => (p.parent().map(Path::to_path_buf), Some(cfg)),
        None => (None, None),
    };

    let title_id = args
        .title
        .clone()
        .or_else(|| project_cfg.as_ref().and_then(|c| c.title.clone()))
        .ok_or_else(|| {
            CliError::msg("push needs --title <id> (or set \"title\" in arcane.json)")
        })?;

    let version = args
        .version
        .clone()
        .or_else(detect_git_tag)
        .ok_or_else(|| {
            CliError::msg(
                "push needs --version <version> (or run on a git tag / with GITHUB_REF_NAME set)",
            )
        })?;

    let folder_path = match args.folder.clone() {
        Some(f) => f,
        None => {
            let from_cfg = project_cfg
                .as_ref()
                .and_then(|c| c.platforms.get(&platform))
                .ok_or_else(|| {
                    CliError::msg(format!(
                        "push needs <folder> (or set \"platforms.{platform}\" in arcane.json)"
                    ))
                })?;
            match &project_dir {
                Some(dir) => dir.join(from_cfg),
                None => PathBuf::from(from_cfg),
            }
        }
    };

    let root = fs::canonicalize(&folder_path).map_err(|_| {
        CliError::msg(format!(
            "{} is not an existing folder",
            folder_path.display()
        ))
    })?;
    if !root.is_dir() {
        return Err(CliError::msg(format!("{} is not a folder", root.display())));
    }

    let mut ignores = project_cfg
        .as_ref()
        .map(|c| c.ignore.clone())
        .unwrap_or_default();
    ignores.extend(args.ignore.clone());

    let files = list_files(&root, &ignores)?;
    if files.is_empty() {
        return Err(CliError::msg(format!("{} holds no file", root.display())));
    }
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();

    let chunking = args
        .chunking
        .or_else(|| project_cfg.as_ref().and_then(|c| c.chunking))
        .unwrap_or(ChunkingMode::Cdc);

    let allow_zstd = if args.no_compress {
        false
    } else {
        project_cfg
            .as_ref()
            .and_then(|c| c.compress)
            .unwrap_or(true)
    };

    let concurrency = args
        .concurrency
        .or_else(|| project_cfg.as_ref().and_then(|c| c.concurrency))
        .unwrap_or(DEFAULT_CONCURRENCY)
        .max(1);

    let interactive = is_interactive(args.json);

    if !args.json {
        eprintln!(
            "Preparing {} files ({}) from {}",
            files.len(),
            format_bytes(total_bytes),
            root.display()
        );
    }

    let auth = resolve_auth()?;
    let api = ApiClient::new(&auth)?;

    if platform == "web" {
        let limits = api.get_web_limits(&title_id).await?;
        if files.len() > limits.max_files {
            return Err(CliError::msg(format!(
                "A web build holds at most {} files, this one has {}",
                limits.max_files,
                files.len()
            )));
        }
        if total_bytes > limits.max_total_bytes {
            return Err(CliError::msg(format!(
                "A web build weighs at most {}, this one weighs {}",
                format_bytes(limits.max_total_bytes),
                format_bytes(total_bytes)
            )));
        }
        if let Some(largest) = files.iter().max_by_key(|f| f.size) {
            if largest.size > limits.max_file_bytes {
                return Err(CliError::msg(format!(
                    "{} is over the {} per-file limit",
                    largest.relative,
                    format_bytes(limits.max_file_bytes)
                )));
            }
        }
    }

    let session = api.create_upload_session(&title_id).await?;
    let session_route = format!(
        "/titles/{title_id}/builds/upload-sessions/{}",
        session.session_id
    );
    let is_native = matches!(platform.as_str(), "windows" | "macos" | "linux");

    let run_result: Result<Value> = async {
        let upload_summary = if is_native {
            let chunk_size = session.chunk_size.unwrap_or(DEFAULT_FIXED_CHUNK_SIZE);
            let stats = upload_all_cas(
                &api,
                &session_route,
                &files,
                chunk_size,
                allow_zstd,
                chunking,
                concurrency,
                !args.no_cache,
                interactive,
                args.json,
            )
            .await?;
            serde_json::to_value(stats)?
        } else {
            let current = api.get_upload_session(&session_route).await?;
            let already_there: HashMap<String, u64> = current
                .files
                .into_iter()
                .map(|f| (f.path, f.size))
                .collect();
            let stats = upload_all_multipart(
                &api,
                &title_id,
                &session.session_id,
                &files,
                &already_there,
                concurrency.min(4),
                interactive,
            )
            .await?;
            serde_json::to_value(stats)?
        };

        let changelog = match &args.notes_file {
            Some(path) => Some(fs::read_to_string(path)?),
            None => None,
        };
        let create_game_update = args.create_notes || changelog.is_some();

        let mut payload = Map::new();
        payload.insert("version".into(), json!(version));
        if args.draft {
            payload.insert("draft".into(), json!(true));
        }
        if let Some(ref sched) = args.scheduled_at {
            payload.insert("scheduled_at".into(), json!(sched));
        }

        let endpoint = match platform.as_str() {
            "web" => {
                let web_cfg = project_cfg.as_ref().and_then(|c| c.web.as_ref());
                merge_web_settings(&mut payload, &args, web_cfg)?;
                "publish-web"
            }
            "android" => "publish-android",
            _ => {
                payload.insert("platform".into(), json!(platform));
                if create_game_update {
                    payload.insert("create_game_update".into(), json!(true));
                }
                if let Some(notes) = changelog {
                    payload.insert("changelog".into(), json!(notes));
                }
                "publish"
            }
        };

        let spinner = make_spinner("Publishing...", interactive);
        if !interactive && !args.json {
            eprintln!("Publishing...");
        }
        let result = api
            .publish_and_wait(&session_route, endpoint, &Value::Object(payload), |phase| {
                if interactive {
                    spinner.set_message(format!("{phase}..."));
                } else if !args.json {
                    eprintln!("  {phase}...");
                }
            })
            .await?;
        spinner.finish_and_clear();

        Ok(json!({
            "title_id": title_id,
            "platform": platform,
            "version": version,
            "upload": upload_summary,
            "result": result,
        }))
    }
    .await;

    match run_result {
        Ok(output) => {
            if args.json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                let res = output.get("result").unwrap_or(&Value::Null);
                let build = res.get("build").unwrap_or(&Value::Null);
                let build_version = build
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or(&version);
                let status = build
                    .get("deploy_status")
                    .or_else(|| build.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("registered");
                let build_id = build.get("id").and_then(Value::as_str).unwrap_or("?");
                println!("Build {build_version} on {platform}: {status} (id {build_id})");
                if let (Some(uploaded), Some(reused)) = (
                    res.get("uploaded").and_then(Value::as_u64),
                    res.get("reused").and_then(Value::as_u64),
                ) {
                    println!("  {uploaded} new chunks stored, {reused} already on Arcane");
                }
                if let Some(warnings) = res.get("warnings").and_then(Value::as_array) {
                    for w in warnings {
                        let code = w.get("code").and_then(Value::as_str).unwrap_or("warning");
                        println!("  warning: {code}");
                    }
                }
            }
            Ok(())
        }
        Err(err) => {
            let _ = api.delete_upload_session(&session_route).await;
            Err(err)
        }
    }
}

fn merge_web_settings(
    out: &mut Map<String, Value>,
    args: &PushArgs,
    cfg: Option<&ProjectWebConfig>,
) -> Result<()> {
    if let Some(entry) = args
        .entry
        .as_ref()
        .or_else(|| cfg.and_then(|c| c.entry.as_ref()))
    {
        out.insert("entry_path".into(), json!(entry));
    }
    if let Some(vp) = args
        .viewport
        .as_ref()
        .or_else(|| cfg.and_then(|c| c.viewport.as_ref()))
    {
        let (w, h) = parse_viewport(vp)?;
        out.insert("viewport_width".into(), json!(w));
        out.insert("viewport_height".into(), json!(h));
    }
    if args.no_fullscreen || cfg.and_then(|c| c.no_fullscreen).unwrap_or(false) {
        out.insert("fullscreen_allowed".into(), json!(false));
    }
    let origins: Vec<String> = if let Some(ref raw) = args.origins {
        split_csv(raw)
    } else {
        cfg.map(|c| c.origins.clone()).unwrap_or_default()
    };
    if !origins.is_empty() {
        out.insert("allowed_origins".into(), json!(origins));
    }
    if args.unsafe_eval || cfg.and_then(|c| c.unsafe_eval).unwrap_or(false) {
        out.insert("unsafe_eval".into(), json!(true));
    }
    let inputs: Vec<String> = if let Some(ref raw) = args.inputs {
        split_csv(raw)
    } else {
        cfg.map(|c| c.inputs.clone()).unwrap_or_default()
    };
    if !inputs.is_empty() {
        out.insert("inputs".into(), json!(inputs));
    }
    if args.sdk_bridge || cfg.and_then(|c| c.sdk_bridge).unwrap_or(false) {
        out.insert("sdk_bridge".into(), json!(true));
    }
    Ok(())
}

fn parse_viewport(raw: &str) -> Result<(u32, u32)> {
    let mut parts = raw.split('x');
    let w = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| CliError::msg("--viewport must look like 1280x720"))?;
    let h = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| CliError::msg("--viewport must look like 1280x720"))?;
    if parts.next().is_some() {
        return Err(CliError::msg("--viewport must look like 1280x720"));
    }
    Ok((w, h))
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn detect_git_tag() -> Option<String> {
    if let Ok(output) = Command::new("git")
        .args(["describe", "--tags", "--exact-match"])
        .output()
    {
        if output.status.success() {
            let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !tag.is_empty() {
                return Some(tag);
            }
        }
    }
    std::env::var("GITHUB_REF_NAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
