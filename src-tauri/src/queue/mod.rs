pub mod retry;
pub mod rules;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::api::upload::{
    chunk_count, upload_chunk, upload_single, ChunkOutcome, ChunkPolicy, Progress, UploadError,
    UploadMeta, UploadResult,
};
use crate::api::discovery::FolderRules;
use crate::config::{AfterUpload, MediaKind, Settings};
use crate::ledger::{Claim, Ledger};
use crate::secrets::TokenCache;

/// How long the loop waits when there is nothing due. Short enough that a clip
/// finishing feels immediate, long enough to be free when idle.
const IDLE_POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadEvent {
    pub id: i64,
    pub path: String,
    pub size: i64,
    pub state: String,
    pub reason: Option<String>,
    pub url: Option<String>,
    /// Bytes handed to the socket so far, on an `uploading` event.
    pub sent: i64,
    /// Where the server filed it, which is not always where we asked: it
    /// suffixes the name when something is already sitting on it.
    pub landed_as: Option<String>,
    /// Set when the local copy was removed afterwards, so the UI can say so
    /// rather than leaving somebody to notice their folder emptying by itself.
    pub removed_local: Option<String>,
    /// How fast this file is currently going up, on an `uploading` event.
    ///
    /// Measured here rather than in the webview because two views show it and
    /// both should agree, and because the raw figure needs smoothing before it
    /// is fit to read.
    pub bytes_per_second: Option<i64>,
}

/// Shared run/stop control for the whole queue.
///
/// A rejected token pauses everything rather than failing file by file: the
/// account behind it has lost upload rights or the token is gone, so every
/// subsequent request would fail identically — and the server's failed-token
/// throttle would lock this address out for five minutes as a reward.
pub struct QueueControl {
    paused: AtomicBool,
    pause_reason: Mutex<Option<String>>,
}

impl QueueControl {
    pub fn new() -> Self {
        Self { paused: AtomicBool::new(false), pause_reason: Mutex::new(None) }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn pause(&self, reason: Option<String>) {
        *self.pause_reason.lock().expect("pause reason mutex") = reason;
        self.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        *self.pause_reason.lock().expect("pause reason mutex") = None;
        self.paused.store(false, Ordering::Relaxed);
    }

    pub fn reason(&self) -> Option<String> {
        self.pause_reason.lock().expect("pause reason mutex").clone()
    }
}

impl Default for QueueControl {
    fn default() -> Self {
        Self::new()
    }
}

pub struct QueueDeps<F: Fn(UploadEvent) + Send + Sync + 'static> {
    pub ledger: Arc<Ledger>,
    pub settings: Arc<Mutex<Settings>>,
    pub control: Arc<QueueControl>,
    pub token: Arc<TokenCache>,
    pub folder_rules: Arc<Mutex<FolderRules>>,
    pub types: Arc<Mutex<rules::SupportedTypes>>,
    pub on_event: Arc<F>,
}

/// Run the upload queue until the app exits.
pub fn spawn<F>(deps: QueueDeps<F>)
where
    F: Fn(UploadEvent) + Send + Sync + 'static,
{
    // Anything left mid-upload was interrupted by a quit or a crash, not by a
    // decision, so it goes back in the queue rather than sitting as a lie.
    if let Err(e) = deps.ledger.requeue_interrupted() {
        eprintln!("firesync: could not requeue interrupted uploads: {e}");
    }

    tauri::async_runtime::spawn(async move {
        loop {
            if deps.control.is_paused() {
                tokio::time::sleep(IDLE_POLL).await;
                continue;
            }

            let (base_url, concurrency, paused_folders) = {
                let settings = deps.settings.lock().expect("settings mutex");
                let paused: Vec<String> = settings
                    .folders
                    .iter()
                    .filter(|f| !f.enabled)
                    .map(|f| f.id.clone())
                    .collect();
                (
                    settings.server_url.clone(),
                    settings.transfers.max_concurrent.max(1) as i64,
                    paused,
                )
            };

            let Some(base_url) = base_url else {
                tokio::time::sleep(IDLE_POLL).await;
                continue;
            };
            // From memory. This loop runs every couple of seconds; asking the
            // OS credential store each time is what made macOS prompt for a
            // password on repeat.
            let Some(token) = deps.token.get() else {
                tokio::time::sleep(IDLE_POLL).await;
                continue;
            };

            let claims = match deps.ledger.claim(concurrency, &paused_folders) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("firesync: could not claim uploads: {e}");
                    tokio::time::sleep(IDLE_POLL).await;
                    continue;
                }
            };

            if claims.is_empty() {
                tokio::time::sleep(IDLE_POLL).await;
                continue;
            }

            let mut tasks = Vec::new();
            for claim in claims {
                let ledger = deps.ledger.clone();
                let settings = deps.settings.clone();
                let folder_rules = deps.folder_rules.clone();
                let control = deps.control.clone();
                let on_event = deps.on_event.clone();
                let base_url = base_url.clone();
                let token = token.clone();
                let types = deps.types.clone();

                tasks.push(tauri::async_runtime::spawn(async move {
                    run_one(
                        claim,
                        &base_url,
                        &token,
                        ledger,
                        settings,
                        control,
                        on_event,
                        ChunkPolicy::default(),
                        folder_rules,
                        types,
                    )
                    .await;
                }));
            }
            for task in tasks {
                let _ = task.await;
            }
        }
    });
}

/// Where an upload goes, and what it says about itself.
///
/// With auto-sort on, the file is filed in whichever folder Fireshare already
/// associates with this folder's game, and the game is *not* sent: the server
/// tags anything scanned there with that folder's game anyway, so naming it
/// again adds nothing and reintroduces the one failure a name can cause — a
/// game the library does not have, which fails every upload from the folder
/// until somebody notices.
///
/// When the game has no folder of its own, the explicit destination is used and
/// the game name is sent as before. A guess would be worse than the choice
/// somebody already made.
fn destination_for(
    folder: &crate::config::WatchedFolder,
    rules: &FolderRules,
    path: &std::path::Path,
) -> UploadMeta {
    if folder.auto_sort_by_game {
        if let Some(game) = folder.game.as_deref().filter(|g| !g.trim().is_empty()) {
            let is_image = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    let e = e.to_ascii_lowercase();
                    crate::queue::rules::SupportedTypes::default()
                        .image
                        .iter()
                        .any(|t| *t == e)
                })
                .unwrap_or(false)
                || (folder.media.contains(&MediaKind::Image)
                    && !folder.media.contains(&MediaKind::Video));

            if let Some(sorted) = rules.folder_for(game, is_image) {
                return UploadMeta { folder: Some(sorted), game: None, title: None };
            }
        }
    }

    UploadMeta {
        folder: folder.dest_folder.clone(),
        game: folder.game.clone(),
        title: None,
    }
}

/// What sending a file came to. `Restart` is the chunked path's own case: the
/// parts are gone from the server and there is nothing left to resume from, so
/// the file has to begin again under a fresh id.
enum SendOutcome {
    Ok(UploadResult),
    Err(UploadError),
    Restart(String),
}

