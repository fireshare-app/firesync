use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;

use serde::Deserialize;

use crate::error::AppError;

/// Uploads get their own budget. A clip on a slow upstream can legitimately take
/// an hour, and the 15 second discovery timeout would kill every one of them.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

/// What the server said, in the terms the queue reasons about.
#[derive(Debug)]
pub enum UploadResult {
    /// 201 — on disk, scanning started.
    Accepted { filename: String, folder: String },
    /// 409 — already in the library. Not a failure; there is nothing to fix and
    /// nothing to retry.
    Duplicate { url: Option<String> },
}

/// Why an upload did not land, split by what the caller should do about it.
#[derive(Debug)]
pub enum UploadError {
    /// The server has given its answer and it will not change.
    Permanent(String),
    /// Worth trying again later.
    Retryable(String),
    /// The server asked for a specific wait — honour it exactly rather than
    /// applying our own backoff on top.
    RetryAfter { message: String, seconds: u64 },
    /// The token is gone. Every queue stops; retrying only trips the throttle.
    Unauthorized(String),
}

/// Bytes handed to the socket so far.
///
/// A counter rather than a callback: the sender only ever adds to it, and
/// whoever wants to report progress reads it on its own schedule. That keeps
/// the reporting cadence out of the transfer path entirely — a 3 GB upload
/// should not be deciding how often the UI redraws.
pub type Progress = Arc<AtomicU64>;

#[derive(Debug, Default)]
pub struct UploadMeta {
    pub folder: Option<String>,
    pub game: Option<String>,
    pub title: Option<String>,
}

