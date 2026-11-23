use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use arcane_cli::api::ApiClient;
use arcane_cli::config::{AuthSource, ResolvedAuth};

#[tokio::test]
async fn phase2_builds_channels_rollback_pin_and_delete() {
    let server = MockServer::start().await;
    let title_id = "00000000-0000-0000-0000-000000000010";
    let build_id = "00000000-0000-0000-0000-000000000020";

    // GET /builds
    Mock::given(method("GET"))
        .and(path(format!("/api/ci/titles/{title_id}/builds")))
        .and(header("authorization", "Bearer ci-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "builds": [
                {
                    "id": build_id,
                    "version": "1.4.2",
                    "platform": "windows",
                    "status": "published",
                    "total_bytes": 524288000,
                    "chunk_count": 250,
                    "created_at": "2026-11-20T12:00:00Z",
                    "pinned": false
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    // GET /builds/storage
    Mock::given(method("GET"))
        .and(path(format!("/api/ci/titles/{title_id}/builds/storage")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "unique_blobs": 250,
            "deduped_stored_bytes": 310000000,
            "deduped_raw_bytes": 524288000,
            "builds": [
                {
                    "build_id": build_id,
                    "version": "1.4.2",
                    "platform": "windows",
                    "status": "published",
                    "pinned": false,
                    "total_bytes": 524288000,
                    "stored_bytes": 310000000,
                    "reclaimable_bytes": 12000000,
                    "blob_count": 250
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    // PUT /builds/channels/beta
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/ci/titles/{title_id}/builds/channels/beta"
        )))
        .and(body_json(json!({ "build_id": build_id })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "channel": "beta",
            "platform": "windows",
            "is_demo": false,
            "build_id": build_id,
            "updated_at": "2026-11-23T12:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;

    // POST /builds/versions/1.4.1/rollback
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/ci/titles/{title_id}/builds/versions/1.4.1/rollback"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "builds": [
                {
                    "id": build_id,
                    "version": "1.4.1",
                    "platform": "windows",
                    "status": "published"
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    // PATCH /builds/:buildId (pin)
    Mock::given(method("PATCH"))
        .and(path(format!("/api/ci/titles/{title_id}/builds/{build_id}")))
        .and(body_json(json!({ "pinned": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": build_id,
            "version": "1.4.2",
            "platform": "windows",
            "status": "published",
            "pinned": true
        })))
        .expect(1)
        .mount(&server)
        .await;

    // DELETE /builds/:buildId
    Mock::given(method("DELETE"))
        .and(path(format!("/api/ci/titles/{title_id}/builds/{build_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "deleted": true })))
        .expect(1)
        .mount(&server)
        .await;

    let auth = ResolvedAuth {
        api_key: "ci-key".to_string(),
        portal_url: server.uri(),
        api_key_source: AuthSource::Env,
        portal_url_source: AuthSource::Env,
    };
    let api = ApiClient::new(&auth).unwrap();

    let builds = api.list_builds(title_id).await.unwrap();
    assert_eq!(builds.len(), 1);
    assert_eq!(builds[0].version, "1.4.2");

    let storage = api.get_build_storage(title_id).await.unwrap();
    assert_eq!(storage.unique_blobs, 250);
    assert_eq!(storage.deduped_stored_bytes, 310_000_000);

    let act = api
        .set_build_channel(title_id, "beta", build_id)
        .await
        .unwrap();
    assert_eq!(act.channel, "beta");
    assert_eq!(act.build_id, build_id);

    let rolled = api.rollback_version(title_id, "1.4.1").await.unwrap();
    assert_eq!(rolled.len(), 1);
    assert_eq!(rolled[0].version, "1.4.1");

    let pinned = api
        .set_build_pinned(title_id, build_id, true)
        .await
        .unwrap();
    assert!(pinned.pinned);

    let deleted = api.delete_build(title_id, build_id).await.unwrap();
    assert_eq!(deleted["deleted"], json!(true));
}
