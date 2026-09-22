pub mod retry;
pub mod rules;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::api::upload::{upload_single, UploadError, UploadMeta, UploadResult};
use crate::config::Settings;
use crate::ledger::{Claim, Ledger};
use crate::secrets;

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
    /// Where the server filed it, which is not always where we asked: it
    /// suffixes the name when something is already sitting on it.
    pub landed_as: Option<String>,
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
            let Ok(Some(token)) = secrets::load_token() else {
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
                let control = deps.control.clone();
                let on_event = deps.on_event.clone();
                let base_url = base_url.clone();
                let token = token.clone();

                tasks.push(tauri::async_runtime::spawn(async move {
                    run_one(claim, &base_url, &token, ledger, settings, control, on_event).await;
                }));
            }
            for task in tasks {
                let _ = task.await;
            }
        }
    });
}

async fn run_one<F>(
    claim: Claim,
    base_url: &str,
    token: &str,
    ledger: Arc<Ledger>,
    settings: Arc<Mutex<Settings>>,
    control: Arc<QueueControl>,
    on_event: Arc<F>,
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

    let meta = {
        let settings = settings.lock().expect("settings mutex");
        match settings.folders.iter().find(|f| f.id == claim.folder_id) {
            Some(folder) => UploadMeta {
                folder: folder.dest_folder.clone(),
                game: folder.game.clone(),
                title: None,
            },
            // The folder was removed while this file was in flight.
            None => {
                let _ = ledger.release(claim.id);
                return;
            }
        }
    };

    match upload_single(base_url, token, &path, &meta).await {
        Ok(UploadResult::Accepted { filename, folder }) => {
            let landed = if folder.is_empty() {
                filename.clone()
            } else {
                format!("{folder}/{filename}")
            };
            let _ = ledger.mark_done(claim.id, None);
            on_event(UploadEvent {
                id: claim.id,
                path: claim.path.clone(),
                size: claim.size,
                state: "done".into(),
                reason: None,
                url: None,
                landed_as: Some(landed),
            });
        }

        // Not a failure. The library already has it, so there is nothing to fix
        // and nothing to try again.
        Ok(UploadResult::Duplicate { url }) => {
            let _ = ledger.mark_duplicate(claim.id, url.as_deref());
            emit(&on_event, &claim, "duplicate", Some("Already in your library"), url);
        }

        Err(UploadError::Unauthorized(message)) => {
            // Put this file back untouched — it did nothing wrong, and charging
            // it an attempt for a credential problem would burn its retries.
            let _ = ledger.release(claim.id);
            control.pause(Some(message.clone()));
            emit(&on_event, &claim, "paused", Some(&message), None);
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

fn emit<F>(on_event: &Arc<F>, claim: &Claim, state: &str, reason: Option<&str>, url: Option<String>)
where
    F: Fn(UploadEvent) + Send + Sync + 'static,
{
    on_event(UploadEvent {
        id: claim.id,
        path: claim.path.clone(),
        size: claim.size,
        state: state.to_string(),
        reason: reason.map(str::to_string),
        url,
        landed_as: None,
    });
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MediaKind, WatchedFolder};
    use crate::ledger::FileState;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;

    /// A one-shot HTTP server that answers every request the same way.
    ///
    /// Real sockets rather than a mocked client: the point of these tests is the
    /// status-to-disposition table, and that is only meaningful against a
    /// response that actually crossed a connection.
    fn serve(status: u16, extra_headers: &str, body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let headers = extra_headers.to_string();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the request so the client is not writing into a closed
                // socket while we answer.
                let peek = stream.try_clone().unwrap();
                let mut reader = BufReader::new(peek);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line == "\r\n" {
                        break;
                    }
                    line.clear();
                }
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
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n{headers}\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), handle)
    }

    struct Fixture {
        dir: PathBuf,
        ledger: Arc<Ledger>,
        settings: Arc<Mutex<Settings>>,
        claim: Claim,
        events: Arc<Mutex<Vec<UploadEvent>>>,
    }

    fn fixture() -> Fixture {
        let dir = std::env::temp_dir().join(format!("firesync-q-{}", uuid::Uuid::new_v4()));
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

    async fn run_against(f: &Fixture, url: &str, control: Arc<QueueControl>) {
        let events = f.events.clone();
        run_one(
            f.claim.clone(),
            url,
            "fsk_test",
            f.ledger.clone(),
            f.settings.clone(),
            control,
            Arc::new(move |e: UploadEvent| events.lock().unwrap().push(e)),
        )
        .await;
    }

    fn state_of(f: &Fixture) -> FileState {
        f.ledger.recent(10).unwrap().into_iter().find(|r| r.id == f.claim.id).unwrap().state
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

    #[tokio::test(flavor = "multi_thread")]
    async fn a_413_is_permanent() {
        let f = fixture();
        let (url, server) = serve(413, "", "too large");
        run_against(&f, &url, Arc::new(QueueControl::new())).await;
        server.join().unwrap();

        assert_eq!(state_of(&f), FileState::Failed);
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