/// Send a large file in pieces, picking up wherever the last attempt stopped.
///
/// Chunks go one at a time. The server reassembles as soon as it sees a full
/// set, so two requests that both observe one would both try — and the one that
/// loses finds the parts already consumed and answers 500. Sending several
/// *files* at once is fine; it is within a file that order matters.
async fn send_chunked(
    claim: &Claim,
    base_url: &str,
    token: &str,
    path: &std::path::Path,
    meta: &UploadMeta,
    file_size: u64,
    ledger: &Arc<Ledger>,
    policy: ChunkPolicy,
    progress: Progress,
) -> SendOutcome {
    let total = chunk_count(file_size, policy.size);
    let state = ledger.chunk_state(claim.id).unwrap_or_default();

    // Resume only onto a set that describes this same file. A clip re-recorded
    // under the same name has a different length and therefore a different
    // number of chunks, and pouring its bytes into the old set would assemble a
    // file that never existed.
    let (check_sum, mut done) = match (state.check_sum, state.chunks_total) {
        (Some(cs), Some(t)) if t == total && state.chunks_done < total => (cs, state.chunks_done),
        _ => {
            let fresh = uuid::Uuid::new_v4().simple().to_string();
            if let Err(e) = ledger.begin_chunks(claim.id, &fresh, total) {
                return SendOutcome::Err(UploadError::Retryable(format!(
                    "Could not record the upload's progress: {e}"
                )));
            }
            (fresh, 0)
        }
    };

    for index in (done + 1)..=total {
        match upload_chunk(
            base_url,
            token,
            path,
            meta,
            &check_sum,
            index,
            total,
            file_size,
            policy.size,
            progress.clone(),
        )
        .await
        {
            Ok(ChunkOutcome::Complete(result)) => {
                let _ = ledger.clear_chunks(claim.id);
                return SendOutcome::Ok(result);
            }

            Ok(ChunkOutcome::Partial { received }) => {
                // `received` is what the server is holding, which is not the
                // same as what we have sent. Fewer means its parts were swept
                // out from under us — a restart of Fireshare, or a media
                // directory cleared — and every remaining chunk we send would
                // answer 202 forever against a set that can never complete.
                if received >= 0 && received < index {
                    return SendOutcome::Restart(format!(
                        "The server has {received} of the {index} parts sent so far, so the \
                         earlier ones are gone."
                    ));
                }

                done = index;
                let _ = ledger.advance_chunks(claim.id, done);

                // The same conclusion without needing the count, for a Fireshare
                // old enough to answer 202 with an empty body: every chunk sent
                // and still not complete can only mean parts are missing.
                if index == total {
                    return SendOutcome::Restart(
                        "Every part was sent and the server still has not assembled the file."
                            .into(),
                    );
                }
            }

            // A failure on the request that completes the set is not resumable.
            // Reassembly consumes each part as it goes, so whatever it got
            // through is already gone, and a size mismatch deletes the staged
            // file too. There is nothing left to continue from.
            Err(UploadError::Retryable(message)) if index == total => {
                return SendOutcome::Restart(format!("{message} The assembled parts are gone."));
            }

            Err(e) => return SendOutcome::Err(e),
        }
    }

    SendOutcome::Restart("Ran out of parts to send without the file completing.".into())
}

