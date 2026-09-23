use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::Manager;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::api::client::normalize_base_url;
use crate::api::discovery::{check_token, fetch_options, media_exists, TokenCheck, UploadOptions};
use crate::api::identity::video_id;
use crate::config::{self, AfterUpload, MediaKind, Settings, WatchedFolder};
use crate::error::{AppError, Result};
use crate::ledger::{FileRow, FileState, Ledger};
use crate::queue::rules::SupportedTypes;
use crate::queue::QueueControl;
use crate::secrets::TokenCache;
use crate::watcher::{scan_existing, Watchers};

pub type WatchEvent = (String, PathBuf);

pub struct AppState {
    pub app_data_dir: PathBuf,
    pub settings: Arc<Mutex<Settings>>,
    pub ledger: Arc<Ledger>,
    pub types: Arc<Mutex<SupportedTypes>>,
    /// The server's folder-to-game mapping, refreshed whenever options are
    /// fetched. Cached because the queue needs it per upload and it changes
    /// about as often as somebody reorganises their library.
    pub folder_rules: Arc<Mutex<crate::api::discovery::FolderRules>>,
    pub watchers: Mutex<Watchers>,
    pub queue: Arc<QueueControl>,
    pub token: Arc<TokenCache>,
}

impl AppState {
    pub fn new(app_data_dir: PathBuf) -> Result<(Self, UnboundedReceiver<WatchEvent>)> {
        let settings = config::load(&app_data_dir);
        let ledger = Ledger::open(&app_data_dir.join("ledger.sqlite"))?;
        let (tx, rx): (UnboundedSender<WatchEvent>, _) = tokio::sync::mpsc::unbounded_channel();

        Ok((
            Self {
                app_data_dir,
                settings: Arc::new(Mutex::new(settings)),
                ledger: Arc::new(ledger),
                types: Arc::new(Mutex::new(SupportedTypes::default())),
                folder_rules: Arc::new(Mutex::new(Default::default())),
                watchers: Mutex::new(Watchers::new(tx)),
                queue: Arc::new(QueueControl::new()),
                // Read from the keychain once, here, rather than on every pass
                // of the upload loop.
                token: Arc::new(TokenCache::load()),
            },
            rx,
        ))
    }

    pub fn snapshot(&self) -> Settings {
        self.settings.lock().expect("settings mutex poisoned").clone()
    }

    /// Public face of `persist`, for callers outside the command layer such as
    /// the tray's notification toggle.
    pub fn save(&self, next: Settings) -> Result<()> {
        self.persist(next)
    }

    /// Change one part of the settings and write them back.
    fn mutate(&self, edit: impl FnOnce(&mut Settings)) -> Result<()> {
        let mut next = self.snapshot();
        edit(&mut next);
        self.persist(next)
    }

    fn persist(&self, next: Settings) -> Result<()> {
        config::save(&self.app_data_dir, &next)?;
        *self.settings.lock().expect("settings mutex poisoned") = next;
        Ok(())
    }

    /// Rewrite stored folder paths into their tidy form, moving the ledger's
    /// rows with them.
    ///
    /// Windows' `\\?\` prefix was stored verbatim by earlier versions. Dropping
    /// it from the config alone would leave every row keyed on the old spelling,
    /// so each file would look unseen and a folder's baseline would be re-queued
    /// as new — an upload of everything it was deliberately leaving alone. The
    /// two move together or not at all.
    pub fn tidy_stored_paths(&self) {
        let mut settings = self.snapshot();
        let mut changed = false;

        for folder in settings.folders.iter_mut() {
            let tidy = crate::watcher::simplified(&folder.path);
            if tidy == folder.path {
                continue;
            }
            let (old, new) = (folder.path.to_string_lossy().to_string(), tidy.to_string_lossy().to_string());
            match self.ledger.rewrite_path_prefix(&folder.id, &old, &new) {
                Ok(moved) => {
                    eprintln!("firesync: tidied {old} -> {new} ({moved} rows moved)");
                    folder.path = tidy;
                    changed = true;
                }
                // Leave the pair alone rather than split them.
                Err(e) => eprintln!("firesync: could not move rows for {old}: {e}"),
            }
        }

        if changed {
            if let Err(e) = self.persist(settings) {
                eprintln!("firesync: could not save tidied paths: {e}");
            }
        }
    }