#[derive(Deserialize)]
struct DuplicateBody {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
struct AcceptedBody {
    #[serde(default)]
    filename: String,
    #[serde(default)]
    folder: String,
}

/// Send one file to `POST /api/upload/token`.
///
/// The body is streamed from disk rather than read into memory: a 3 GB clip must
/// not become 3 GB of RSS on a machine that is also running a game.
pub async fn upload_single(
    base_url: &str,
    token: &str,
    path: &Path,
    meta: &UploadMeta,
    progress: Progress,
) -> std::result::Result<UploadResult, UploadError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| UploadError::Permanent(format!("Could not read the file: {e}")))?;
    let size = file
        .metadata()
        .await
        .map(|m| m.len())
        .map_err(|e| UploadError::Permanent(format!("Could not read the file: {e}")))?;

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload")
        .to_string();

    let stream = tokio_util::io::ReaderStream::new(file).map(move |result| {
        if let Ok(bytes) = &result {
            progress.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
        result
    });
    let part = reqwest::multipart::Part::stream_with_length(reqwest::Body::wrap_stream(stream), size)
        .file_name(filename.clone())
        .mime_str("application/octet-stream")
        .map_err(|e| UploadError::Permanent(format!("Could not build the upload: {e}")))?;

    let mut form = reqwest::multipart::Form::new().part("file", part);
    if let Some(folder) = meta.folder.as_deref().filter(|s| !s.is_empty()) {
        form = form.text("folder", folder.to_string());
    }
    if let Some(game) = meta.game.as_deref().filter(|s| !s.is_empty()) {
        form = form.text("game", game.to_string());
    }
    if let Some(title) = meta.title.as_deref().filter(|s| !s.is_empty()) {
        form = form.text("title", title.to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(UPLOAD_TIMEOUT)
        .user_agent(concat!("Firesync/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| UploadError::Retryable(format!("Could not start the HTTP client: {e}")))?;

    let response = client
        .post(format!("{base_url}/api/upload/token"))
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| classify_transport(&e))?;

    classify_response(response).await
}

fn classify_transport(err: &reqwest::Error) -> UploadError {
    if err.is_timeout() {
        return UploadError::Retryable("The upload timed out.".into());
    }
    if err.is_connect() {
        return UploadError::Retryable("Could not reach the server.".into());
    }
    if err.is_body() || err.is_request() {
        return UploadError::Retryable(format!("The connection dropped mid-upload: {err}"));
    }
    UploadError::Retryable(format!("Network error: {err}"))
}

/// The response table from the upload-token API, turned into a disposition.
///
/// The split that matters is permanent versus retryable. Several of these will
/// never succeed no matter how long you wait, and retrying them looks to the
/// person like an app that has hung rather than one that has an answer.
pub async fn classify_response(
    response: reqwest::Response,
) -> std::result::Result<UploadResult, UploadError> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    match status.as_u16() {
        201 => {
            let body = response.json::<AcceptedBody>().await.unwrap_or(AcceptedBody {
                filename: String::new(),
                folder: String::new(),
            });
            Ok(UploadResult::Accepted { filename: body.filename, folder: body.folder })
        }

        // Already in the library. The file is fine, the library is fine, and
        // there is nothing for the person to do.
        409 => {
            let body = response.json::<DuplicateBody>().await.ok();
            Ok(UploadResult::Duplicate { url: body.and_then(|b| b.url) })
        }

        // The account behind the token lost upload rights, or the token was
        // deleted or regenerated. Hammering it only trips the failed-token
        // throttle and locks this address out for five minutes.
        401 => Err(UploadError::Unauthorized(
            "The upload token was rejected. Uploads are paused until it is replaced.".into(),
        )),

        // An unknown game name, or an extension the server will not take. Both
        // are configuration, and both stay wrong until somebody changes them.
        400 => {
            let detail = response.text().await.unwrap_or_default();
            Err(UploadError::Permanent(friendly_400(&detail)))
        }

        413 => Err(UploadError::Permanent(
            "The server refused the file as too large.".into(),
        )),

        429 => Err(UploadError::RetryAfter {
            message: "The server is rate limiting this machine.".into(),
            seconds: retry_after.unwrap_or(60),
        }),

        503 => Err(UploadError::Permanent(
            "Images are not enabled on this Fireshare instance.".into(),
        )),

        s if (500..600).contains(&s) => {
            Err(UploadError::Retryable(format!("The server answered {s}.")))
        }

        s => Err(UploadError::Permanent(format!("The server answered {s}."))),
    }
}

/// A 400 body is either JSON with an `unknown_game` error or a plain sentence.
/// Both should read as something a person can act on.
fn friendly_400(body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) {
        if parsed.get("error").and_then(|v| v.as_str()) == Some("unknown_game") {
            return parsed
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("That game does not exist in this library.")
                .to_string();
        }
    }
    if body.trim().is_empty() {
        "The server rejected the file.".to_string()
    } else {
        body.trim().to_string()
    }
}

impl From<UploadError> for AppError {
    fn from(e: UploadError) -> AppError {
        match e {
            UploadError::Unauthorized(m) => AppError::TokenRejected(m),
            UploadError::RetryAfter { message, .. } => AppError::Throttled(message),
            UploadError::Permanent(m) | UploadError::Retryable(m) => AppError::Server(m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_game_reads_as_something_to_fix() {
        let body = r#"{"error":"unknown_game","message":"No game named \"Apex Legends\" exists in this library. Add it first, or pass game_id."}"#;
        assert!(friendly_400(body).starts_with("No game named"));
    }

    #[test]
    fn a_plain_400_body_survives_intact() {
        assert_eq!(friendly_400("Unsupported file type."), "Unsupported file type.");
    }

    #[test]
    fn an_empty_400_still_says_something() {
        assert_eq!(friendly_400("   "), "The server rejected the file.");
    }
}

// ---------------------------------------------------------------------------
// Chunked upload
// ---------------------------------------------------------------------------

/// Above this, send in pieces. Below it, one request is fewer moving parts than
/// several and finishes in about the same time.
pub const CHUNK_THRESHOLD: u64 = 200 * 1024 * 1024;

/// The browser client uses 90 MB. On a home upstream that is a lot to lose to
/// one dropped connection, and the server's ceiling of 20000 chunks leaves
/// plenty of headroom at this size: 20000 x 32 MiB is 625 GiB, far past any
/// clip anyone is going to record.
pub const CHUNK_SIZE: u64 = 32 * 1024 * 1024;

/// The sizes that decide whether and how a file is split.
///
/// A struct rather than two constants so a test can exercise the real chunking
/// path with kilobytes instead of having to write out a 200 MB file to reach it.
#[derive(Debug, Clone, Copy)]
pub struct ChunkPolicy {
    pub threshold: u64,
    pub size: u64,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self { threshold: CHUNK_THRESHOLD, size: CHUNK_SIZE }
    }
}

#[derive(Debug)]
pub enum ChunkOutcome {
    /// The set completed and the server answered as the single-shot route does.
    Complete(UploadResult),
    /// Still assembling. `received` is how many parts the server actually holds,
    /// which is not the same as how many we have sent — see the queue.
    Partial { received: i64 },
}

#[derive(Deserialize)]
struct PartialBody {
    #[serde(default)]
    received: i64,
}

/// Send one chunk of a file.
///
/// Every chunk carries the full metadata, and it must: the server rebuilds its
/// plan from each request, and derives the directory the parts are written into
/// from `folder`. A chunk that named a different folder — or omitted it, and so
/// landed in the default — would leave its part somewhere the completing
/// request never looks, and the upload would sit at 202 forever.
#[allow(clippy::too_many_arguments)]
pub async fn upload_chunk(
    base_url: &str,
    token: &str,
    path: &Path,
    meta: &UploadMeta,
    check_sum: &str,
    index: i64,
    total: i64,
    file_size: u64,
    chunk_size: u64,
    progress: Progress,
) -> std::result::Result<ChunkOutcome, UploadError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let offset = (index as u64 - 1) * chunk_size;
    let len = chunk_size.min(file_size.saturating_sub(offset));
    if len == 0 {
        return Err(UploadError::Permanent(
            "Worked out a chunk with no bytes in it.".into(),
        ));
    }

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| UploadError::Permanent(format!("Could not read the file: {e}")))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|e| UploadError::Permanent(format!("Could not seek the file: {e}")))?;

    // Streamed and length-limited rather than read into a buffer: the chunk is
    // 32 MB and there is no reason for it to also be 32 MB of memory.
    let slice = file.take(len);
    let counted = tokio_util::io::ReaderStream::new(slice).map(move |result| {
        if let Ok(bytes) = &result {
            progress.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
        result
    });
    let part = reqwest::multipart::Part::stream_with_length(
        reqwest::Body::wrap_stream(counted),
        len,
    )
    .file_name("blob")
    .mime_str("application/octet-stream")
    .map_err(|e| UploadError::Permanent(format!("Could not build the chunk: {e}")))?;

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload")
        .to_string();

    let mut form = reqwest::multipart::Form::new()
        .part("blob", part)
        .text("chunkPart", index.to_string())
        .text("totalChunks", total.to_string())
        .text("checkSum", check_sum.to_string())
        .text("fileName", filename)
        .text("fileSize", file_size.to_string());

    if let Some(folder) = meta.folder.as_deref().filter(|s| !s.is_empty()) {
        form = form.text("folder", folder.to_string());
    }
    if let Some(game) = meta.game.as_deref().filter(|s| !s.is_empty()) {
        form = form.text("game", game.to_string());
    }
    if let Some(title) = meta.title.as_deref().filter(|s| !s.is_empty()) {
        form = form.text("title", title.to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(UPLOAD_TIMEOUT)
        .user_agent(concat!("Firesync/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| UploadError::Retryable(format!("Could not start the HTTP client: {e}")))?;

    let response = client
        .post(format!("{base_url}/api/upload/token/chunked"))
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| classify_transport(&e))?;

    if response.status().as_u16() == 202 {
        let received = response
            .json::<PartialBody>()
            .await
            .map(|b| b.received)
            .unwrap_or(-1);
        return Ok(ChunkOutcome::Partial { received });
    }

    classify_response(response).await.map(ChunkOutcome::Complete)
}

/// How many chunks a file of this size takes.
pub fn chunk_count(file_size: u64, chunk_size: u64) -> i64 {
    file_size.div_ceil(chunk_size).max(1) as i64
}

#[cfg(test)]
mod chunk_tests {
    use super::*;

    #[test]
    fn chunk_counts_round_up() {
        assert_eq!(chunk_count(1, CHUNK_SIZE), 1);
        assert_eq!(chunk_count(CHUNK_SIZE, CHUNK_SIZE), 1);
        assert_eq!(chunk_count(CHUNK_SIZE + 1, CHUNK_SIZE), 2);
        assert_eq!(chunk_count(CHUNK_SIZE * 3, CHUNK_SIZE), 3);
        // A 3 GB clip, comfortably inside the server's 20000 ceiling.
        assert_eq!(chunk_count(3 * 1024 * 1024 * 1024, CHUNK_SIZE), 96);
    }

    #[test]
    fn a_zero_length_file_still_claims_one_chunk() {
        assert_eq!(chunk_count(0, CHUNK_SIZE), 1);
    }

    #[test]
    fn the_shipped_policy_splits_where_it_says_it_does() {
        let p = ChunkPolicy::default();
        assert_eq!(p.threshold, 200 * 1024 * 1024);
        assert_eq!(p.size, 32 * 1024 * 1024);
        // 20000 parts is the server's ceiling, which at this chunk size is a
        // 625 GiB file exactly. Worth pinning: raising the chunk count or
        // shrinking the chunk would quietly lower the largest file that can be
        // sent at all.
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(chunk_count(625 * GIB, p.size), 20000);
        assert!(chunk_count(626 * GIB, p.size) > 20000);
    }
}
