use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Match `arcane-builds.mjs`: zstd level 3, only keep compressed if <= 92% of raw size.
pub const ZSTD_LEVEL: i32 = 3;
pub const COMPRESSION_RATIO_THRESHOLD: f64 = 0.92;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkEncoding {
    Zstd,
    Identity,
}

impl ChunkEncoding {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Zstd => "zstd",
            Self::Identity => "identity",
        }
    }
}

pub struct CompressedChunk {
    pub encoding: ChunkEncoding,
    pub stored: Vec<u8>,
}

/// Compress a chunk with zstd level 3 if beneficial (<= 92% of raw size), otherwise return raw.
pub fn compress_if_beneficial(raw: Vec<u8>, allow_zstd: bool) -> Result<CompressedChunk> {
    if !allow_zstd || raw.is_empty() {
        return Ok(CompressedChunk {
            encoding: ChunkEncoding::Identity,
            stored: raw,
        });
    }

    let compressed = zstd::bulk::compress(&raw, ZSTD_LEVEL)?;
    let threshold = ((raw.len() as f64) * COMPRESSION_RATIO_THRESHOLD).floor() as usize;
    if compressed.len() <= threshold {
        Ok(CompressedChunk {
            encoding: ChunkEncoding::Zstd,
            stored: compressed,
        })
    } else {
        Ok(CompressedChunk {
            encoding: ChunkEncoding::Identity,
            stored: raw,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressible_data_uses_zstd() {
        let raw = vec![b'A'; 64 * 1024];
        let out = compress_if_beneficial(raw.clone(), true).unwrap();
        assert_eq!(out.encoding, ChunkEncoding::Zstd);
        assert!(out.stored.len() < raw.len());
        let decoded = zstd::bulk::decompress(&out.stored, raw.len()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn no_compress_flag_uses_identity() {
        let raw = vec![b'A'; 64 * 1024];
        let out = compress_if_beneficial(raw.clone(), false).unwrap();
        assert_eq!(out.encoding, ChunkEncoding::Identity);
        assert_eq!(out.stored, raw);
    }

    #[test]
    fn incompressible_data_falls_back_to_identity() {
        // Pseudo-random incompressible bytes.
        let mut raw = vec![0u8; 32 * 1024];
        let mut x: u32 = 0xdead_beef;
        for b in &mut raw {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = (x & 0xff) as u8;
        }
        let out = compress_if_beneficial(raw.clone(), true).unwrap();
        assert_eq!(out.encoding, ChunkEncoding::Identity);
        assert_eq!(out.stored, raw);
    }
}
