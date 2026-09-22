pub mod settle;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebouncedEvent, Debouncer, RecommendedCache};
use serde::Serialize;

use crate::config::WatchedFolder;
use crate::error::{AppError, Result};
use crate::ledger::{Ledger, Outcome};
use crate::queue::rules::{self, SupportedTypes};
use settle::{wait_until_settled, SettleConfig, Settled};

/// Recorders emit a storm of write events per file; without this the settle loop
/// would be started dozens of times for one clip.
const DEBOUNCE: Duration = Duration::from_secs(2);

type Deb = Debouncer<notify::RecommendedWatcher, RecommendedCache>;

/// What the watcher decided, published to the UI so "why didn't my clip upload"
/// has an answer that is visible rather than buried in a log.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub folder_id: String,
    pub path: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub size: u64,
    pub at: i64,
}

pub struct Watchers {
    debouncers: HashMap<String, Deb>,
    tx: tokio::sync::mpsc::UnboundedSender<(String, PathBuf)>,
    debounce: Duration,
}

impl Watchers {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<(String, PathBuf)>) -> Self {
        Self { debouncers: HashMap::new(), tx, debounce: DEBOUNCE }
    }

    /// Tests need the debounce shorter than the two seconds a real recorder
    /// warrants, or every case costs a wall-clock eternity.
    #[cfg(test)]
    pub fn with_debounce(
        tx: tokio::sync::mpsc::UnboundedSender<(String, PathBuf)>,
        debounce: Duration,
    ) -> Self {
        Self { debouncers: HashMap::new(), tx, debounce }
    }

    /// Bring the running watchers in line with the configured folders. Called on
    /// startup and after any folder is added, removed, paused or resumed.
    pub fn sync(&mut self, folders: &[WatchedFolder]) -> Vec<(String, AppError)> {
        let wanted: HashSet<&str> =
            folders.iter().filter(|f| f.enabled).map(|f| f.id.as_str()).collect();

        self.debouncers.retain(|id, _| wanted.contains(id.as_str()));

        let mut problems = Vec::new();
        for folder in folders.iter().filter(|f| f.enabled) {
            if self.debouncers.contains_key(&folder.id) {
                continue;
            }
            match self.start_one(folder) {
                Ok(deb) => {
                    self.debouncers.insert(folder.id.clone(), deb);
                }
                Err(e) => problems.push((folder.id.clone(), e)),
            }
        }
        problems
    }

    fn start_one(&self, folder: &WatchedFolder) -> Result<Deb> {
        if !folder.path.is_dir() {
            return Err(AppError::Storage(format!(
                "{} is not a folder, or is not reachable from this machine.",
                folder.path.display()
            )));
        }

        let tx = self.tx.clone();
        let folder_id = folder.id.clone();

        let mut debouncer = new_debouncer(
            self.debounce,
            None,
            move |result: std::result::Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
                let Ok(events) = result else { return };
                for event in events {
                    // Creates, writes and renames-into-the-folder all mean "look
                    // at this path". Removals are ignored: the ledger keeps its
                    // row so a file that comes back is not treated as new.
                    if !matches!(
                        event.kind,
                        notify::EventKind::Create(_) | notify::EventKind::Modify(_)
                    ) {
                        continue;
                    }
                    for path in &event.paths {
                        let _ = tx.send((folder_id.clone(), path.clone()));
                    }
                }
            },
        )
        .map_err(describe_watch_error)?;

        let mode = if folder.include_subfolders {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        debouncer.watch(&folder.path, mode).map_err(describe_watch_error)?;

        Ok(debouncer)
    }
}