    /// Bring watchers in line with the current folder list. Any folder that
    /// could not be watched is reported rather than silently dropped — a folder
    /// that looks active in the UI but is watching nothing is the worst outcome.
    pub fn resync_watchers(&self) -> Vec<String> {
        let folders = self.snapshot().folders;
        let problems = self.watchers.lock().expect("watchers mutex").sync(&folders);
        problems.into_iter().map(|(id, e)| format!("{id}: {e}")).collect()
    }

    fn credentials(&self) -> Result<(String, String)> {
        let url = self
            .snapshot()
            .server_url
            .ok_or_else(|| AppError::NotConnected("No Fireshare instance is set up yet.".into()))?;
        let token = self.token.get().ok_or_else(|| {
            AppError::NotConnected(
                "The upload token is missing from the keychain. Reconnect to store it again."
                    .into(),
            )
        })?;
        Ok((url, token))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub server_url: String,
    pub check: TokenCheck,
    /// Whether the server confirmed this just now, or whether these are the last
    /// answers it gave and could not be re-checked.
    pub verified: bool,
    /// Why it could not be re-checked, when it could not.
    pub problem: Option<String>,
}

/// Validate a URL and token, and keep them only if they actually work.
#[tauri::command]
pub async fn connect(
    state: tauri::State<'_, AppState>,
    url: String,
    token: String,
) -> Result<Connection> {
    let base_url = normalize_base_url(&url)?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(AppError::TokenRejected("Paste an upload token first.".into()));
    }

    let check = check_token(&base_url, &token).await?;

    // The server's own allowlist replaces the compiled-in default, so the rules
    // match what this instance would actually accept.
    *state.types.lock().expect("types mutex") = SupportedTypes {
        video: check.supported_video_types.clone(),
        image: check.supported_image_types.clone(),
    };

    state.token.set(&token)?;
    let mut next = state.snapshot();
    next.server_url = Some(base_url.clone());
    next.last_check = Some(check.clone());
    state.persist(next)?;

    // A working token is exactly the signal that clears an auth pause: the
    // queue stopped because the credential was bad, and it no longer is.
    state.queue.resume();

    Ok(Connection { server_url: base_url, check, verified: true, problem: None })
}

/// Whether this failure means somebody has to reconnect, or only that we could
/// not ask right now.
///
/// The three that qualify are all the server having answered: the token was
/// refused, the address is not usable, or whatever is there is not Fireshare.
/// A timeout, a gateway error or a dropped connection say nothing about the
/// credentials, and must not be allowed to look as though they do.
fn needs_reconnect(e: &AppError) -> bool {
    matches!(e, AppError::TokenRejected(_) | AppError::BadUrl(_) | AppError::NotFireshare(_))
}

#[tauri::command]
/// What we know about the configured server, without throwing away a working
/// setup over one failed request.
///
/// The distinction this draws is the whole point. "The server says this token is
/// no longer valid" means somebody has to reconnect. "I could not reach the
/// server" means nothing about the token at all — and collapsing the two asked
/// people to re-enter credentials that were still perfectly good, while the
/// queue behind the window carried on uploading with them.
pub async fn connection_status(state: tauri::State<'_, AppState>) -> Result<Option<Connection>> {
    let settings = state.snapshot();
    let Some(server_url) = settings.server_url else {
        return Ok(None);
    };
    let Some(token) = state.token.get() else {
        return Ok(None);
    };

    match check_token(&server_url, &token).await {
        Ok(check) => {
            *state.types.lock().expect("types mutex") = SupportedTypes {
                video: check.supported_video_types.clone(),
                image: check.supported_image_types.clone(),
            };
            // Remembered so the next launch has something to fall back on.
            state.mutate(|s| s.last_check = Some(check.clone()))?;
            Ok(Some(Connection { server_url, check, verified: true, problem: None }))
        }

        Err(e) if needs_reconnect(&e) => Err(e),

        // Everything else is about reaching the server, not about the token.
        // Carry on with what it told us last time, and say plainly at the top of
        // the window that this is what we are doing.
        Err(e) => match settings.last_check {
            Some(check) => Ok(Some(Connection {
                server_url,
                check,
                verified: false,
                problem: Some(e.to_string()),
            })),
            // Never successfully connected, so there is nothing to fall back to.
            None => Err(e),
        },
    }
}

