use std::fs;

use serde_json::json;
use tempfile::tempdir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use arcane_cli::api::ApiClient;
use arcane_cli::config::{AuthSource, ResolvedAuth};
use arcane_cli::fastcdc::{ChunkingMode, DEFAULT_FIXED_CHUNK_SIZE};
use arcane_cli::upload::upload_all_cas;
use arcane_cli::walker::list_files;

#[tokio::test]
async fn cas_upload_deduplicates_compresses_and_commits() {
    let server = MockServer::start().await;
    let session_route = "/titles/00000000-0000-0000-0000-000000000001/builds/upload-sessions/00000000-0000-0000-0000-000000000002";

    let dir = tempdir().unwrap();
    // File A: highly compressible (will be zstd-compressed)
    fs::write(dir.path().join("a.bin"), vec![b'A'; 128 * 1024]).unwrap();
    // File B: identical content to File A (deduplicated locally into 1 unique chunk)
    fs::write(dir.path().join("b.bin"), vec![b'A'; 128 * 1024]).unwrap();
    // File C: different content (simulated as already present on Arcane)
    let c_bytes = vec![b'C'; 64 * 1024];
    fs::write(dir.path().join("c.bin"), &c_bytes).unwrap();
    let c_hash = arcane_cli::fastcdc::sha256_hex(&c_bytes);

    let files = list_files(dir.path(), &[]).unwrap();
    assert_eq!(files.len(), 3);

    let server_uri = server.uri();
    let c_hash_clone = c_hash.clone();

    // Mock /chunks/plan:
    // - First call (discovery, no stored_hash): marks c.bin as present, a.bin as missing
    // - Second call (presign, has stored_hash): returns presigned PUT URL to mock server
    Mock::given(method("POST"))
        .and(path(format!("/api/ci{session_route}/chunks/plan")))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = req.body_json().unwrap();
            let chunks = body["chunks"].as_array().unwrap();
            let mut present = Vec::new();
            let mut missing = Vec::new();
            for c in chunks {
                let raw_hash = c["raw_hash"].as_str().unwrap();
                let raw_size = c["raw_size"].as_u64().unwrap();
                if raw_hash == c_hash_clone {
                    present.push(json!({
                        "raw_hash": raw_hash,
                        "raw_size": raw_size,
                        "stored_hash": raw_hash,
                        "stored_size": raw_size,
                        "encoding": "identity",
                    }));
                } else if c.get("stored_hash").is_some() {
                    missing.push(json!({
                        "raw_hash": raw_hash,
                        "raw_size": raw_size,
                        "upload_url": format!("{server_uri}/r2/put/{raw_hash}"),
                        "headers": { "x-amz-test": "1" },
                    }));
                } else {
                    missing.push(json!({
                        "raw_hash": raw_hash,
                        "raw_size": raw_size,
                    }));
                }
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "present": present,
                "missing": missing,
            }))
        })
        .expect(2)
        .mount(&server)
        .await;

    // Mock presigned R2 PUT
    Mock::given(method("PUT"))
        .and(header("x-amz-test", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    // Mock /chunks/commit
    Mock::given(method("POST"))
        .and(path(format!("/api/ci{session_route}/chunks/commit")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "committed": 1 })))
        .expect(1)
        .mount(&server)
        .await;

    // Mock /files/cas (3 files recorded)
    Mock::given(method("POST"))
        .and(path(format!("/api/ci{session_route}/files/cas")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(3)
        .mount(&server)
        .await;

    let auth = ResolvedAuth {
        api_key: "test-key".to_string(),
        portal_url: server.uri(),
        api_key_source: AuthSource::Env,
        portal_url_source: AuthSource::Env,
    };
    let api = ApiClient::new(&auth).unwrap();

    let stats = upload_all_cas(
        &api,
        session_route,
        &files,
        DEFAULT_FIXED_CHUNK_SIZE,
        true,
        ChunkingMode::Cdc,
        4,
        false,
        false,
        true,
    )
    .await
    .unwrap();

    assert_eq!(stats.total_files, 3);
    assert_eq!(stats.total_unique_chunks, 2);
    assert_eq!(stats.reused_chunks, 1);
    assert_eq!(stats.uploaded_chunks, 1);
    assert!(stats.uploaded_stored_bytes < stats.uploaded_raw_bytes);
}
