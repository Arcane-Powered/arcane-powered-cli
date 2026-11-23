use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde_json::json;

use crate::config::{ProjectConfig, PROJECT_CONFIG_FILENAME};
use crate::error::{CliError, Result};
use crate::fastcdc::ChunkingMode;

pub struct InitArgs {
    pub title: Option<String>,
    pub force: bool,
    pub json: bool,
}

pub fn run(args: InitArgs) -> Result<()> {
    let target = PathBuf::from(PROJECT_CONFIG_FILENAME);
    if target.exists() && !args.force {
        return Err(CliError::msg(format!(
            "{} already exists. Pass --force to overwrite.",
            target.display()
        )));
    }

    let mut platforms = BTreeMap::new();
    platforms.insert("windows".to_string(), "./build/windows".to_string());
    platforms.insert("macos".to_string(), "./build/macos".to_string());
    platforms.insert("linux".to_string(), "./build/linux".to_string());

    let cfg = ProjectConfig {
        title: args.title,
        chunking: Some(ChunkingMode::Cdc),
        compress: Some(true),
        concurrency: Some(6),
        ignore: vec![
            "*.pdb".to_string(),
            "*.dSYM".to_string(),
            ".git".to_string(),
            ".DS_Store".to_string(),
        ],
        platforms,
        web: None,
    };

    let content = serde_json::to_string_pretty(&cfg)?;
    fs::write(&target, format!("{content}\n"))?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "path": target,
                "config": cfg,
            }))?
        );
    } else {
        println!("Created {}", target.display());
        if cfg.title.is_none() {
            println!("  Tip: run `arcane titles` to find your title ID and add it to arcane.json.");
        }
    }

    Ok(())
}
