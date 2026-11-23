use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::cache::file_mtime;
use crate::error::{CliError, Result};

const DEFAULT_IGNORES: &[&str] = &[".git", ".DS_Store", "Thumbs.db"];

#[derive(Debug, Clone)]
pub struct LocalFile {
    pub full_path: PathBuf,
    pub relative: String,
    pub size: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
}

/// Walk `root` recursively, skipping default ignores (`.git`, `.DS_Store`, `Thumbs.db`)
/// plus any user-provided patterns (exact entry names or simple `*.ext` / prefix* globs).
pub fn list_files(root: &Path, extra_ignores: &[String]) -> Result<Vec<LocalFile>> {
    if !root.is_dir() {
        return Err(CliError::msg(format!("{} is not a folder", root.display())));
    }

    let mut out = Vec::new();
    let walker = WalkDir::new(root).into_iter().filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        let name = entry.file_name().to_string_lossy();
        !should_ignore(&name, extra_ignores)
    });

    for entry in walker {
        let entry = entry.map_err(|e| CliError::msg(format!("Failed reading directory: {e}")))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let full_path = entry.into_path();
        let meta = std::fs::metadata(&full_path)?;
        let (mtime_secs, mtime_nanos) = file_mtime(&meta);
        let rel = full_path
            .strip_prefix(root)
            .map_err(|e| CliError::msg(format!("Path strip error: {e}")))?;
        let relative = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");

        out.push(LocalFile {
            full_path,
            relative,
            size: meta.len(),
            mtime_secs,
            mtime_nanos,
        });
    }

    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

fn should_ignore(name: &str, extra_ignores: &[String]) -> bool {
    if DEFAULT_IGNORES.contains(&name) {
        return true;
    }
    for pattern in extra_ignores {
        if matches_pattern(name, pattern) {
            return true;
        }
    }
    false
}

/// Supports exact match, `*.ext`, `prefix*`, and `*contains*`.
fn matches_pattern(name: &str, pattern: &str) -> bool {
    if pattern == name {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        if let Some(middle) = suffix.strip_suffix('*') {
            return name.contains(middle);
        }
        return name.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn skips_default_and_custom_ignores() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), b"x").unwrap();
        fs::write(dir.path().join(".DS_Store"), b"x").unwrap();
        fs::write(dir.path().join("game.exe"), b"bin").unwrap();
        fs::write(dir.path().join("game.pdb"), b"sym").unwrap();
        fs::create_dir_all(dir.path().join("data")).unwrap();
        fs::write(dir.path().join("data/level1.pak"), b"lvl").unwrap();

        let files = list_files(dir.path(), &["*.pdb".to_string()]).unwrap();
        let rels: Vec<&str> = files.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(rels, vec!["data/level1.pak", "game.exe"]);
    }
}