#[tauri::command]
pub async fn disconnect(state: tauri::State<'_, AppState>) -> Result<()> {
    state.token.clear()?;
    let mut next = state.snapshot();
    next.server_url = None;
    state.persist(next)
}

#[tauri::command]
pub async fn upload_options(state: tauri::State<'_, AppState>) -> Result<UploadOptions> {
    let (url, token) = state.credentials()?;
    let options = fetch_options(&url, &token).await?;
    *state.folder_rules.lock().expect("folder rules mutex") = options.folder_rules.clone();
    Ok(options)
}

// ---------------------------------------------------------------------------
// Watched folders
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFolder {
    pub path: String,
    #[serde(default)]
    pub include_subfolders: bool,
    #[serde(default)]
    pub media: Vec<MediaKind>,
    #[serde(default)]
    pub dest_folder: Option<String>,
    #[serde(default)]
    pub game: Option<String>,
    #[serde(default)]
    pub min_size_bytes: Option<u64>,
    #[serde(default)]
    pub max_size_bytes: Option<u64>,
    #[serde(default)]
    pub after_upload: AfterUpload,
    #[serde(default = "default_true")]
    pub auto_sort_by_game: bool,
    /// Upload what is already there, instead of leaving it as baseline.
    #[serde(default)]
    pub upload_existing: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSummary {
    #[serde(flatten)]
    pub folder: WatchedFolder,
    /// state -> count, straight from the ledger.
    pub counts: Vec<(String, i64)>,
    /// When something from this folder last reached the server.
    pub last_upload_at: Option<i64>,
    /// Media files sitting in the folder right now.
    ///
    /// Counted from disk rather than from the ledger, because the ledger keeps
    /// a row for every file it has ever seen — including ones that have since
    /// been moved out, or removed after being uploaded. Reporting those as
    /// present would describe the folder as it was when it was added rather
    /// than as it is.
    pub present_count: i64,
}

#[tauri::command]
pub async fn add_folder(
    state: tauri::State<'_, AppState>,
    folder: NewFolder,
) -> Result<FolderSummary> {
    let path = PathBuf::from(&folder.path);
    if !path.is_dir() {
        return Err(AppError::Storage(format!(
            "{} is not a folder, or is not reachable from this machine.",
            path.display()
        )));
    }
    // Stored resolved, because that is the form the watcher reports paths in.
    // Keeping the raw string would let the same folder be added twice under two
    // names, and would file its files under two identities in the ledger.
    let path = crate::watcher::canonical(&path);

    let existing = state.snapshot().folders;
    if existing.iter().any(|f| f.path == path) {
        return Err(AppError::Storage("That folder is already being watched.".into()));
    }
    // A folder inside a folder already watched recursively would upload
    // everything twice.
    if let Some(parent) = existing
        .iter()
        .find(|f| f.include_subfolders && path.starts_with(&f.path))
    {
        return Err(AppError::Storage(format!(
            "{} is already covered by the watch on {}, which includes subfolders.",
            path.display(),
            parent.path.display()
        )));
    }

    let media = if folder.media.is_empty() { vec![MediaKind::Video] } else { folder.media };
    let record = WatchedFolder {
        id: uuid::Uuid::new_v4().to_string(),
        path: path.clone(),
        enabled: true,
        include_subfolders: folder.include_subfolders,
        media,
        dest_folder: folder.dest_folder,
        game: folder.game,
        min_size_bytes: folder.min_size_bytes,
        max_size_bytes: folder.max_size_bytes,
        after_upload: folder.after_upload,
        auto_sort_by_game: folder.auto_sort_by_game,
    };

    // Snapshot what is already here BEFORE watching, so nothing that predates
    // the folder being added can be mistaken for something new.
    let types = state.types.lock().expect("types mutex").clone();
    let present = scan_existing(&record, &types);
    let baseline_count = state.ledger.record_baseline(&record.id, &present)? as i64;

    if folder.upload_existing {
        let paths: Vec<String> = present.iter().map(|(p, _, _)| p.clone()).collect();
        state.ledger.promote_baseline(&record.id, &paths)?;
    }

    let mut next = state.snapshot();
    next.folders.push(record.clone());
    state.persist(next)?;
    state.resync_watchers();

    let counts = state.ledger.counts(&record.id)?;
    let present_count = baseline_count.max(present.len() as i64);
    Ok(FolderSummary { folder: record, counts, present_count, last_upload_at: None })
}

#[tauri::command]
pub async fn remove_folder(state: tauri::State<'_, AppState>, id: String) -> Result<()> {
    let mut next = state.snapshot();
    next.folders.retain(|f| f.id != id);
    state.persist(next)?;
    state.ledger.forget_folder(&id)?;
    state.resync_watchers();
    Ok(())
}

#[tauri::command]
pub async fn set_folder_enabled(
    state: tauri::State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<()> {
    let mut next = state.snapshot();
    let Some(folder) = next.folders.iter_mut().find(|f| f.id == id) else {
        return Err(AppError::Storage("That folder is not being watched.".into()));
    };
    folder.enabled = enabled;
    state.persist(next)?;
    state.resync_watchers();
    Ok(())
}

/// The rules for a folder, as the dialog edits them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRules {
    #[serde(default)]
    pub include_subfolders: bool,
    #[serde(default)]
    pub media: Vec<MediaKind>,
    #[serde(default)]
    pub dest_folder: Option<String>,
    #[serde(default)]
    pub game: Option<String>,
    #[serde(default)]
    pub min_size_bytes: Option<u64>,
    #[serde(default)]
    pub max_size_bytes: Option<u64>,
    #[serde(default)]
    pub after_upload: AfterUpload,
    #[serde(default = "default_true")]
    pub auto_sort_by_game: bool,
}

/// Change a folder's rules in place.
///
/// The path is not editable: it is the folder's identity, and the ledger keys
/// every file it has seen against it. Pointing an existing folder somewhere else
/// would inherit another directory's history, so that is a new folder.
///
/// Rules apply from now on. Files already decided keep their verdict — a floor
/// lowered after the fact does not retroactively un-skip anything, which is
/// what the backlog picker is for.
#[tauri::command]
pub async fn update_folder(
    state: tauri::State<'_, AppState>,
    id: String,
    rules: FolderRules,
) -> Result<()> {
    let mut next = state.snapshot();
    let Some(folder) = next.folders.iter_mut().find(|f| f.id == id) else {
        return Err(AppError::Storage("That folder is not being watched.".into()));
    };

    folder.include_subfolders = rules.include_subfolders;
    folder.media = if rules.media.is_empty() { vec![MediaKind::Video] } else { rules.media };
    folder.dest_folder = rules.dest_folder.filter(|s| !s.trim().is_empty());
    folder.game = rules.game.filter(|s| !s.trim().is_empty());
    folder.min_size_bytes = rules.min_size_bytes.filter(|n| *n > 0);
    folder.max_size_bytes = rules.max_size_bytes.filter(|n| *n > 0);
    folder.after_upload = rules.after_upload;
    folder.auto_sort_by_game = rules.auto_sort_by_game;

    state.persist(next)?;
    // Recursion may have been turned on or off, which changes what is watched.
    state.resync_watchers();
    Ok(())
}

/// Change what happens to a file after it uploads, without rebuilding the
/// folder. Destructive enough that it should be visible and reversible on the
/// card rather than buried in a config file.
#[tauri::command]
pub async fn set_folder_after_upload(
    state: tauri::State<'_, AppState>,
    id: String,
    after_upload: AfterUpload,
) -> Result<()> {
    let mut next = state.snapshot();
    let Some(folder) = next.folders.iter_mut().find(|f| f.id == id) else {
        return Err(AppError::Storage("That folder is not being watched.".into()));
    };
    folder.after_upload = after_upload;
    state.persist(next)
}

#[tauri::command]
pub fn list_folders(state: tauri::State<'_, AppState>) -> Result<Vec<FolderSummary>> {
    state
        .snapshot()
        .folders
        .into_iter()
        .map(|folder| {
            let counts = state.ledger.counts(&folder.id)?;
            let types = state.types.lock().expect("types mutex").clone();
            let present_count = scan_existing(&folder, &types).len() as i64;
            let last_upload_at = state.ledger.last_upload_at(&folder.id)?;
            Ok(FolderSummary { folder, counts, present_count, last_upload_at })
        })
        .collect()
}

/// Queue files that were already in the folder when it was added.
#[tauri::command]
pub fn upload_existing(
    state: tauri::State<'_, AppState>,
    folder_id: String,
    paths: Vec<String>,
) -> Result<usize> {
    state.ledger.promote_baseline(&folder_id, &paths)
}

/// A ledger row plus the one thing the UI cannot work out for itself.
///
/// The link is built here rather than in the webview because everything it
/// needs — the server address and which extensions the server calls a video —
/// already lives on this side, and duplicating the extension lists in
/// TypeScript would mean two places to be wrong.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRow {
    #[serde(flatten)]
    pub file: FileRow,
    /// Where this landed in Fireshare. `None` until the file has a hash and has
    /// actually arrived — an upload that failed has nothing to open.
    pub link: Option<String>,
}

