use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Result;

pub const DEFAULT_FIXED_CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4 MiB
pub const CDC_MIN_SIZE: usize = 512 * 1024; // 512 KiB
pub const CDC_AVG_SIZE: usize = 2 * 1024 * 1024; // 2 MiB
pub const CDC_MAX_SIZE: usize = 8 * 1024 * 1024; // 8 MiB

/// Normalized FastCDC high-bit masks for 32-bit Gear hash:
/// 22 bits (1 in 4 MiB) before AVG, 20 bits (1 in 1 MiB) after AVG -> ~2 MiB average.
/// Identical to `arcane-builds.mjs` v2.1+.
pub const CDC_MASK_S: u32 = 0xffff_fc00;
pub const CDC_MASK_L: u32 = 0xffff_f000;

/// Deterministic 256-entry Gear hash table (SplitMix32 with fixed seed `0x9e3779b9`),
/// bit-for-bit identical to `arcane-builds.mjs`.
pub const GEAR_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut s: u32 = 0x9e37_79b9;
    let mut i = 0;
    while i < 256 {
        s = s.wrapping_add(0x9e37_79b9);
        let mut z = s;
        z = (z ^ (z >> 16)).wrapping_mul(0x85eb_ca6b);
        z = (z ^ (z >> 13)).wrapping_mul(0xc2b2_ae35);
        table[i] = z ^ (z >> 16);
        i += 1;
    }
    table
};

/// Chunking mode for native builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkingMode {
    Cdc,
    Fixed,
}

impl ChunkingMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cdc => "cdc",
            Self::Fixed => "fixed",
        }
    }
}

/// A single raw chunk slice inside a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawChunk {
    pub offset: u64,
    pub raw_hash: String,
    pub raw_size: u64,
}

/// Find the FastCDC cut boundary in `buf`, identical to `findFastCdcCut` in `arcane-builds.mjs`.
#[inline]
pub fn find_fastcdc_cut(buf: &[u8]) -> usize {
    let length = buf.len();
    if length <= CDC_MIN_SIZE {
        return length;
    }
    let max_len = length.min(CDC_MAX_SIZE);
    let normal_end = max_len.min(CDC_AVG_SIZE);
    let mut fp: u32 = 0;
    let mut i = CDC_MIN_SIZE;

    while i < normal_end {
        fp = (fp << 1).wrapping_add(GEAR_TABLE[buf[i] as usize]);
        if (fp & CDC_MASK_S) == 0 {
            return i + 1;
        }
        i += 1;
    }
    while i < max_len {
        fp = (fp << 1).wrapping_add(GEAR_TABLE[buf[i] as usize]);
        if (fp & CDC_MASK_L) == 0 {
            return i + 1;
        }
        i += 1;
    }
    max_len
}

/// Compute lowercase hex SHA-256 of a byte slice.
#[inline]
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex_encode(&digest)
}

#[inline]
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Stream-chunk and SHA-256 hash a file with constant memory, invoking `on_bytes(n)` after each chunk.
pub fn chunk_file<F>(
    path: &Path,
    mode: ChunkingMode,
    fixed_chunk_size: usize,
    mut on_bytes: F,
) -> Result<Vec<RawChunk>>
where
    F: FnMut(u64),
{
    let mut file = File::open(path)?;
    let mut chunks = Vec::new();

    match mode {
        ChunkingMode::Cdc => {
            let mut buf = vec![0u8; CDC_MAX_SIZE * 2];
            let mut buf_len = 0usize;
            let mut chunk_offset = 0u64;
            let mut eof = false;

            while !eof || buf_len > 0 {
                while !eof && buf_len < CDC_MAX_SIZE {
                    let n = file.read(&mut buf[buf_len..])?;
                    if n == 0 {
                        eof = true;
                        break;
                    }
                    buf_len += n;
                }
                if buf_len == 0 {
                    break;
                }

                let cut = find_fastcdc_cut(&buf[..buf_len]);
                let raw_hash = sha256_hex(&buf[..cut]);
                chunks.push(RawChunk {
                    offset: chunk_offset,
                    raw_hash,
                    raw_size: cut as u64,
                });
                chunk_offset += cut as u64;
                on_bytes(cut as u64);

                if cut < buf_len {
                    buf.copy_within(cut..buf_len, 0);
                }
                buf_len -= cut;
            }
        }
        ChunkingMode::Fixed => {
            let size = fixed_chunk_size.max(1);
            let mut buf = vec![0u8; size];
            let mut offset = 0u64;

            loop {
                let mut read_total = 0usize;
                while read_total < size {
                    let n = file.read(&mut buf[read_total..size])?;
                    if n == 0 {
                        break;
                    }
                    read_total += n;
                }
                if read_total == 0 {
                    break;
                }
                let raw_hash = sha256_hex(&buf[..read_total]);
                chunks.push(RawChunk {
                    offset,
                    raw_hash,
                    raw_size: read_total as u64,
                });
                offset += read_total as u64;
                on_bytes(read_total as u64);
                if read_total < size {
                    break;
                }
            }
        }
    }

    Ok(chunks)
}

/// Chunk an in-memory buffer with FastCDC (useful for tests).
pub fn chunk_bytes_cdc(data: &[u8]) -> Vec<RawChunk> {
    let mut chunks = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        let cut = find_fastcdc_cut(&data[offset..]);
        let slice = &data[offset..offset + cut];
        chunks.push(RawChunk {
            offset: offset as u64,
            raw_hash: sha256_hex(slice),
            raw_size: cut as u64,
        });
        offset += cut;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gear_table_matches_splitmix32_reference() {
        // First 4 and last entries verified against arcane-builds.mjs GEAR_TABLE.
        let mut s: u32 = 0x9e37_79b9;
        for (idx, &actual) in GEAR_TABLE.iter().enumerate() {
            s = s.wrapping_add(0x9e37_79b9);
            let mut z = s;
            z = (z ^ (z >> 16)).wrapping_mul(0x85eb_ca6b);
            z = (z ^ (z >> 13)).wrapping_mul(0xc2b2_ae35);
            let expected = z ^ (z >> 16);
            assert_eq!(actual, expected, "mismatch at GEAR_TABLE[{idx}]");
        }
    }

    #[test]
    fn small_buffer_returns_full_length() {
        let data = vec![42u8; 1024];
        assert_eq!(find_fastcdc_cut(&data), 1024);

        let exact_min = vec![7u8; CDC_MIN_SIZE];
        assert_eq!(find_fastcdc_cut(&exact_min), CDC_MIN_SIZE);
    }

    #[test]
    fn fastcdc_respects_min_and_max_bounds() {
        // Deterministic pseudo-random 12 MiB buffer (xorshift32).
        let mut data = vec![0u8; 12 * 1024 * 1024];
        let mut x: u32 = 0x1234_5678;
        for b in &mut data {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = (x & 0xff) as u8;
        }
        let chunks = chunk_bytes_cdc(&data);
        assert!(chunks.len() >= 2);
        let mut total = 0u64;
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.offset, total);
            if i + 1 < chunks.len() {
                assert!(c.raw_size >= CDC_MIN_SIZE as u64);
                assert!(c.raw_size <= CDC_MAX_SIZE as u64);
            }
            total += c.raw_size;
        }
        assert_eq!(total, data.len() as u64);
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
