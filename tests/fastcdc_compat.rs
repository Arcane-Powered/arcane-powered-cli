use std::collections::HashSet;
use std::fs;

use arcane_cli::fastcdc::{chunk_bytes_cdc, chunk_file, ChunkingMode, GEAR_TABLE};
use tempfile::tempdir;

fn gen_xorshift32(len: usize, seed: u32) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let mut x = seed;
    for b in &mut buf {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = (x & 0xff) as u8;
    }
    buf
}

#[test]
fn gear_table_matches_arcane_builds_mjs_bit_for_bit() {
    assert_eq!(
        &GEAR_TABLE[..4],
        &[0x3cd6_e3f3, 0x1b14_7dcc, 0x4c08_1dbf, 0x4879_81ab]
    );
    assert_eq!(
        &GEAR_TABLE[252..],
        &[0x4c7f_81b9, 0x8987_eb0e, 0x7970_d887, 0xa637_99a4]
    );
}

#[test]
fn fastcdc_cuts_and_hashes_match_arcane_builds_mjs_exactly() {
    // Golden vector generated directly by publisher-portal/public/cli/arcane-builds.mjs (v2.2.0).
    let expected: &[(u64, u64, &str)] = &[
        (
            0,
            3_613_945,
            "aeb8ee6a8513d44324b080f900fb522c8596f0d68e58b385312b8e37d3f22166",
        ),
        (
            3_613_945,
            2_258_569,
            "a09648864464b3bcead85de7e2d35790ec37436058e3e19cbf3f87008a50493c",
        ),
        (
            5_872_514,
            1_555_702,
            "3d6c36d5bac6c2e9c9ca59e6800328f4d0f4fc954b329a4f33e65100b2116753",
        ),
        (
            7_428_216,
            2_383_380,
            "1a2927698a2c69a91adace79ab9472465ba89b953287b9d703018e9e602fe615",
        ),
        (
            9_811_596,
            756_588,
            "ad7698a79e4f53e72f040762fb2247a35023281d878b23cebf047b1da8f7e4b8",
        ),
        (
            10_568_184,
            2_014_728,
            "94ed6c20cabe1fc737b64a537e3cbefaae84c0f4f9153486b3c99a17fe04a1bd",
        ),
    ];

    let data = gen_xorshift32(12 * 1024 * 1024, 0x1234_5678);

    // 1. In-memory FastCDC
    let mem_chunks = chunk_bytes_cdc(&data);
    assert_eq!(mem_chunks.len(), expected.len());
    for (actual, &(exp_off, exp_size, exp_hash)) in mem_chunks.iter().zip(expected.iter()) {
        assert_eq!(actual.offset, exp_off);
        assert_eq!(actual.raw_size, exp_size);
        assert_eq!(actual.raw_hash, exp_hash);
    }

    // 2. Streaming file FastCDC must match in-memory 100%
    let dir = tempdir().unwrap();
    let path = dir.path().join("asset.pak");
    fs::write(&path, &data).unwrap();

    let mut progress_total = 0u64;
    let file_chunks = chunk_file(&path, ChunkingMode::Cdc, 0, |n| progress_total += n).unwrap();
    assert_eq!(progress_total, data.len() as u64);
    assert_eq!(file_chunks, mem_chunks);
}

#[test]
fn fastcdc_hotfix_insertion_reuses_shifted_chunks() {
    let original = gen_xorshift32(12 * 1024 * 1024, 0x1234_5678);
    let prefix = gen_xorshift32(1337, 0xcafe_babe);
    let mut patched = Vec::with_capacity(prefix.len() + original.len());
    patched.extend_from_slice(&prefix);
    patched.extend_from_slice(&original);

    let c1 = chunk_bytes_cdc(&original);
    let c2 = chunk_bytes_cdc(&patched);

    let h1: HashSet<&str> = c1.iter().map(|c| c.raw_hash.as_str()).collect();
    let reused = c2
        .iter()
        .filter(|c| h1.contains(c.raw_hash.as_str()))
        .count();
    assert_eq!(c2.len(), 6);
    assert_eq!(reused, 5);
}
