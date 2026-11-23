use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::fastcdc::{ChunkingMode, RawChunk};

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime_secs: i64,
    mtime_nanos: u32,
    mode: ChunkingMode,
    fixed_chunk_size: usize,
    chunks: Vec<RawChunk>,
}

/// Local hash cache mapping `(canonical_path, size, mtime, chunking_mode, fixed_chunk_size)`
/// to precomputed `Vec<RawChunk>`.
///
/// On a 150 GB game hotfix where only 50 MB changed, unchanged files are never re-read from disk.
pub struct HashCache {
    path: PathBuf,
    entries: HashMap<String, CacheEntry>,
    dirty: bool,
}

impl HashCache {
    pub fn default_path() -> PathBuf {
        let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
        base.join("arcane").join("hash-cache-v1.json")
    }

    pub fn load(path: PathBuf) -> Self {
        let entries = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<CacheFile>(&text).ok())
            .filter(|f| f.version == CACHE_VERSION)
            .map(|f| f.entries)
            .unwrap_or_default();

        Self {
            path,
            entries,
            dirty: false,
        }
    }

    pub fn load_default() -> Self {
        Self::load(Self::default_path())
    }

    pub fn get(
        &self,
        file_path: &Path,
        size: u64,
        mtime_secs: i64,
        mtime_nanos: u32,
        mode: ChunkingMode,
        fixed_chunk_size: usize,
    ) -> Option<&[RawChunk]> {
        let key = cache_key(file_path);
        let entry = self.entries.get(&key)?;
        if entry.size == size
            && entry.mtime_secs == mtime_secs
            && entry.mtime_nanos == mtime_nanos
            && entry.mode == mode
            && entry.fixed_chunk_size == fixed_chunk_size
        {
            Some(&entry.chunks)
        } else {
            None
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        file_path: &Path,
        size: u64,
        mtime_secs: i64,
        mtime_nanos: u32,
        mode: ChunkingMode,
        fixed_chunk_size: usize,
        chunks: Vec<RawChunk>,
    ) {
        let key = cache_key(file_path);
        self.entries.insert(
            key,
            CacheEntry {
                size,
                mtime_secs,
                mtime_nanos,
                mode,
                fixed_chunk_size,
                chunks,
            },
        );
        self.dirty = true;
    }

    /// Persist cache atomically (write temp file then rename) if modified.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = CacheFile {
            version: CACHE_VERSION,
            entries: self.entries.clone(),
        };
        let bytes = serde_json::to_vec(&payload)?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &self.path)?;
        self.dirty = false;
        Ok(())
    }
}

pub fn file_mtime(meta: &fs::Metadata) -> (i64, u32) {
    match meta.modified() {
        Ok(time) => match time.duration_since(UNIX_EPOCH) {
            Ok(dur) => (dur.as_secs() as i64, dur.subsec_nanos()),
            Err(err) => {
                let dur = err.duration();
                (-(dur.as_secs() as i64), dur.subsec_nanos())
            }
        },
        Err(_) => (0, 0),
    }
}

fn cache_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn cache_roundtrip_and_invalidation() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("cache.json");
        let file_path = dir.path().join("game.pak");
        fs::write(&file_path, b"hello").unwrap();

        let chunks = vec![RawChunk {
            offset: 0,
            raw_hash: "a".repeat(64),
            raw_size: 5,
        }];

        let mut cache = HashCache::load(cache_path.clone());
        cache.insert(
            &file_path,
            5,
            1000,
            500,
            ChunkingMode::Cdc,
            0,
            chunks.clone(),
        );
        cache.save().unwrap();

        let reloaded = HashCache::load(cache_path);
        assert_eq!(
            reloaded.get(&file_path, 5, 1000, 500, ChunkingMode::Cdc, 0),
            Some(chunks.as_slice())
        );
        // Different mtime invalidates cache hit.
        assert!(reloaded
            .get(&file_path, 5, 1001, 500, ChunkingMode::Cdc, 0)
            .is_none());
        // Different chunking mode invalidates cache hit.
        assert!(reloaded
            .get(&file_path, 5, 1000, 500, ChunkingMode::Fixed, 0)
            .is_none());
    }
}