async fn run_one<F>(
    claim: Claim,
    base_url: &str,
    token: &str,
    ledger: Arc<Ledger>,
    settings: Arc<Mutex<Settings>>,
    control: Arc<QueueControl>,
    on_event: Arc<F>,
    policy: ChunkPolicy,
    folder_rules: Arc<Mutex<FolderRules>>,
    types: Arc<Mutex<rules::SupportedTypes>>,
) where
    F: Fn(UploadEvent) + Send + Sync + 'static,
{
    let path = std::path::PathBuf::from(&claim.path);

    // The file may have been moved or deleted between being queued and being
    // sent. That is not a failure worth retrying.
    if !path.is_file() {
        let _ = ledger.mark_failed(claim.id, "The file is no longer on disk.");
        emit(&on_event, &claim, "failed", Some("The file is no longer on disk."), None);
        return;
    }

    let (meta, after_upload) = {
        let settings = settings.lock().expect("settings mutex");
        match settings.folders.iter().find(|f| f.id == claim.folder_id) {
            Some(folder) => (
                destination_for(folder, &folder_rules.lock().expect("folder rules mutex"), &path),
                folder.after_upload,
            ),
            // The folder was removed while this file was in flight.
            None => {
                let _ = ledger.release(claim.id);
                return;
            }
        }
    };

    let file_size = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);

    // Fireshare's id for these bytes, worked out before a single one is sent.
    // The order matters twice over: it is what lets us ask whether the server
    // already has this file, and "remove after upload" trashes the local copy
    // the moment the server confirms, so anything computed from its contents
    // has to already exist by then. Reading the 16 MB header is cheap next to
    // the upload that follows.
    let hash = match claim.content_hash.clone() {
        Some(hash) => Some(hash),
        None => {
            let for_hash = path.clone();
            match tokio::task::spawn_blocking(move || crate::api::identity::video_id(&for_hash))
                .await
            {
                Ok(Ok(hash)) => {
                    let _ = ledger.record_hash(claim.id, &hash);
                    Some(hash)
                }
                _ => None,
            }
        }
    };

    // Ask before sending. A video would otherwise cross the network in full
    // before its 409 came back, and an image is never rejected at all — the
    // server accepts it and folds it into the row it already has, so without
    // asking there is no way to find out. Any failure here means "not known to
    // be present" and falls through to the upload: a server too old to have the
    // route must not stop anything being sent.
    if let Some(hash) = hash.as_deref() {
        let viewer = types.lock().expect("types mutex").viewer_for(&path);
        if let Some(viewer) = viewer {
            if let Ok(answer) =
                crate::api::discovery::media_exists(base_url, token, hash, viewer).await
            {
                if answer.exists {
                    let _ = ledger.mark_duplicate(claim.id, answer.url.as_deref());
                    let removed = reclaim_space(&path, after_upload).await;
                    on_event(UploadEvent {
                        id: claim.id,
                        path: claim.path.clone(),
                        size: claim.size,
                        sent: claim.size,
                        state: "duplicate".into(),
                        reason: Some("Already in your library".into()),
                        url: answer.url,
                        landed_as: None,
                        removed_local: removed,
                        bytes_per_second: None,
                    });
                    return;
                }
            }
        }
    }

    // Reported from the side rather than from inside the transfer: the sender
    // only adds to this counter, and a ticker reads it on its own schedule. A
    // three gigabyte upload should not be deciding how often the UI redraws,
    // and a small one should not be paying for progress it does not need.
    let progress: Progress = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let ticker = {
        let progress = progress.clone();
        let on_event = on_event.clone();
        let path_str = claim.path.clone();
        let id = claim.id;
        let size = claim.size;
        tauri::async_runtime::spawn(async move {
            let mut last_sent = 0u64;
            let mut last_at = std::time::Instant::now();
            // Half a second of a real network is far too noisy to put in front
            // of somebody: a chunk boundary or a stalled window would have the
            // number leaping about. Smoothed towards the latest reading rather
            // than averaged over the whole upload, so it still follows a genuine
            // change in speed within a second or two.
            let mut smoothed: Option<f64> = None;

            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;

                let sent = progress.load(std::sync::atomic::Ordering::Relaxed);
                let now = std::time::Instant::now();
                let elapsed = now.duration_since(last_at).as_secs_f64();
                if elapsed > 0.0 {
                    let rate = sent.saturating_sub(last_sent) as f64 / elapsed;
                    smoothed = Some(match smoothed {
                        Some(previous) => previous * 0.7 + rate * 0.3,
                        None => rate,
                    });
                }
                last_sent = sent;
                last_at = now;

                on_event(UploadEvent {
                    id,
                    path: path_str.clone(),
                    size,
                    sent: sent as i64,
                    state: "uploading".into(),
                    reason: None,
                    url: None,
                    landed_as: None,
                    removed_local: None,
                    bytes_per_second: smoothed.map(|r| r.round() as i64),
                });
            }
        })
    };

    let outcome = if file_size > policy.threshold {
        send_chunked(
            &claim, base_url, token, &path, &meta, file_size, &ledger, policy, progress,
        )
        .await
    } else {
        match upload_single(base_url, token, &path, &meta, progress.clone()).await {
            Ok(r) => SendOutcome::Ok(r),
            // Under the threshold but still refused for its size: something
            // between here and Fireshare caps how big one request may be, and
            // it is lower than we assumed. The same bytes in 32 MiB pieces are
            // each well under any such cap, so send it that way rather than
            // failing a file that the library would have been happy to take.
            Err(UploadError::TooLarge(_)) => {
                // The abandoned attempt already counted its bytes.
                progress.store(0, Ordering::Relaxed);
                send_chunked(
                    &claim, base_url, token, &path, &meta, file_size, &ledger, policy, progress,
                )
                .await
            }
            Err(e) => SendOutcome::Err(e),
        }
    };

    ticker.abort();

    let result = match outcome {
        SendOutcome::Ok(r) => Ok(r),
        SendOutcome::Err(e) => Err(e),
        SendOutcome::Restart(message) => {
            // Start the file again from nothing: a new id, and no progress to
            // resume onto. Counted as an attempt so a server that keeps losing
            // sets cannot hold one file in a loop forever.
            let _ = ledger.clear_chunks(claim.id);
            if retry::should_retry(claim.attempts) {
                let wait = retry::backoff(claim.attempts);
                let reason = retry::waiting_reason(
                    &format!("{message} Starting this file again."),
                    claim.attempts,
                    wait,
                );
                let due = unix_now() + wait.as_secs() as i64;
                let _ = ledger.reschedule(claim.id, &reason, due);
                emit(&on_event, &claim, "waiting", Some(&reason), None);
            } else {
                let reason = format!("{message} Gave up after {} attempts.", retry::MAX_ATTEMPTS);
                let _ = ledger.mark_failed(claim.id, &reason);
                emit(&on_event, &claim, "failed", Some(&reason), None);
            }
            return;
        }
    };

    match result {
        Ok(UploadResult::Accepted { filename, folder }) => {
            let landed = if folder.is_empty() {
                filename.clone()
            } else {
                format!("{folder}/{filename}")
            };
            let _ = ledger.mark_done(claim.id, None);
            let removed = reclaim_space(&path, after_upload).await;
            on_event(UploadEvent {
                id: claim.id,
                path: claim.path.clone(),
                size: claim.size,
                sent: claim.size,
                state: "done".into(),
                reason: None,
                url: None,
                landed_as: Some(landed),
                removed_local: removed,
                bytes_per_second: None,
            });
        }

        // Not a failure. The library already has it, so there is nothing to fix
        // and nothing to try again.
        // The server already has these bytes, which is the same confirmation a
        // 201 gives — so the local copy is just as safe to clear, and a folder
        // being re-scanned after an earlier upload is exactly when it helps.
        Ok(UploadResult::Duplicate { url }) => {
            let _ = ledger.mark_duplicate(claim.id, url.as_deref());
            let removed = reclaim_space(&path, after_upload).await;
            on_event(UploadEvent {
                id: claim.id,
                path: claim.path.clone(),
                size: claim.size,
                sent: claim.size,
                state: "duplicate".into(),
                reason: Some("Already in your library".into()),
                url,
                landed_as: None,
                removed_local: removed,
                bytes_per_second: None,
            });
        }

        Err(UploadError::Unauthorized(message)) => {
            // Put this file back untouched — it did nothing wrong, and charging
            // it an attempt for a credential problem would burn its retries.
            let _ = ledger.release(claim.id);
            control.pause(Some(message.clone()));
            emit(&on_event, &claim, "paused", Some(&message), None);
        }

        // Chunking was the way out of a 413 and it did not help, so the cap is
        // below the chunk size or the refusal was never about size.
        Err(UploadError::TooLarge(message)) => {
            let _ = ledger.mark_failed(claim.id, &message);
            emit(&on_event, &claim, "failed", Some(&message), None);
        }

        Err(UploadError::Permanent(message)) => {
            let _ = ledger.mark_failed(claim.id, &message);
            emit(&on_event, &claim, "failed", Some(&message), None);
        }

        // The server named a wait. Honour it exactly rather than stacking our
        // own backoff on top of a number it already chose.
        Err(UploadError::RetryAfter { message, seconds }) => {
            let due = unix_now() + seconds as i64;
            let reason = format!("{message} Waiting {seconds}s as asked.");
            let _ = ledger.reschedule(claim.id, &reason, due);
            emit(&on_event, &claim, "waiting", Some(&reason), None);
        }

        Err(UploadError::Retryable(message)) => {
            if retry::should_retry(claim.attempts) {
                let wait = retry::backoff(claim.attempts);
                let reason = retry::waiting_reason(&message, claim.attempts, wait);
                let due = unix_now() + wait.as_secs() as i64;
                let _ = ledger.reschedule(claim.id, &reason, due);
                emit(&on_event, &claim, "waiting", Some(&reason), None);
            } else {
                let reason = format!("{message} Gave up after {} attempts.", retry::MAX_ATTEMPTS);
                let _ = ledger.mark_failed(claim.id, &reason);
                emit(&on_event, &claim, "failed", Some(&reason), None);
            }
        }
    }
}