/// Resolve a path to the form the ledger keys on.
///
/// The baseline scan walks the folder as configured, but notify reports paths
/// with symlinks already resolved — on macOS `/var/folders/…` comes back as
/// `/private/var/folders/…`, and a home directory behind a symlink does the same
/// on Linux. Keying the ledger on the raw string would file one file under two
/// identities, so a clip that was present when the folder was added would be
/// recorded as baseline and then queued anyway by the watcher, which is exactly
/// the promise this feature exists to keep.
///
/// Falls back to the original path when the file has already gone: an identity
/// for something that no longer exists is not worth failing over.
pub fn canonical(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// notify's errors are accurate and unreadable. The inotify limit in particular
/// is a real thing people hit with a recursive watch on a deep tree, and it
/// deserves to say what to do rather than "os error 28".
fn describe_watch_error(e: notify::Error) -> AppError {
    let detail = e.to_string();
    match e.kind {
        notify::ErrorKind::MaxFilesWatch => AppError::Storage(
            "This machine has run out of filesystem watches. On Linux, raise \
             fs.inotify.max_user_watches — a recursive watch on a deep folder can exhaust the \
             default of 8192."
                .into(),
        ),
        notify::ErrorKind::PathNotFound => {
            AppError::Storage("That folder no longer exists on this machine.".into())
        }
        notify::ErrorKind::Io(ref io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
            AppError::Storage("Firesync is not allowed to read that folder.".into())
        }
        _ => AppError::Storage(format!("Could not watch that folder: {detail}")),
    }
}

/// Consume watcher events: wait for each file to finish, apply the folder's
/// rules, and record the verdict.
pub fn spawn_event_loop<F>(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<(String, PathBuf)>,
    ledger: Arc<Ledger>,
    settings: Arc<Mutex<crate::config::Settings>>,
    types: Arc<Mutex<SupportedTypes>>,
    settle_cfg: SettleConfig,
    on_decision: F,
) where
    F: Fn(Decision) + Send + Sync + 'static,
{
    let in_flight: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    let on_decision = Arc::new(on_decision);

    tauri::async_runtime::spawn(async move {
        while let Some((folder_id, path)) = rx.recv().await {
            let current_types = types.lock().expect("types mutex").clone();
            if !rules::worth_settling(&path, &current_types) {
                continue;
            }

            // One waiter per path. A clip being written produces events for the
            // whole time it is being written; without this each one starts its
            // own settle loop.
            {
                let mut flight = in_flight.lock().expect("in_flight mutex");
                if !flight.insert(path.clone()) {
                    continue;
                }
            }

            let ledger = ledger.clone();
            let settings = settings.clone();
            let in_flight = in_flight.clone();
            let on_decision = on_decision.clone();
            let settle_cfg = settle_cfg.clone();

            tauri::async_runtime::spawn(async move {
                let settled = wait_until_settled(&path, &settle_cfg).await;
                in_flight.lock().expect("in_flight mutex").remove(&path);

                let Settled::Ready { size, mtime } = settled else {
                    return;
                };

                let folder = settings
                    .lock()
                    .expect("settings mutex")
                    .folders
                    .iter()
                    .find(|f| f.id == folder_id)
                    .cloned();
                let Some(folder) = folder else { return };

                let verdict = rules::evaluate(&folder, &path, size, &current_types);
                // Same identity the baseline scan used, or the two disagree.
                let path_str = canonical(&path).to_string_lossy().to_string();

                let outcome = match ledger.observe(
                    &folder_id,
                    &path_str,
                    size as i64,
                    mtime,
                    verdict.as_deref(),
                ) {
                    Ok(o) => o,
                    Err(e) => {
                        eprintln!("firesync: could not record {path_str}: {e}");
                        return;
                    }
                };

                // Nothing changed and nothing to say — do not spam the UI.
                if matches!(outcome, Outcome::Unchanged | Outcome::Held) {
                    return;
                }

                on_decision(Decision {
                    folder_id,
                    path: path_str,
                    outcome: format!("{outcome:?}").to_lowercase(),
                    reason: verdict,
                    size,
                    at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                });
            });
        }
    });
}

/// Everything already in a folder, for the baseline snapshot taken when it is
/// added. Only files the rules could ever accept are listed — a folder full of
/// `.log` noise should not produce thousands of baseline rows.
pub fn scan_existing(folder: &WatchedFolder, types: &SupportedTypes) -> Vec<(String, i64, i64)> {
    let mut out = Vec::new();
    // Start canonical so every path below is too, matching what the watcher
    // records for the same file.
    let mut stack = vec![canonical(&folder.path)];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };

            if meta.is_dir() {
                if folder.include_subfolders {
                    stack.push(path);
                }
                continue;
            }
            if !meta.is_file() || !rules::worth_settling(&path, types) {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            out.push((path.to_string_lossy().to_string(), meta.len() as i64, mtime));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MediaKind, Settings};
    use std::io::Write;

    struct Harness {
        dir: PathBuf,
        _watchers: Watchers,
        ledger: Arc<Ledger>,
        folder_id: String,
        decisions: Arc<Mutex<Vec<Decision>>>,
    }

    fn write_sized(path: &PathBuf, bytes: usize) {
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    /// A folder with two files already in it, watched with a 5 MB floor.
    async fn harness() -> Harness {
        let dir = std::env::temp_dir().join(format!("firesync-watch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Already present before the folder is added.
        write_sized(&dir.join("already-here.mp4"), 6 << 20);
        write_sized(&dir.join("also-here.mp4"), 7 << 20);

        let folder = WatchedFolder {
            id: "f-test".into(),
            path: dir.clone(),
            enabled: true,
            include_subfolders: false,
            media: vec![MediaKind::Video],
            dest_folder: None,
            game: None,
            min_size_bytes: Some(5 << 20),
            max_size_bytes: None,
            after_upload: crate::config::AfterUpload::Keep,
        };

        let types = SupportedTypes::default();
        let ledger =
            Arc::new(Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens"));

        let present = scan_existing(&folder, &types);
        assert_eq!(present.len(), 2, "baseline scan should see both existing clips");
        ledger.record_baseline(&folder.id, &present).unwrap();

        let settings = Arc::new(Mutex::new(Settings {
            folders: vec![folder.clone()],
            ..Settings::default()
        }));

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut watchers = Watchers::with_debounce(tx, Duration::from_millis(120));
        assert!(watchers.sync(&[folder.clone()]).is_empty(), "watch should start cleanly");

        let decisions = Arc::new(Mutex::new(Vec::new()));
        let sink = decisions.clone();
        spawn_event_loop(
            rx,
            ledger.clone(),
            settings,
            Arc::new(Mutex::new(types)),
            SettleConfig {
                poll_interval: Duration::from_millis(60),
                stable_polls: 3,
                max_wait: Duration::from_secs(15),
            },
            move |d| sink.lock().unwrap().push(d),
        );

        Harness { dir, _watchers: watchers, ledger, folder_id: folder.id, decisions }
    }

    async fn decision_for(h: &Harness, name: &str, within: Duration) -> Option<Decision> {
        let deadline = tokio::time::Instant::now() + within;
        while tokio::time::Instant::now() < deadline {
            if let Some(d) =
                h.decisions.lock().unwrap().iter().find(|d| d.path.ends_with(name)).cloned()
            {
                return Some(d);
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        None
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_clip_still_being_written_is_not_queued_until_it_stops_growing() {
        let h = harness().await;
        let path = h.dir.join("recording.mp4");

        // A recorder: creates the file, then keeps appending for a while.
        write_sized(&path, 1 << 20);
        let writer = {
            let path = path.clone();
            tokio::spawn(async move {
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(70)).await;
                    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
                    f.write_all(&vec![0u8; 1 << 20]).unwrap();
                }
            })
        };

        // While it is still growing, nothing about it should have been decided.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            h.decisions.lock().unwrap().iter().all(|d| !d.path.ends_with("recording.mp4")),
            "queued a clip that was still being written"
        );

        writer.await.unwrap();
        let decision = decision_for(&h, "recording.mp4", Duration::from_secs(10))
            .await
            .expect("the finished clip should be decided on");

        assert_eq!(decision.outcome, "queued");
        assert_eq!(decision.reason, None);
        // 1 MB + 10 x 1 MB: proof it did not settle at the size it started at.
        assert_eq!(decision.size, 11 << 20, "settled before the writer finished");

        let _ = std::fs::remove_dir_all(&h.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn files_that_predate_the_folder_are_never_queued() {
        let h = harness().await;

        // Touching an existing file must not promote it out of baseline: it was
        // here before the folder was added, so it is the user's to send or not.
        let existing = h.dir.join("already-here.mp4");
        let meta = std::fs::metadata(&existing).unwrap();
        std::fs::File::options().append(true).open(&existing).unwrap();
        drop(meta);

        tokio::time::sleep(Duration::from_millis(900)).await;

        let rows = h.ledger.recent(100).unwrap();
        for name in ["already-here.mp4", "also-here.mp4"] {
            let row = rows.iter().find(|r| r.path.ends_with(name)).expect("row exists");
            assert_eq!(
                row.state,
                crate::ledger::FileState::Baseline,
                "{name} left baseline without being asked"
            );
        }

        let _ = std::fs::remove_dir_all(&h.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_under_the_floor_is_skipped_with_the_reason_kept() {
        let h = harness().await;
        write_sized(&h.dir.join("thumb.mp4"), 200 << 10);

        let decision = decision_for(&h, "thumb.mp4", Duration::from_secs(10))
            .await
            .expect("small file should still be decided on");

        assert_eq!(decision.outcome, "skipped");
        assert_eq!(decision.reason.as_deref(), Some("Under 5.0 MB"));

        let _ = std::fs::remove_dir_all(&h.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_restart_does_not_requeue_what_was_already_decided() {
        let h = harness().await;
        write_sized(&h.dir.join("clip.mp4"), 9 << 20);
        decision_for(&h, "clip.mp4", Duration::from_secs(10)).await.expect("queued once");

        let before = h.ledger.recent(500).unwrap().len();

        // Reopening the ledger and rescanning is what a relaunch does.
        let reopened = Ledger::open(&h.dir.join("ledger.sqlite")).unwrap();
        let folder = WatchedFolder {
            id: h.folder_id.clone(),
            path: h.dir.clone(),
            enabled: true,
            include_subfolders: false,
            media: vec![MediaKind::Video],
            dest_folder: None,
            game: None,
            min_size_bytes: Some(5 << 20),
            max_size_bytes: None,
            after_upload: crate::config::AfterUpload::Keep,
        };
        let present = scan_existing(&folder, &SupportedTypes::default());
        reopened.record_baseline(&folder.id, &present).unwrap();

        let rows = reopened.recent(500).unwrap();
        assert_eq!(rows.len(), before, "relaunch created duplicate rows");

        let clip = rows.iter().find(|r| r.path.ends_with("clip.mp4")).unwrap();
        assert_eq!(
            clip.state,
            crate::ledger::FileState::Queued,
            "relaunch demoted a queued clip back to baseline"
        );

        let _ = std::fs::remove_dir_all(&h.dir);
    }
}