#[tauri::command]
pub fn recent_activity(
    state: tauri::State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<ActivityRow>> {
    let rows = state.ledger.recent(limit.unwrap_or(200))?;
    let base = state.settings.lock().expect("settings mutex").server_url.clone();
    let types = state.types.lock().expect("types mutex").clone();

    Ok(rows
        .into_iter()
        .map(|file| {
            let link = link_for(&file, base.as_deref(), &types);
            ActivityRow { file, link }
        })
        .collect())
}

/// The page a row can be opened at, when there is one.
///
/// `duplicate` counts alongside `done`: the server said it already had these
/// bytes, which means the page exists — arguably the case where wanting the
/// link is most likely, since nothing new appeared to go looking for.
fn link_for(file: &FileRow, base: Option<&str>, types: &SupportedTypes) -> Option<String> {
    use crate::api::identity::media_url;

    if !matches!(file.state, FileState::Done | FileState::Duplicate) {
        return None;
    }
    let hash = file.content_hash.as_deref()?;
    let base = base?;

    let viewer = types.viewer_for(std::path::Path::new(&file.path))?;

    Some(media_url(base, hash, viewer))
}

/// Send a notification right now and report what the platform said.
///
/// Worth a button because "no toast appeared" has two completely different
/// causes — Firesync held it on purpose, or Windows declined to show it — and
/// from the outside they look the same. This separates them: it always attempts
/// delivery, and reports the suppression state alongside rather than obeying it.
#[tauri::command]
pub fn test_notification(
    notifier: tauri::State<'_, std::sync::Arc<crate::notify::Notifier>>,
) -> crate::notify::NotificationProbe {
    notifier.probe()
}

/// Every release, for the "what changed" window.
///
/// Not cached: it is asked for when somebody opens a panel, which is rare
/// enough that a stale answer would be a worse trade than a request.
#[tauri::command]
pub async fn release_history(limit: Option<u8>) -> Result<Vec<crate::releases::Release>> {
    crate::releases::history(limit.unwrap_or(15)).await
}

#[tauri::command]
pub fn watcher_problems(state: tauri::State<'_, AppState>) -> Vec<String> {
    state.resync_watchers()
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_settings(state: tauri::State<'_, AppState>) -> Settings {
    state.snapshot()
}

#[tauri::command]
pub fn save_settings(state: tauri::State<'_, AppState>, settings: Settings) -> Result<()> {
    state.persist(settings)?;
    state.resync_watchers();
    Ok(())
}

#[tauri::command]
pub fn config_location(app: tauri::AppHandle) -> Result<String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Storage(format!("Could not resolve the app data directory: {e}")))?;
    Ok(config::config_path(&dir).display().to_string())
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStatus {
    pub paused: bool,
    pub pause_reason: Option<String>,
    pub queued: i64,
    pub uploading: i64,
    pub failed: i64,
}

#[tauri::command]
pub fn queue_status(state: tauri::State<'_, AppState>) -> Result<QueueStatus> {
    use crate::ledger::FileState;
    Ok(QueueStatus {
        paused: state.queue.is_paused(),
        pause_reason: state.queue.reason(),
        queued: state.ledger.count_in_state(FileState::Queued)?,
        uploading: state.ledger.count_in_state(FileState::Uploading)?,
        failed: state.ledger.count_in_state(FileState::Failed)?,
    })
}

#[tauri::command]
pub fn pause_queue(state: tauri::State<'_, AppState>) {
    state.queue.pause(None);
}

#[tauri::command]
pub fn resume_queue(state: tauri::State<'_, AppState>) {
    state.queue.resume();
}

/// Clear the backoff on everything that failed and try again now.
#[tauri::command]
pub fn retry_failed(state: tauri::State<'_, AppState>) -> Result<usize> {
    let n = state.ledger.retry_failed()?;
    state.queue.resume();
    Ok(n)
}

/// Launch at login, kept in step with the OS rather than only in our config.
///
/// The setting and the registration can disagree — somebody removes the login
/// item themselves, or a reinstall drops it — so the OS is asked what it
/// actually has rather than trusted to match what we wrote down.
#[tauri::command]
pub fn set_launch_at_login(app: tauri::AppHandle, state: tauri::State<'_, AppState>, enabled: bool) -> Result<bool> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    let outcome = if enabled { manager.enable() } else { manager.disable() };
    outcome.map_err(|e| {
        AppError::Storage(format!("Could not change the login item: {e}"))
    })?;

    let actual = manager.is_enabled().unwrap_or(enabled);
    let mut next = state.snapshot();
    next.startup.launch_at_login = actual;
    state.persist(next)?;
    Ok(actual)
}

#[tauri::command]
pub fn launch_at_login_state(app: tauri::AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Updates
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Result<Option<crate::updater::UpdateInfo>> {
    crate::updater::check(&app).await
}

#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> Result<()> {
    crate::updater::install(app).await
}

/// Whether an install would interrupt something, so the UI can say "after this
/// upload" instead of offering a button that refuses.
#[tauri::command]
pub fn update_blocked_by_upload(state: tauri::State<'_, AppState>) -> bool {
    crate::updater::busy_uploading(&state)
}

// ---------------------------------------------------------------------------
// The backlog: files that were already there when a folder was added
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklogFile {
    pub path: String,
    pub name: String,
    pub size: i64,
    pub mtime: i64,
    /// Why this folder's rules would exclude it, if they would.
    pub excluded: Option<String>,
    /// Set once the library has been asked about it.
    pub in_library: Option<bool>,
}

/// Everything a folder is holding back, with the rules applied.
///
/// The rules are re-evaluated rather than trusted from when the file was
/// recorded: a folder's size limits or media kinds may have changed since, and
/// the picker should offer what would happen now, not what would have happened
/// then.
#[tauri::command]
pub fn list_backlog(state: tauri::State<'_, AppState>, folder_id: String) -> Result<Vec<BacklogFile>> {
    let settings = state.snapshot();
    let Some(folder) = settings.folders.iter().find(|f| f.id == folder_id) else {
        return Err(AppError::Storage("That folder is not being watched.".into()));
    };
    let types = state.types.lock().expect("types mutex").clone();

    Ok(state
        .ledger
        .baseline_files(&folder_id)?
        .into_iter()
        .map(|row| {
            let path = PathBuf::from(&row.path);
            let excluded =
                crate::queue::rules::evaluate(folder, &path, row.size.max(0) as u64, &types);
            BacklogFile {
                name: path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&row.path)
                    .to_string(),
                path: row.path,
                size: row.size,
                mtime: row.mtime,
                excluded,
                in_library: None,
            }
        })
        .collect())
}

