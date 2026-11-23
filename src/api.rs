use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::compress::ChunkEncoding;
use crate::config::ResolvedAuth;
use crate::error::{CliError, Result};

const MAX_ATTEMPTS: u32 = 4;

#[derive(Clone)]
pub struct ApiClient {
    http: Client,
    base_url: String,
    api_key: String,
    user_agent: String,
}

impl ApiClient {
    pub fn new(auth: &ResolvedAuth) -> Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(300))
            .pool_max_idle_per_host(16)
            .build()?;

        Ok(Self {
            http,
            base_url: auth.ci_base_url(),
            api_key: auth.api_key.clone(),
            user_agent: format!("arcane-cli/{}", env!("CARGO_PKG_VERSION")),
        })
    }

    /// Low-level JSON request with exponential backoff retries on 5xx / network errors.
    pub async fn request_json<B, R>(
        &self,
        method: Method,
        route: &str,
        body: Option<&B>,
    ) -> Result<R>
    where
        B: Serialize + ?Sized,
        R: for<'de> Deserialize<'de>,
    {
        let value = self.request_raw(method, route, body).await?;
        serde_json::from_value(value).map_err(CliError::from)
    }

    pub async fn request_raw<B>(
        &self,
        method: Method,
        route: &str,
        body: Option<&B>,
    ) -> Result<Value>
    where
        B: Serialize + ?Sized,
    {
        let url = format!("{}{}", self.base_url, route);
        let serialized = match body {
            Some(b) => Some(serde_json::to_vec(b)?),
            None => None,
        };

        let mut last_err: Option<CliError> = None;

        for attempt in 1..=MAX_ATTEMPTS {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
                .header(USER_AGENT, &self.user_agent);

            if let Some(bytes) = &serialized {
                req = req
                    .header(CONTENT_TYPE, "application/json")
                    .body(bytes.clone());
            }

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    let payload: Value = if text.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_str(&text)
                            .unwrap_or_else(|_| serde_json::json!({ "error": text.chars().take(500).collect::<String>() }))
                    };

                    if status.is_success() {
                        return Ok(payload);
                    }

                    let err = describe_api_error(status, &payload);
                    if (status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS)
                        && attempt < MAX_ATTEMPTS
                    {
                        last_err = Some(err);
                        tokio::time::sleep(Duration::from_millis(1000 * (1u64 << (attempt - 1))))
                            .await;
                        continue;
                    }
                    return Err(err);
                }
                Err(err) => {
                    last_err = Some(CliError::msg(format!("Could not reach the portal: {err}")));
                    if attempt < MAX_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(1000 * (1u64 << (attempt - 1))))
                            .await;
                        continue;
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| CliError::msg("Could not reach the portal")))
    }

    /// Multipart file upload (used for web / android builds) with retries.
    pub async fn upload_multipart_file(
        &self,
        route: &str,
        relative_path: &str,
        file_name: &str,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let url = format!("{}{}", self.base_url, route);
        let mut last_err: Option<CliError> = None;

        for attempt in 1..=MAX_ATTEMPTS {
            let file_part =
                reqwest::multipart::Part::bytes(bytes.clone()).file_name(file_name.to_string());
            let form = reqwest::multipart::Form::new()
                .text("relative_path", relative_path.to_string())
                .part("file", file_part);

            let req = self
                .http
                .post(&url)
                .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
                .header(USER_AGENT, &self.user_agent)
                .multipart(form);

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(());
                    }
                    let text = resp.text().await.unwrap_or_default();
                    let payload: Value = serde_json::from_str(&text)
                        .unwrap_or_else(|_| serde_json::json!({ "error": text }));
                    let err = describe_api_error(status, &payload);
                    if (status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS)
                        && attempt < MAX_ATTEMPTS
                    {
                        last_err = Some(err);
                        tokio::time::sleep(Duration::from_millis(1000 * (1u64 << (attempt - 1))))
                            .await;
                        continue;
                    }
                    return Err(err);
                }
                Err(err) => {
                    last_err = Some(CliError::msg(format!("Multipart upload failed: {err}")));
                    if attempt < MAX_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(1000 * (1u64 << (attempt - 1))))
                            .await;
                        continue;
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| CliError::msg("Multipart upload failed")))
    }

    /// Direct presigned R2 PUT with retries & exponential backoff.
    pub async fn put_presigned(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        body: Vec<u8>,
    ) -> Result<()> {
        let mut header_map = HeaderMap::new();
        for (k, v) in headers {
            if let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                header_map.insert(name, val);
            }
        }

        let mut last_err: Option<CliError> = None;

        for attempt in 1..=MAX_ATTEMPTS {
            let req = self
                .http
                .put(url)
                .headers(header_map.clone())
                .body(body.clone());

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(());
                    }
                    let msg = format!("R2 PUT HTTP {}", status.as_u16());
                    if (status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS)
                        && attempt < MAX_ATTEMPTS
                    {
                        last_err = Some(CliError::api(msg, Some(status.as_u16()), None));
                        tokio::time::sleep(Duration::from_millis(800 * (1u64 << (attempt - 1))))
                            .await;
                        continue;
                    }
                    return Err(CliError::api(msg, Some(status.as_u16()), None));
                }
                Err(err) => {
                    last_err = Some(CliError::msg(format!("Chunk upload failed: {err}")));
                    if attempt < MAX_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(800 * (1u64 << (attempt - 1))))
                            .await;
                        continue;
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| CliError::msg("Chunk upload failed")))
    }

    // ---- Typed CI endpoints ----

    pub async fn list_titles(&self) -> Result<Vec<CiTitle>> {
        let resp: TitlesResponse = self
            .request_json(Method::GET, "/titles", None::<&()>)
            .await?;
        Ok(resp.titles)
    }

    pub async fn get_web_limits(&self, title_id: &str) -> Result<WebLimits> {
        self.request_json(
            Method::GET,
            &format!("/titles/{title_id}/builds/web-limits"),
            None::<&()>,
        )
        .await
    }

    pub async fn create_upload_session(&self, title_id: &str) -> Result<UploadSession> {
        self.request_json(
            Method::POST,
            &format!("/titles/{title_id}/builds/upload-sessions"),
            None::<&()>,
        )
        .await
    }

    pub async fn get_upload_session(&self, session_route: &str) -> Result<UploadSessionState> {
        self.request_json(Method::GET, session_route, None::<&()>)
            .await
    }

    pub async fn delete_upload_session(&self, session_route: &str) -> Result<()> {
        let _ = self
            .request_raw(Method::DELETE, session_route, None::<&()>)
            .await?;
        Ok(())
    }

    pub async fn plan_chunks(
        &self,
        session_route: &str,
        chunks: &[PlanChunkRequest],
    ) -> Result<PlanChunksResponse> {
        let body = serde_json::json!({ "chunks": chunks });
        self.request_json(
            Method::POST,
            &format!("{session_route}/chunks/plan"),
            Some(&body),
        )
        .await
    }

    pub async fn commit_chunks(
        &self,
        session_route: &str,
        blobs: &[CasBlobDescriptor],
    ) -> Result<()> {
        let body = serde_json::json!({ "blobs": blobs });
        let _ = self
            .request_raw(
                Method::POST,
                &format!("{session_route}/chunks/commit"),
                Some(&body),
            )
            .await?;
        Ok(())
    }

    pub async fn record_cas_file(
        &self,
        session_route: &str,
        req: &RecordCasFileRequest,
    ) -> Result<()> {
        let _ = self
            .request_raw(
                Method::POST,
                &format!("{session_route}/files/cas"),
                Some(req),
            )
            .await?;
        Ok(())
    }

    pub async fn list_publish_jobs(&self, title_id: &str) -> Result<Vec<PublishJobListItem>> {
        let resp: PublishJobsResponse = self
            .request_json(
                Method::GET,
                &format!("/titles/{title_id}/builds/publish-jobs"),
                None::<&()>,
            )
            .await?;
        Ok(resp.jobs)
    }

    pub async fn publish_and_wait<F>(
        &self,
        session_route: &str,
        endpoint: &str,
        payload: &Value,
        mut on_phase: F,
    ) -> Result<Value>
    where
        F: FnMut(&str),
    {
        let mut job = self
            .request_raw(
                Method::POST,
                &format!("{session_route}/{endpoint}"),
                Some(payload),
            )
            .await?;
        let mut last_phase = String::new();

        while job.get("status").and_then(Value::as_str) == Some("running") {
            if let Some(phase) = job.get("phase").and_then(Value::as_str) {
                if phase != last_phase {
                    last_phase = phase.to_string();
                    on_phase(phase);
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            match self
                .request_raw(
                    Method::GET,
                    &format!("{session_route}/publish-status"),
                    None::<&()>,
                )
                .await
            {
                Ok(next) => job = next,
                Err(err) if err.status() == Some(404) => {
                    return Err(CliError::msg(
                        "The publish was interrupted on Arcane's side before it finished. Nothing was published: run the command again.",
                    ));
                }
                Err(err) => return Err(err),
            }
        }

        match job.get("status").and_then(Value::as_str) {
            Some("error") => {
                let msg = job
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("The publish failed");
                let code = job.get("code").and_then(Value::as_str).map(String::from);
                Err(CliError::api(msg, None, code))
            }
            Some("done") => Ok(job.get("result").cloned().unwrap_or(job)),
            _ => Ok(job),
        }
    }
}

fn describe_api_error(status: StatusCode, payload: &Value) -> CliError {
    let base = payload
        .get("error")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
    let code = payload
        .get("code")
        .and_then(Value::as_str)
        .map(String::from);
    let code_suffix = code
        .as_deref()
        .map(|c| format!(" ({c})"))
        .unwrap_or_default();
    let extra = match (payload.get("limit"), payload.get("max")) {
        (Some(l), Some(m)) => format!(" — limit {l}, max {m}"),
        _ => String::new(),
    };
    CliError::api(
        format!("{base}{code_suffix}{extra}"),
        Some(status.as_u16()),
        code,
    )
}

// ---- Wire DTOs ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitlesResponse {
    pub titles: Vec<CiTitle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiTitle {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub parent_title_id: Option<String>,
    pub linked: bool,
    pub web_builds: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebLimits {
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_file_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadSession {
    pub session_id: String,
    #[serde(default)]
    pub chunk_size: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadSessionState {
    #[serde(default)]
    pub files: Vec<SessionFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFile {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanChunkRequest {
    pub raw_hash: String,
    pub raw_size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<ChunkEncoding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanChunksResponse {
    #[serde(default)]
    pub present: Vec<PresentChunk>,
    #[serde(default)]
    pub missing: Vec<MissingChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresentChunk {
    pub raw_hash: String,
    pub raw_size: u64,
    pub stored_hash: String,
    pub stored_size: u64,
    pub encoding: ChunkEncoding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingChunk {
    pub raw_hash: String,
    pub raw_size: u64,
    #[serde(default)]
    pub upload_url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasBlobDescriptor {
    pub stored_hash: String,
    pub raw_hash: String,
    pub raw_size: u64,
    pub stored_size: u64,
    pub encoding: ChunkEncoding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordCasFileRequest {
    pub path: String,
    pub size: u64,
    pub chunks: Vec<CasFileChunkEntry>,
    pub all_reused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasFileChunkEntry {
    pub hash: String,
    pub raw_hash: String,
    pub raw_size: u64,
    pub stored_size: u64,
    pub encoding: ChunkEncoding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishJobsResponse {
    pub jobs: Vec<PublishJobListItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishJobListItem {
    pub session_id: String,
    pub kind: String,
    pub version: String,
    pub platform: Option<String>,
    pub status: String,
    pub phase: String,
    pub error: Option<String>,
    pub code: Option<String>,
    pub build_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}