/// Remove the local file, but only ever on the path where the server has said
/// it holds the bytes.
///
/// A 201 is issued after the upload is written to the server's media directory,
/// and a 409 means it was already there, so both are real confirmations rather
/// than optimism. Nothing else in this function's callers reaches it: a
/// retryable failure, a permanent one and a paused queue all leave the file
/// alone, because the whole point of keeping it is that the upload might still
/// need it.
///
/// A removal that fails is reported and otherwise ignored. The upload
/// succeeded, which is what the person asked for; a file left behind is untidy,
/// not lost.
///
/// Both removals run on a blocking thread. Trashing is not a quick unlink — it
/// is a round trip through the OS, and on a volume that has no trash directory
/// yet macOS has been measured taking nearly two minutes to make one. Doing
/// that on an async worker would park a whole upload slot for the duration.
async fn reclaim_space(path: &std::path::Path, action: AfterUpload) -> Option<String> {
    if matches!(action, AfterUpload::Keep) {
        return None;
    }
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || match action {
        AfterUpload::Keep => None,
        AfterUpload::Trash => match trash::delete(&path) {
            Ok(()) => Some("Moved to trash".to_string()),
            Err(e) => {
                eprintln!("firesync: could not trash {}: {e}", path.display());
                None
            }
        },
        AfterUpload::Delete => match std::fs::remove_file(&path) {
            Ok(()) => Some("Deleted locally".to_string()),
            Err(e) => {
                eprintln!("firesync: could not delete {}: {e}", path.display());
                None
            }
        },
    })
    .await
    .unwrap_or(None)
}