/// Ask the library which of these it already has.
///
/// Separate from listing because it is the slow part: every file is hashed and
/// every hash is a round trip. The picker shows the list immediately and fills
/// this in behind it, rather than making somebody wait on the network to see
/// their own folder.
#[tauri::command]
pub async fn check_backlog_against_library(
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
) -> Result<Vec<(String, bool)>> {
    let (url, token) = state.credentials()?;
    let types = state.types.lock().expect("types mutex").clone();
    let mut answers = Vec::with_capacity(paths.len());

    for path in paths {
        // Images and videos are asked about under different names, though the
        // digest is computed identically for both.
        let Some(viewer) = types.viewer_for(std::path::Path::new(&path)) else {
            answers.push((path, false));
            continue;
        };
        let hashed = {
            let p = PathBuf::from(&path);
            tokio::task::spawn_blocking(move || video_id(&p)).await
        };
        // Only the first 16 MB is read, but that is still disk work, and a
        // folder of 200 clips is 200 of them.
        let Ok(Ok(id)) = hashed else {
            answers.push((path, false));
            continue;
        };
        match media_exists(&url, &token, &id, viewer).await {
            Ok(answer) => answers.push((path, answer.exists)),
            // An unreachable server should leave the picker usable rather than
            // failing the whole listing; "not known to be present" is the safe
            // reading, since it only ever means offering to upload something.
            Err(_) => answers.push((path, false)),
        }
    }
    Ok(answers)
}

