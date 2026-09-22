use std::path::Path;
use std::time::Duration;

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

    let stream = tokio_util::io::ReaderStream::new(file);
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