fn emit<F>(on_event: &Arc<F>, claim: &Claim, state: &str, reason: Option<&str>, url: Option<String>)
where
    F: Fn(UploadEvent) + Send + Sync + 'static,
{
    on_event(UploadEvent {
        id: claim.id,
        path: claim.path.clone(),
        size: claim.size,
        sent: 0,
        state: state.to_string(),
        reason: reason.map(str::to_string),
        url,
        landed_as: None,
        removed_local: None,
        bytes_per_second: None,
    });
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::{MediaKind, WatchedFolder};
    use crate::ledger::FileState;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;

    /// A one-shot HTTP server that answers every request the same way.
    ///
    /// Real sockets rather than a mocked client: the point of these tests is the
    /// status-to-disposition table, and that is only meaningful against a
    /// response that actually crossed a connection.
    /// Read a whole request: headers, then the body.
    ///
    /// Reading only the headers and then answering is what made these tests
    /// flaky. The server would respond and close while the client was still
    /// writing its multipart body, the client would see the reset instead of
    /// the status, and reqwest classifies that as a transport failure — so a
    /// deliberate 400 arrived as "retryable" and the test asserting `Failed`
    /// saw `Queued`. Whether it lost that race depended on machine load, which
    /// is why it failed a few runs in twenty-five and never the same test.
    pub(crate) fn read_request(stream: &mut std::net::TcpStream) -> Option<Vec<u8>> {
        let mut headers = Vec::new();
        let mut byte = [0u8; 1];
        while !headers.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).ok()? == 0 {
                return None;
            }
            headers.push(byte[0]);
        }

        let text = String::from_utf8_lossy(&headers).to_lowercase();
        let length: Option<usize> = text
            .split("content-length:")
            .nth(1)
            .and_then(|rest| rest.split("\r\n").next())
            .and_then(|v| v.trim().parse().ok());

        let mut body = Vec::new();
        match length {
            Some(len) => {
                body.resize(len, 0);
                stream.read_exact(&mut body).ok()?;
            }
            // Chunked transfer encoding, which reqwest uses when a part's
            // length is unknown. Rather than decode it, read until the client
            // stops talking — enough to be sure it is no longer writing.
            None => {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(300)));
                let mut buf = [0u8; 8192];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n]);
                }
                let _ = stream.set_read_timeout(None);
            }
        }
        Some(body)
    }

    pub(crate) fn serve(status: u16, extra_headers: &str, body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let headers = extra_headers.to_string();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // The whole request, body included, before answering.
                let _ = read_request(&mut stream);

                let reason = match status {
                    201 => "CREATED",
                    400 => "BAD REQUEST",
                    401 => "UNAUTHORIZED",
                    409 => "CONFLICT",
                    413 => "PAYLOAD TOO LARGE",
                    429 => "TOO MANY REQUESTS",
                    503 => "SERVICE UNAVAILABLE",
                    _ => "ERROR",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{headers}\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), handle)
    }

    pub(crate) struct Fixture {
        pub dir: PathBuf,
        pub ledger: Arc<Ledger>,
        pub settings: Arc<Mutex<Settings>>,
        pub claim: Claim,
        pub events: Arc<Mutex<Vec<UploadEvent>>>,
    }

    pub(crate) fn fixture() -> Fixture {
        fixture_with(AfterUpload::Keep)
    }

    pub(crate) fn fixture_with(after_upload: AfterUpload) -> Fixture {
        fixture_in(std::env::temp_dir(), after_upload)
    }

    /// A watched folder lives where a person keeps their clips, not on the
    /// system temp volume — and for anything touching the trash that
    /// distinction is the difference between 200ms and two minutes.
    pub(crate) fn fixture_in_home(after_upload: AfterUpload) -> Fixture {
        let home = std::path::PathBuf::from(std::env::var("HOME").expect("HOME"));
        fixture_in(home, after_upload)
    }

    pub(crate) fn fixture_in(base: std::path::PathBuf, after_upload: AfterUpload) -> Fixture {
        let dir = base.join(format!(".firesync-q-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        std::fs::write(&clip, vec![0u8; 4096]).unwrap();

        let folder = WatchedFolder {
            id: "f1".into(),
            path: dir.clone(),
            enabled: true,
            include_subfolders: false,
            media: vec![MediaKind::Video],
            dest_folder: Some("clips".into()),
            game: Some("VALORANT".into()),
            min_size_bytes: None,
            max_size_bytes: None,
            after_upload,
            auto_sort_by_game: false,
        };

        let ledger = Arc::new(Ledger::open(&dir.join("l.sqlite")).unwrap());
        ledger
            .observe("f1", clip.to_str().unwrap(), 4096, 0, None)
            .unwrap();
        let claim = ledger.claim(1, &[]).unwrap().pop().expect("one queued file");

        Fixture {
            dir,
            ledger,
            settings: Arc::new(Mutex::new(Settings { folders: vec![folder], ..Settings::default() })),
            claim,
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Run an upload with the pre-flight existence check switched off.
    ///
    /// Empty type lists mean `viewer_for` has no answer, so the queue cannot
    /// name a parameter for the exists route and skips it. These tests are about
    /// the status-to-disposition table and the one-shot server answers exactly
    /// one request; a pre-flight would eat it. The check has its own tests
    /// below, against a server that expects it.
    pub(crate) async fn run_against(f: &Fixture, url: &str, control: Arc<QueueControl>) {
        let no_types = rules::SupportedTypes { video: vec![], image: vec![] };
        run_one_with(f, url, control, no_types).await
    }

    /// Run an upload with the server's real type lists, so the pre-flight runs.
    pub(crate) async fn run_checking(f: &Fixture, url: &str, control: Arc<QueueControl>) {
        run_one_with(f, url, control, rules::SupportedTypes::default()).await
    }

    async fn run_one_with(
        f: &Fixture,
        url: &str,
        control: Arc<QueueControl>,
        types: rules::SupportedTypes,
    ) {
        let events = f.events.clone();
        run_one(
            f.claim.clone(),
            url,
            "fsk_test",
            f.ledger.clone(),
            f.settings.clone(),
            control,
            Arc::new(move |e: UploadEvent| events.lock().unwrap().push(e)),
            ChunkPolicy::default(),
            Arc::new(Mutex::new(Default::default())),
            Arc::new(Mutex::new(types)),
        )
        .await;
    }

    pub(crate) fn state_of(f: &Fixture) -> FileState {
        f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap().state
    }

    /// A server that answers the pre-flight check, then the upload if one comes.
    ///
    /// The accept count is exact on purpose: the test joins this thread, so a
    /// server waiting for a request the client was never going to make would
    /// hang rather than fail. That makes "was the upload skipped?" something the
    /// test proves structurally instead of asserting after the fact.
    fn serve_preflight(
        exists_status: u16,
        exists_body: &'static str,
        upload: Option<(u16, &'static str)>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut answers: Vec<(u16, &'static str)> = vec![(exists_status, exists_body)];
            answers.extend(upload);
            for (status, body) in answers {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let _ = read_request(&mut stream);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), handle)
    }

    /// The whole point: the bytes never leave. The server is told to expect one
    /// request and one only, so an upload here would hang the join below.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_the_library_already_has_is_never_uploaded() {
        let f = fixture();
        let (url, server) =
            serve_preflight(200, r#"{"exists":true,"url":"/w/abc123"}"#, None);
        run_checking(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Duplicate);
        let ev = f.events.lock().unwrap();
        assert_eq!(ev[0].state, "duplicate");
        assert_eq!(ev[0].url.as_deref(), Some("/w/abc123"));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_the_library_does_not_have_is_uploaded() {
        let f = fixture();
        let (url, server) = serve_preflight(
            200,
            r#"{"exists":false}"#,
            Some((201, r#"{"filename":"clip.mp4","folder":"clips"}"#)),
        );
        run_checking(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Done);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// A server too old to have the route must not stop anything being sent.
    /// "Not known to be present" is the only safe reading of a failed check.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_without_the_exists_route_still_uploads() {
        let f = fixture();
        let (url, server) = serve_preflight(
            404,
            r#"{"error":"not found"}"#,
            Some((201, r#"{"filename":"clip.mp4","folder":"clips"}"#)),
        );
        run_checking(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Done);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// Skipping the transfer must not skip the tidying that a 409 would have
    /// done — the server has the bytes either way.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_skipped_upload_still_reclaims_the_local_file() {
        let f = fixture_in_home(AfterUpload::Delete);
        let clip = f.dir.join("clip.mp4");
        let (url, server) = serve_preflight(200, r#"{"exists":true}"#, None);
        run_checking(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Duplicate);
        assert!(!clip.exists(), "the local copy should have been reclaimed");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_201_is_done() {
        let f = fixture();
        let (url, server) = serve(201, "", r#"{"filename":"clip.mp4","folder":"clips"}"#);
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Done);
        let ev = f.events.lock().unwrap();
        assert_eq!(ev[0].state, "done");
        assert_eq!(ev[0].landed_as.as_deref(), Some("clips/clip.mp4"));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_409_is_a_duplicate_and_never_a_failure() {
        let f = fixture();
        let (url, server) = serve(409, "", r#"{"error":"duplicate","url":"/w/abc123"}"#);
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Duplicate);
        let ev = f.events.lock().unwrap();
        assert_eq!(ev[0].state, "duplicate");
        assert_eq!(ev[0].url.as_deref(), Some("/w/abc123"));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_401_pauses_the_whole_queue_and_charges_no_attempt() {
        let f = fixture();
        let control = Arc::new(QueueControl::new());
        let (url, server) = serve(401, "", "unauthorized");
        run_against(&f, &url, control.clone()).await;
        server.join().unwrap();

        assert!(control.is_paused(), "a rejected token must stop every queue");
        assert!(control.reason().is_some());

        // Back in the queue untouched: the file did nothing wrong, and burning
        // its retries on a credential problem would be charging the wrong thing.
        let row = f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap();
        assert_eq!(row.state, FileState::Queued);
        assert_eq!(row.attempts, 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_game_fails_permanently_with_the_server_s_own_words() {
        let f = fixture();
        let (url, server) = serve(
            400,
            "",
            r#"{"error":"unknown_game","message":"No game named \"Apex Legends\" exists in this library. Add it first, or pass game_id."}"#,
        );
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Failed);
        let row = f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap();
        assert!(row.reason.as_deref().unwrap().starts_with("No game named"), "{row:?}");
        // Permanent means permanent: no backoff was scheduled.
        assert_eq!(row.attempts, 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn images_being_off_is_permanent_not_retryable() {
        let f = fixture();
        let (url, server) = serve(503, "", "IMAGE_DIRECTORY is not configured.");
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Failed);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// A gateway saying 503 while Fireshare restarts is the opposite of a
    /// permanent answer, and must not be read as one — nor blamed on images.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_gateway_503_is_retryable_and_says_nothing_about_images() {
        let f = fixture();
        let (url, server) = serve(
            503,
            "",
            "<html><head><title>503 Service Temporarily Unavailable</title></head></html>",
        );
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Queued, "a restarting server is worth waiting for");
        let ev = f.events.lock().unwrap();
        let reason = ev[0].reason.clone().unwrap_or_default();
        assert!(!reason.to_lowercase().contains("image"), "blamed images: {reason}");
        let _ = std::fs::remove_dir_all(&f.dir);
    }


    #[tokio::test(flavor = "multi_thread")]
    async fn a_429_waits_exactly_as_long_as_the_server_asked() {
        let f = fixture();
        let (url, server) = serve(429, "Retry-After: 120\r\n", "slow down");
        let before = unix_now();
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        let row = f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap();
        assert_eq!(row.state, FileState::Queued);
        assert!(row.reason.as_deref().unwrap().contains("120s"), "{row:?}");

        // Due ~120s out, not on our own backoff curve.
        let due = f
            .ledger
            .claim(1, &[])
            .unwrap()
            .len();
        assert_eq!(due, 0, "a file told to wait 120s must not be claimable now");
        assert!(unix_now() - before < 10, "the wait must be scheduled, not slept through");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_500_backs_off_and_stays_queued() {
        let f = fixture();
        let (url, server) = serve(500, "", "boom");
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        let row = f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap();
        assert_eq!(row.state, FileState::Queued);
        assert_eq!(row.attempts, 1, "a retryable failure should count an attempt");
        assert!(row.reason.as_deref().unwrap().contains("attempt 1 of 8"), "{row:?}");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreachable_server_is_retryable_not_fatal() {
        let f = fixture();
        // Nothing is listening here.
        run_against(&f, "http://127.0.0.1:1", Arc::new(QueueControl::new())).await;

        let row = f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap();
        assert_eq!(row.state, FileState::Queued);
        assert_eq!(row.attempts, 1);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_that_vanished_fails_without_burning_retries() {
        let f = fixture();
        std::fs::remove_file(&f.claim.path).unwrap();
        run_against(&f, "http://127.0.0.1:1", Arc::new(QueueControl::new())).await;

        let row = f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap();
        assert_eq!(row.state, FileState::Failed);
        assert_eq!(row.attempts, 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_crash_mid_upload_puts_the_file_back() {
        let f = fixture();
        // `claim` in the fixture already marked it uploading.
        assert_eq!(state_of(&f), FileState::Uploading);
        assert_eq!(f.ledger.requeue_interrupted().unwrap(), 1);
        assert_eq!(state_of(&f), FileState::Queued);
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}

#[cfg(test)]
mod removal_tests {
    use super::tests::*;
    use super::*;
    use crate::config::AfterUpload;
    use crate::ledger::FileState;

    /// The whole feature in one assertion: the bytes are on the server, so the
    /// local copy can go.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_successful_upload_can_delete_the_local_file() {
        let f = fixture_with(AfterUpload::Delete);
        let path = std::path::PathBuf::from(&f.claim.path);
        assert!(path.exists());

        let (url, server) = serve(201, "", r#"{"filename":"clip.mp4","folder":"clips"}"#);
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert!(!path.exists(), "the local file should be gone after a confirmed upload");
        assert_eq!(state_of(&f), FileState::Done);
        assert_eq!(f.events.lock().unwrap()[0].removed_local.as_deref(), Some("Deleted locally"));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// The setting is off by default and stays off unless asked for.
    #[tokio::test(flavor = "multi_thread")]
    async fn keep_is_the_default_and_leaves_the_file_alone() {
        let f = fixture();
        let path = std::path::PathBuf::from(&f.claim.path);

        let (url, server) = serve(201, "", r#"{"filename":"clip.mp4","folder":"clips"}"#);
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert!(path.exists(), "nothing should be removed unless the folder asked for it");
        assert_eq!(f.events.lock().unwrap()[0].removed_local, None);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// The one that matters. A file that did not make it is the only copy there
    /// is, and deleting it would be destroying the thing we were asked to send.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_permanent_failure_never_removes_the_local_file() {
        let f = fixture_with(AfterUpload::Delete);
        let path = std::path::PathBuf::from(&f.claim.path);

        let (url, server) = serve(400, "", "Unsupported file type.");
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Failed);
        assert!(path.exists(), "a failed upload must leave the only copy where it is");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_retryable_failure_never_removes_the_local_file() {
        let f = fixture_with(AfterUpload::Delete);
        let path = std::path::PathBuf::from(&f.claim.path);

        let (url, server) = serve(500, "", "boom");
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Queued);
        assert!(path.exists(), "a file waiting to be retried still needs its bytes");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_rejected_token_never_removes_the_local_file() {
        let f = fixture_with(AfterUpload::Delete);
        let path = std::path::PathBuf::from(&f.claim.path);

        let (url, server) = serve(401, "", "unauthorized");
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert!(path.exists(), "a paused queue has uploaded nothing, so it may delete nothing");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// A 409 means the server holds these bytes already, which is the same
    /// confirmation a 201 gives — and re-scanning a folder that was uploaded
    /// before is exactly when clearing it out is useful.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_duplicate_counts_as_the_server_having_it() {
        let f = fixture_with(AfterUpload::Delete);
        let path = std::path::PathBuf::from(&f.claim.path);

        let (url, server) = serve(409, "", r#"{"error":"duplicate","url":"/w/abc"}"#);
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Duplicate);
        assert!(!path.exists(), "the library already has it, so the local copy can go");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trash_removes_it_from_the_folder_but_recoverably() {
        let f = fixture_in_home(AfterUpload::Trash);
        let path = std::path::PathBuf::from(&f.claim.path);

        let (url, server) = serve(201, "", r#"{"filename":"clip.mp4","folder":"clips"}"#);
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert!(!path.exists(), "trashing should clear it out of the watched folder");
        assert_eq!(f.events.lock().unwrap()[0].removed_local.as_deref(), Some("Moved to trash"));
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}

#[cfg(test)]
mod chunked_tests {
    use super::tests::*;
    use super::*;
    use crate::api::upload::ChunkPolicy;
    use crate::config::AfterUpload;
    use crate::ledger::FileState;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    /// How a fake Fireshare behaves while a set is being assembled.
    #[derive(Clone, Copy, PartialEq)]
    enum Behaviour {
        /// Keeps every part and completes when it has them all.
        Honest,
        /// Forgets everything it holds after this many chunks, the way a
        /// restart of Fireshare sweeps the part files.
        LoseAfter(usize),
        /// Answers 202 with no body, the way a Fireshare predating the
        /// informative-202 change does. The client cannot see a shortfall and
        /// has to notice it by running out of chunks.
        Silent,
        /// Refuses a whole-file upload as too large but takes chunks happily,
        /// the way a proxy with a request-body cap in front of Fireshare does.
        RefuseWholeFile,
        /// Refuses everything as too large, including chunks. Nothing the client
        /// can do differently will help.
        RefuseEverything,
    }

    struct FakeServer {
        url: String,
        /// chunkPart values in the order they arrived.
        seen: Arc<Mutex<Vec<i64>>>,
        requests: Arc<AtomicUsize>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeServer {
        fn start(behaviour: Behaviour) -> FakeServer {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let requests = Arc::new(AtomicUsize::new(0));

            let seen_t = seen.clone();
            let requests_t = requests.clone();
            let handle = std::thread::spawn(move || {
                // What the server is "holding". Cleared when it loses its parts.
                let mut held: Vec<i64> = Vec::new();
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let Some(body) = read_request(&mut stream) else { break };
                    requests_t.fetch_add(1, AtomicOrdering::Relaxed);

                    if behaviour == Behaviour::RefuseEverything {
                        let _ = respond(&mut stream, 413, "too large");
                        continue;
                    }

                    let part = field(&body, "chunkPart").and_then(|v| v.parse::<i64>().ok());
                    let total = field(&body, "totalChunks").and_then(|v| v.parse::<i64>().ok());
                    let (Some(part), Some(total)) = (part, total) else {
                        let status =
                            if behaviour == Behaviour::RefuseWholeFile { 413 } else { 400 };
                        let _ = respond(&mut stream, status, "not a chunk request");
                        continue;
                    };
                    seen_t.lock().unwrap().push(part);

                    if !held.contains(&part) {
                        held.push(part);
                    }

                    if let Behaviour::LoseAfter(n) = behaviour {
                        if held.len() > n {
                            held.clear();
                            held.push(part);
                        }
                    }

                    if held.len() as i64 == total && behaviour != Behaviour::Silent {
                        let _ = respond(
                            &mut stream,
                            201,
                            r#"{"filename":"clip.mp4","folder":"clips"}"#,
                        );
                        break;
                    }

                    let body = if behaviour == Behaviour::Silent {
                        String::new()
                    } else {
                        format!(
                            r#"{{"status":"partial","received":{},"total":{}}}"#,
                            held.len(),
                            total
                        )
                    };
                    let _ = respond(&mut stream, 202, &body);
                }
            });

            FakeServer { url, seen, requests, handle: Some(handle) }
        }

        fn chunks_seen(&self) -> Vec<i64> {
            self.seen.lock().unwrap().clone()
        }

        fn request_count(&self) -> usize {
            self.requests.load(AtomicOrdering::Relaxed)
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            // The listener closes with the thread; nothing to join deterministically
            // once a test has stopped sending.
            if let Some(h) = self.handle.take() {
                drop(h);
            }
        }
    }

    /// Pull a multipart text field out of a raw body. Crude, and enough: these
    /// are fields this client wrote moments earlier.
    fn field(body: &[u8], name: &str) -> Option<String> {
        let text = String::from_utf8_lossy(body);
        let marker = format!("name=\"{name}\"");
        let start = text.find(&marker)? + marker.len();
        let rest = &text[start..];
        let value_start = rest.find("\r\n\r\n")? + 4;
        let rest = &rest[value_start..];
        let value_end = rest.find("\r\n")?;
        Some(rest[..value_end].to_string())
    }

    fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) -> std::io::Result<()> {
        let reason = match status {
            201 => "CREATED",
            202 => "ACCEPTED",
            _ => "ERROR",
        };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes())?;
        stream.flush()
    }

    /// 10 KB file, 1 KB chunks, threshold 1 KB: ten real chunk requests.
    fn tiny_policy() -> ChunkPolicy {
        ChunkPolicy { threshold: 1024, size: 1024 }
    }

    fn big_fixture() -> Fixture {
        let f = fixture_with(AfterUpload::Keep);
        std::fs::write(&f.claim.path, vec![7u8; 10 * 1024]).unwrap();
        f
    }

    async fn run_chunked(f: &Fixture, url: &str, control: Arc<QueueControl>) {
        let events = f.events.clone();
        let mut claim = f.claim.clone();
        claim.size = 10 * 1024;
        run_one(
            claim,
            url,
            "fsk_test",
            f.ledger.clone(),
            f.settings.clone(),
            control,
            Arc::new(move |e: UploadEvent| events.lock().unwrap().push(e)),
            tiny_policy(),
            Arc::new(Mutex::new(Default::default())),
            // No pre-flight: the fake server counts chunk requests, and an
            // exists check it does not understand would be counted as one.
            Arc::new(Mutex::new(rules::SupportedTypes { video: vec![], image: vec![] })),
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_large_file_goes_up_in_order_and_completes() {
        let f = big_fixture();
        let server = FakeServer::start(Behaviour::Honest);
        run_chunked(&f, &server.url, Arc::new(QueueControl::new())).await;

        assert_eq!(server.chunks_seen(), (1..=10).collect::<Vec<_>>(), "chunks must go in order");
        assert_eq!(
            f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap().state,
            FileState::Done
        );
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// The whole point of the endpoint: an interrupted upload does not start over.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_restart_resumes_from_where_it_stopped() {
        let f = big_fixture();

        // Stand in for a previous run that got four chunks in before dying.
        f.ledger.begin_chunks(f.claim.id, "prevrun", 10).unwrap();
        f.ledger.advance_chunks(f.claim.id, 4).unwrap();

        let server = FakeServer::start(Behaviour::Honest);
        run_chunked(&f, &server.url, Arc::new(QueueControl::new())).await;

        let seen = server.chunks_seen();
        assert_eq!(seen.first(), Some(&5), "should resume at chunk 5, not restart at 1");
        assert!(!seen.contains(&1), "chunk 1 was already sent and must not go again");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// Fireshare restarting sweeps its part files. Every chunk we send after
    /// that answers 202 against a set that can never complete, so the client has
    /// to notice and begin again rather than hang.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_that_loses_its_parts_makes_the_file_start_again() {
        let f = big_fixture();
        let server = FakeServer::start(Behaviour::LoseAfter(3));
        run_chunked(&f, &server.url, Arc::new(QueueControl::new())).await;

        let row = f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap();
        assert_eq!(row.state, FileState::Queued, "it should be waiting for another go");
        assert_eq!(row.attempts, 1);
        assert!(
            row.reason.as_deref().unwrap().contains("Starting this file again"),
            "{row:?}"
        );

        // And the progress is discarded, so the retry mints a fresh set.
        let state = f.ledger.chunk_state(f.claim.id).unwrap();
        assert_eq!(state.check_sum, None);
        assert_eq!(state.chunks_done, 0);

        // It gave up quickly rather than sending all ten into a lost set.
        assert!(server.request_count() <= 5, "sent {} chunks before noticing", server.request_count());
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// A Fireshare without the informative 202 gives no count to compare
    /// against, so the only signal is running out of chunks with no completion.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_old_server_with_a_bare_202_is_still_caught() {
        let f = big_fixture();
        let server = FakeServer::start(Behaviour::Silent);
        run_chunked(&f, &server.url, Arc::new(QueueControl::new())).await;

        assert_eq!(server.chunks_seen(), (1..=10).collect::<Vec<_>>(), "all ten get sent first");

        let row = f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap();
        assert_eq!(row.state, FileState::Queued);
        assert!(row.reason.as_deref().unwrap().contains("Starting this file again"), "{row:?}");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// A re-recorded clip under the same name has a different length, so the
    /// stored progress describes a file that no longer exists. Resuming onto it
    /// would assemble something that never did.
    #[tokio::test(flavor = "multi_thread")]
    async fn progress_from_a_differently_sized_file_is_not_resumed_onto() {
        let f = big_fixture();
        f.ledger.begin_chunks(f.claim.id, "stale", 3).unwrap();
        f.ledger.advance_chunks(f.claim.id, 2).unwrap();

        let server = FakeServer::start(Behaviour::Honest);
        run_chunked(&f, &server.url, Arc::new(QueueControl::new())).await;

        assert_eq!(
            server.chunks_seen().first(),
            Some(&1),
            "a set recorded for a different chunk count must be abandoned, not resumed"
        );
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_small_file_never_touches_the_chunked_route() {
        let f = fixture_with(AfterUpload::Keep); // 4 KB, under the 1 KB... no, over it
        std::fs::write(&f.claim.path, vec![1u8; 512]).unwrap();

        let server = FakeServer::start(Behaviour::Honest);
        let events = f.events.clone();
        let mut claim = f.claim.clone();
        claim.size = 512;
        run_one(
            claim,
            &server.url,
            "fsk_test",
            f.ledger.clone(),
            f.settings.clone(),
            Arc::new(QueueControl::new()),
            Arc::new(move |e: UploadEvent| events.lock().unwrap().push(e)),
            tiny_policy(),
            Arc::new(Mutex::new(Default::default())),
            // No pre-flight: the fake server counts chunk requests, and an
            // exists check it does not understand would be counted as one.
            Arc::new(Mutex::new(rules::SupportedTypes { video: vec![], image: vec![] })),
        )
        .await;

        // The fake server only understands chunk requests, so a single-shot
        // upload reaches it as a 400 — which is itself the proof that the
        // chunked route was not used.
        assert!(server.chunks_seen().is_empty(), "a file under the threshold must go in one request");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// When chunking is no escape either, the file has genuinely been refused
    /// and saying so beats retrying forever.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refusal_that_survives_chunking_is_permanent() {
        let f = fixture_with(AfterUpload::Keep);
        std::fs::write(&f.claim.path, vec![1u8; 2048]).unwrap();

        let server = FakeServer::start(Behaviour::RefuseEverything);
        let events = f.events.clone();
        let mut claim = f.claim.clone();
        claim.size = 2048;
        run_one(
            claim,
            &server.url,
            "fsk_test",
            f.ledger.clone(),
            f.settings.clone(),
            Arc::new(QueueControl::new()),
            Arc::new(move |e: UploadEvent| events.lock().unwrap().push(e)),
            ChunkPolicy { threshold: 4096, size: 1024 },
            Arc::new(Mutex::new(Default::default())),
            Arc::new(Mutex::new(rules::SupportedTypes { video: vec![], image: vec![] })),
        )
        .await;

        assert_eq!(state_of(&f), FileState::Failed);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// A body cap lower than our chunk threshold must not fail the file.
    ///
    /// Whatever sits in front of Fireshare gets to decide how big one request
    /// may be, and it does not tell us in advance. Finding out the hard way is
    /// fine as long as the answer is "send it differently" rather than "this
    /// file can never be uploaded" — the same bytes in chunks are each far
    /// below any such cap.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_whole_file_refused_as_too_large_goes_up_in_chunks_instead() {
        let f = fixture_with(AfterUpload::Keep);
        std::fs::write(&f.claim.path, vec![1u8; 2048]).unwrap();

        let server = FakeServer::start(Behaviour::RefuseWholeFile);
        let events = f.events.clone();
        let mut claim = f.claim.clone();
        claim.size = 2048;
        run_one(
            claim,
            &server.url,
            "fsk_test",
            f.ledger.clone(),
            f.settings.clone(),
            Arc::new(QueueControl::new()),
            Arc::new(move |e: UploadEvent| events.lock().unwrap().push(e)),
            // Threshold above the file, so it starts as a single request and
            // only reaches the chunked route by being refused.
            ChunkPolicy { threshold: 4096, size: 1024 },
            Arc::new(Mutex::new(Default::default())),
            Arc::new(Mutex::new(rules::SupportedTypes { video: vec![], image: vec![] })),
        )
        .await;

        assert_eq!(state_of(&f), FileState::Done, "the fallback should have landed it");
        assert!(!server.chunks_seen().is_empty(), "it should have fallen back to chunks");
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::api::discovery::{FolderRule, FolderRules};
    use crate::config::{MediaKind, WatchedFolder};
    use std::path::{Path, PathBuf};

    fn rules() -> FolderRules {
        FolderRules {
            video: vec![
                FolderRule { folder: "valorant".into(), game_id: Some(1), game: Some("VALORANT".into()) },
                FolderRule { folder: "ow2".into(), game_id: Some(2), game: Some("Overwatch 2".into()) },
            ],
            image: vec![FolderRule {
                folder: "shots-valorant".into(),
                game_id: Some(1),
                game: Some("VALORANT".into()),
            }],
        }
    }

    fn folder(game: Option<&str>, auto: bool, media: Vec<MediaKind>) -> WatchedFolder {
        WatchedFolder {
            id: "f".into(),
            path: PathBuf::from("/tmp/w"),
            enabled: true,
            include_subfolders: false,
            media,
            dest_folder: Some("uploads".into()),
            game: game.map(str::to_string),
            min_size_bytes: None,
            max_size_bytes: None,
            after_upload: AfterUpload::Keep,
            auto_sort_by_game: auto,
        }
    }

    /// The point of the feature: the clip goes where the game already lives, and
    /// the game is not named — the server tags it from the folder.
    #[test]
    fn auto_sort_files_a_clip_in_its_game_s_folder() {
        let meta = destination_for(
            &folder(Some("VALORANT"), true, vec![MediaKind::Video]),
            &rules(),
            Path::new("/tmp/w/clip.mp4"),
        );
        assert_eq!(meta.folder.as_deref(), Some("valorant"));
        assert_eq!(meta.game, None, "the folder does the tagging, so naming the game adds nothing");
    }

    /// Game names are matched the way the server matches them elsewhere.
    #[test]
    fn game_names_match_without_regard_to_case() {
        let meta = destination_for(
            &folder(Some("valorant"), true, vec![MediaKind::Video]),
            &rules(),
            Path::new("/tmp/w/clip.mp4"),
        );
        assert_eq!(meta.folder.as_deref(), Some("valorant"));
    }

    /// Images live in a different tree, so they follow the image rule.
    #[test]
    fn an_image_follows_the_image_rule_not_the_video_one() {
        let meta = destination_for(
            &folder(Some("VALORANT"), true, vec![MediaKind::Video, MediaKind::Image]),
            &rules(),
            Path::new("/tmp/w/shot.png"),
        );
        assert_eq!(meta.folder.as_deref(), Some("shots-valorant"));
    }

    /// A guess would be worse than the choice somebody already made.
    #[test]
    fn a_game_with_no_folder_falls_back_to_the_explicit_one() {
        let meta = destination_for(
            &folder(Some("Helldivers 2"), true, vec![MediaKind::Video]),
            &rules(),
            Path::new("/tmp/w/clip.mp4"),
        );
        assert_eq!(meta.folder.as_deref(), Some("uploads"));
        assert_eq!(meta.game.as_deref(), Some("Helldivers 2"), "the name is still worth sending");
    }

    #[test]
    fn no_game_means_there_is_nothing_to_sort_by() {
        let meta = destination_for(
            &folder(None, true, vec![MediaKind::Video]),
            &rules(),
            Path::new("/tmp/w/clip.mp4"),
        );
        assert_eq!(meta.folder.as_deref(), Some("uploads"));
        assert_eq!(meta.game, None);
    }

    /// Turning it off means the explicit destination wins, game name included.
    #[test]
    fn switching_it_off_restores_the_manual_choice() {
        let meta = destination_for(
            &folder(Some("VALORANT"), false, vec![MediaKind::Video]),
            &rules(),
            Path::new("/tmp/w/clip.mp4"),
        );
        assert_eq!(meta.folder.as_deref(), Some("uploads"));
        assert_eq!(meta.game.as_deref(), Some("VALORANT"));
    }

    /// An instance without the folder-rules field behaves as it did before.
    #[test]
    fn an_instance_with_no_rules_is_not_broken_by_the_setting() {
        let meta = destination_for(
            &folder(Some("VALORANT"), true, vec![MediaKind::Video]),
            &FolderRules::default(),
            Path::new("/tmp/w/clip.mp4"),
        );
        assert_eq!(meta.folder.as_deref(), Some("uploads"));
        assert_eq!(meta.game.as_deref(), Some("VALORANT"));
    }
}