/// Queue chosen files from the backlog.
#[tauri::command]
pub fn queue_backlog(
    state: tauri::State<'_, AppState>,
    folder_id: String,
    paths: Vec<String>,
) -> Result<usize> {
    state.ledger.promote_baseline(&folder_id, &paths)
}

// ---------------------------------------------------------------------------
// The tray panel
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn open_main_window(app: tauri::AppHandle) {
    crate::tray::show_window(&app);
}

#[tauri::command]
pub fn open_main_at(app: tauri::AppHandle, tab: String) {
    use tauri::Emitter;
    crate::tray::show_window(&app);
    let _ = app.emit("firesync://navigate", tab);
}

/// Quit for real, which is the one thing the window's close button does not do.
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod connection_tests {
    use super::*;

    /// Being unable to reach the server says nothing about the token, and the
    /// version that treated it as though it did asked people to re-enter
    /// credentials that were still working — while the queue behind the window
    /// carried on uploading with them.
    #[test]
    fn only_a_real_refusal_asks_somebody_to_reconnect() {
        assert!(needs_reconnect(&AppError::TokenRejected("gone".into())));
        assert!(needs_reconnect(&AppError::BadUrl("nope".into())));
        assert!(needs_reconnect(&AppError::NotFireshare("something else".into())));

        assert!(!needs_reconnect(&AppError::Unreachable("timed out".into())));
        assert!(!needs_reconnect(&AppError::Server("502".into())));
        assert!(!needs_reconnect(&AppError::Throttled("slow down".into())));
        assert!(!needs_reconnect(&AppError::Keychain("locked".into())));
    }
}
