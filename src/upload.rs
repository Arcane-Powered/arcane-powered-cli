use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::stream::{self, StreamExt, TryStreamExt};
use rayon::prelude::*;
use serde::Serialize;

use crate::api::{
    ApiClient, CasBlobDescriptor, CasFileChunkEntry, PlanChunkRequest, RecordCasFileRequest,
};
use crate::cache::HashCache;
use crate::compress::{compress_if_beneficial, ChunkEncoding};
use crate::error::{CliError, Result};
use crate::fastcdc::{chunk_file, sha256_hex, ChunkingMode, RawChunk};
use crate::progress::{format_bytes, make_bytes_bar, make_count_bar};
use crate::walker::LocalFile;

const CHUNK_PLAN_BATCH: usize = 500;
const CHUNK_UPLOAD_BATCH: usize = 32;
const FILE_REGISTER_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Serialize)]
pub struct CasUploadStats {
    pub total_files: usize,
    pub total_bytes: u64,
    pub cache_hit_files: usize,
    pub total_unique_chunks: usize,
    pub reused_chunks: usize,
    pub reused_bytes: u64,
    pub uploaded_chunks: usize,
    pub uploaded_raw_bytes: u64,
    pub uploaded_stored_bytes: u64,
    pub dedup_percent: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MultipartUploadStats {
    pub uploaded_files: usize,
    pub skipped_files: usize,
    pub uploaded_bytes: u64,
}

#[derive(Debug, Clone)]
struct ChunkSource {
    file: PathBuf,
    offset: u64,
    raw_hash: String,
    raw_size: u64,
}

#[derive(Debug, Clone)]
struct ResolvedChunk {
    stored_hash: String,
    raw_hash: String,
    raw_size: u64,
    stored_size: u64,
    encoding: ChunkEncoding,
    existed: bool,
}

struct PreparedChunk {
    raw_hash: String,
    raw_size: u64,
    stored_hash: String,
    stored_size: u64,
    encoding: ChunkEncoding,
    stored: Vec<u8>,
}

fn chunk_key(raw_hash: &str, raw_size: u64) -> String {
    format!("{raw_hash}:{raw_size}")
}

/// Direct CAS chunked upload for native builds (`windows`, `macos`, `linux`).
#[allow(clippy::too_many_arguments)]
pub async fn upload_all_cas(
    api: &ApiClient,
    session_route: &str,
    files: &[LocalFile],
    fixed_chunk_size: usize,
    allow_zstd: bool,
    mode: ChunkingMode,
    concurrency: usize,
    use_cache: bool,
    interactive: bool,
    json_mode: bool,
) -> Result<CasUploadStats> {
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();

    // Step 1: Check local hash cache, then chunk & hash uncached files in parallel via rayon.
    let mut cache = if use_cache {
        Some(HashCache::load_default())
    } else {
        None
    };

    let mut cached_results: Vec<Option<Vec<RawChunk>>> = vec![None; files.len()];
    let mut uncached_indices = Vec::new();
    let mut cache_hit_files = 0usize;
    let mut cached_bytes = 0u64;

    for (idx, f) in files.iter().enumerate() {
        if f.size == 0 {
            cached_results[idx] = Some(Vec::new());
            continue;
        }
        if let Some(ref c) = cache {
            if let Some(hit) = c.get(
                &f.full_path,
                f.size,
                f.mtime_secs,
                f.mtime_nanos,
                mode,
                fixed_chunk_size,
            ) {
                cached_results[idx] = Some(hit.to_vec());
                cache_hit_files += 1;
                cached_bytes += f.size;
                continue;
            }
        }
        uncached_indices.push(idx);
    }

    let hash_label = match mode {
        ChunkingMode::Cdc => "Hashing (FastCDC)",
        ChunkingMode::Fixed => "Hashing (fixed)",
    };
    let hash_bar = make_bytes_bar(total_bytes, hash_label, interactive);
    hash_bar.inc(cached_bytes);

    if !uncached_indices.is_empty() {
        let tasks: Vec<(usize, PathBuf)> = uncached_indices
            .into_iter()
            .map(|idx| (idx, files[idx].full_path.clone()))
            .collect();
        let bar_clone = hash_bar.clone();
        let computed: Result<Vec<(usize, Vec<RawChunk>)>> =
            tokio::task::spawn_blocking(move || {
                tasks
                    .into_par_iter()
                    .map(|(idx, full_path)| {
                        let chunks = chunk_file(&full_path, mode, fixed_chunk_size, |n| {
                            bar_clone.inc(n);
                        })?;
                        Ok((idx, chunks))
                    })
                    .collect()
            })
            .await
            .map_err(|e| CliError::msg(format!("Hashing task failed: {e}")))?;

        for (idx, chunks) in computed? {
            if let Some(ref mut c) = cache {
                let f = &files[idx];
                c.insert(
                    &f.full_path,
                    f.size,
                    f.mtime_secs,
                    f.mtime_nanos,
                    mode,
                    fixed_chunk_size,
                    chunks.clone(),
                );
            }
            cached_results[idx] = Some(chunks);
        }
    }
    hash_bar.finish_and_clear();

    if let Some(ref mut c) = cache {
        let _ = c.save();
    }

    // Build file -> chunks list and deduplicated unique raw chunk map.
    let mut file_chunks: Vec<(&LocalFile, Vec<RawChunk>)> = Vec::with_capacity(files.len());
    let mut unique_raw: HashMap<String, ChunkSource> = HashMap::new();
    let mut unique_order: Vec<String> = Vec::new();

    for (f, maybe_chunks) in files.iter().zip(cached_results) {
        let raw_chunks = maybe_chunks.unwrap_or_default();
        for rc in &raw_chunks {
            let key = chunk_key(&rc.raw_hash, rc.raw_size);
            if let std::collections::hash_map::Entry::Vacant(slot) = unique_raw.entry(key) {
                unique_order.push(slot.key().clone());
                slot.insert(ChunkSource {
                    file: f.full_path.clone(),
                    offset: rc.offset,
                    raw_hash: rc.raw_hash.clone(),
                    raw_size: rc.raw_size,
                });
            }
        }
        file_chunks.push((f, raw_chunks));
    }

    // Step 2: Check which chunks already exist on Arcane in batches of 500.
    let mut resolved: HashMap<String, ResolvedChunk> = HashMap::new();
    let mut missing_raw: Vec<ChunkSource> = Vec::new();
    let mut reused_chunks = 0usize;
    let mut reused_bytes = 0u64;

    for batch_keys in unique_order.chunks(CHUNK_PLAN_BATCH) {
        let req_chunks: Vec<PlanChunkRequest> = batch_keys
            .iter()
            .filter_map(|k| unique_raw.get(k))
            .map(|c| PlanChunkRequest {
                raw_hash: c.raw_hash.clone(),
                raw_size: c.raw_size,
                stored_hash: None,
                stored_size: None,
                encoding: None,
            })
            .collect();

        let plan = api.plan_chunks(session_route, &req_chunks).await?;
        for hit in plan.present {
            let key = chunk_key(&hit.raw_hash, hit.raw_size);
            reused_chunks += 1;
            reused_bytes += hit.raw_size;
            resolved.insert(
                key,
                ResolvedChunk {
                    stored_hash: hit.stored_hash,
                    raw_hash: hit.raw_hash,
                    raw_size: hit.raw_size,
                    stored_size: hit.stored_size,
                    encoding: hit.encoding,
                    existed: true,
                },
            );
        }
        for miss in plan.missing {
            let key = chunk_key(&miss.raw_hash, miss.raw_size);
            if let Some(src) = unique_raw.get(&key) {
                missing_raw.push(src.clone());
            }
        }
    }

    let missing_raw_bytes: u64 = missing_raw.iter().map(|m| m.raw_size).sum();
    let unique_total_bytes = reused_bytes + missing_raw_bytes;
    let dedup_percent = if unique_total_bytes > 0 {
        (reused_bytes as f64 / unique_total_bytes as f64) * 100.0
    } else {
        100.0
    };

    if !json_mode {
        let cache_note = if cache_hit_files > 0 {
            format!(" · {cache_hit_files} files from hash cache")
        } else {
            String::new()
        };
        eprintln!(
            "  Delta check: {reused_chunks} chunks ({}, {:.1}% deduplicated) already on Arcane · {} chunks ({}) to upload{cache_note}",
            format_bytes(reused_bytes),
            dedup_percent,
            missing_raw.len(),
            format_bytes(missing_raw_bytes),
        );
    }

    // Step 3: Process ONLY missing chunks in bounded 32-chunk batches.
    let upload_bar = make_bytes_bar(missing_raw_bytes, "Uploading", interactive);
    let uploaded_chunks_count = Arc::new(AtomicU64::new(0));
    let uploaded_raw_bytes_count = Arc::new(AtomicU64::new(0));
    let uploaded_stored_bytes_count = Arc::new(AtomicU64::new(0));

    for batch in missing_raw.chunks(CHUNK_UPLOAD_BATCH) {
        let batch_owned: Vec<ChunkSource> = batch.to_vec();
        // Read & compress chunks in parallel using rayon.
        let prepared: Vec<PreparedChunk> = tokio::task::spawn_blocking(move || {
            batch_owned
                .into_par_iter()
                .map(|src| -> Result<PreparedChunk> {
                    let mut fh = File::open(&src.file)?;
                    fh.seek(SeekFrom::Start(src.offset))?;
                    let mut raw = vec![0u8; src.raw_size as usize];
                    fh.read_exact(&mut raw).map_err(|e| {
                        CliError::msg(format!(
                            "Short read on {} at offset {}: {e}",
                            src.file.display(),
                            src.offset
                        ))
                    })?;
                    let compressed = compress_if_beneficial(raw, allow_zstd)?;
                    let stored_hash = match compressed.encoding {
                        ChunkEncoding::Identity => src.raw_hash.clone(),
                        ChunkEncoding::Zstd => sha256_hex(&compressed.stored),
                    };
                    let stored_size = compressed.stored.len() as u64;
                    Ok(PreparedChunk {
                        raw_hash: src.raw_hash,
                        raw_size: src.raw_size,
                        stored_hash,
                        stored_size,
                        encoding: compressed.encoding,
                        stored: compressed.stored,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .await
        .map_err(|e| CliError::msg(format!("Compression task failed: {e}")))??;

        // Request fresh presigned PUT URLs right before uploading this batch.
        let plan_req: Vec<PlanChunkRequest> = prepared
            .iter()
            .map(|p| PlanChunkRequest {
                raw_hash: p.raw_hash.clone(),
                raw_size: p.raw_size,
                stored_hash: Some(p.stored_hash.clone()),
                stored_size: Some(p.stored_size),
                encoding: Some(p.encoding),
            })
            .collect();

        let fresh_plan = api.plan_chunks(session_route, &plan_req).await?;

        let mut by_key: HashMap<String, PreparedChunk> = prepared
            .into_iter()
            .map(|p| (chunk_key(&p.raw_hash, p.raw_size), p))
            .collect();

        for hit in fresh_plan.present {
            let key = chunk_key(&hit.raw_hash, hit.raw_size);
            upload_bar.inc(hit.raw_size);
            resolved.insert(
                key,
                ResolvedChunk {
                    stored_hash: hit.stored_hash,
                    raw_hash: hit.raw_hash,
                    raw_size: hit.raw_size,
                    stored_size: hit.stored_size,
                    encoding: hit.encoding,
                    existed: true,
                },
            );
        }

        let mut upload_queue = Vec::new();
        for miss in fresh_plan.missing {
            let key = chunk_key(&miss.raw_hash, miss.raw_size);
            if let Some(item) = by_key.remove(&key) {
                let url = miss.upload_url.ok_or_else(|| {
                    CliError::msg(format!(
                        "Missing presigned upload_url for chunk {}",
                        item.raw_hash
                    ))
                })?;
                upload_queue.push((item, url, miss.headers));
            }
        }

        let committed_batch: Vec<CasBlobDescriptor> = stream::iter(upload_queue)
            .map(|(item, url, headers)| {
                let api = api.clone();
                let bar = upload_bar.clone();
                let chunks_counter = uploaded_chunks_count.clone();
                let raw_counter = uploaded_raw_bytes_count.clone();
                let stored_counter = uploaded_stored_bytes_count.clone();
                async move {
                    api.put_presigned(&url, &headers, item.stored).await?;
                    bar.inc(item.raw_size);
                    chunks_counter.fetch_add(1, Ordering::Relaxed);
                    raw_counter.fetch_add(item.raw_size, Ordering::Relaxed);
                    stored_counter.fetch_add(item.stored_size, Ordering::Relaxed);
                    Ok::<CasBlobDescriptor, CliError>(CasBlobDescriptor {
                        stored_hash: item.stored_hash,
                        raw_hash: item.raw_hash,
                        raw_size: item.raw_size,
                        stored_size: item.stored_size,
                        encoding: item.encoding,
                    })
                }
            })
            .buffer_unordered(concurrency.max(1))
            .try_collect()
            .await?;

        if !committed_batch.is_empty() {
            api.commit_chunks(session_route, &committed_batch).await?;
            for b in committed_batch {
                let key = chunk_key(&b.raw_hash, b.raw_size);
                resolved.insert(
                    key,
                    ResolvedChunk {
                        stored_hash: b.stored_hash,
                        raw_hash: b.raw_hash,
                        raw_size: b.raw_size,
                        stored_size: b.stored_size,
                        encoding: b.encoding,
                        existed: false,
                    },
                );
            }
        }
    }
    upload_bar.finish_and_clear();

    // Step 4: Record each file's ordered CAS manifest entry in the upload session.
    let reg_bar = make_count_bar(file_chunks.len() as u64, "Registering", interactive);
    let mut file_requests = Vec::with_capacity(file_chunks.len());

    for (file, raw_chunks) in file_chunks {
        let mut all_reused = true;
        let mut chunks = Vec::with_capacity(raw_chunks.len());
        for rc in raw_chunks {
            let key = chunk_key(&rc.raw_hash, rc.raw_size);
            let info = resolved.get(&key).ok_or_else(|| {
                CliError::msg(format!(
                    "Internal error: missing resolved chunk {}",
                    rc.raw_hash
                ))
            })?;
            if !info.existed {
                all_reused = false;
            }
            chunks.push(CasFileChunkEntry {
                hash: info.stored_hash.clone(),
                raw_hash: info.raw_hash.clone(),
                raw_size: info.raw_size,
                stored_size: info.stored_size,
                encoding: info.encoding,
            });
        }
        file_requests.push(RecordCasFileRequest {
            path: file.relative.clone(),
            size: file.size,
            chunks,
            all_reused,
        });
    }

    stream::iter(file_requests)
        .map(|req| {
            let api = api.clone();
            let route = session_route.to_string();
            let bar = reg_bar.clone();
            async move {
                api.record_cas_file(&route, &req).await?;
                bar.inc(1);
                Ok::<(), CliError>(())
            }
        })
        .buffer_unordered(FILE_REGISTER_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;
    reg_bar.finish_and_clear();

    Ok(CasUploadStats {
        total_files: files.len(),
        total_bytes,
        cache_hit_files,
        total_unique_chunks: unique_order.len(),
        reused_chunks,
        reused_bytes,
        uploaded_chunks: uploaded_chunks_count.load(Ordering::Relaxed) as usize,
        uploaded_raw_bytes: uploaded_raw_bytes_count.load(Ordering::Relaxed),
        uploaded_stored_bytes: uploaded_stored_bytes_count.load(Ordering::Relaxed),
        dedup_percent,
    })
}

/// Legacy multipart upload (used for `web` and `android` builds).
pub async fn upload_all_multipart(
    api: &ApiClient,
    title_id: &str,
    session_id: &str,
    files: &[LocalFile],
    already_there: &HashMap<String, u64>,
    concurrency: usize,
    interactive: bool,
) -> Result<MultipartUploadStats> {
    let todo: Vec<LocalFile> = files
        .iter()
        .filter(|f| already_there.get(&f.relative).copied() != Some(f.size))
        .cloned()
        .collect();

    let total_bytes: u64 = todo.iter().map(|f| f.size).sum();
    let skipped = files.len() - todo.len();
    let uploaded_count = todo.len();
    let route = format!("/titles/{title_id}/builds/upload-sessions/{session_id}/files");
    let bar = make_bytes_bar(total_bytes, "Uploading", interactive);

    stream::iter(todo)
        .map(|file| {
            let api = api.clone();
            let route = route.clone();
            let bar = bar.clone();
            async move {
                let bytes = tokio::fs::read(&file.full_path).await?;
                let file_name = file
                    .full_path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".to_string());
                api.upload_multipart_file(&route, &file.relative, &file_name, bytes)
                    .await?;
                bar.inc(file.size);
                Ok::<(), CliError>(())
            }
        })
        .buffer_unordered(concurrency.max(1))
        .try_collect::<Vec<_>>()
        .await?;

    bar.finish_and_clear();

    Ok(MultipartUploadStats {
        uploaded_files: uploaded_count,
        skipped_files: skipped,
        uploaded_bytes: total_bytes,
    })
}
