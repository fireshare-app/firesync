use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::Manager;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::api::client::normalize_base_url;
use crate::api::discovery::{check_token, media_exists, TokenCheck};
use crate::api::identity::video_id;
use crate::api::discovery::UploadOptions;
use crate::config::{self, AfterUpload, MediaKind, Settings, SubfolderGame, WatchedFolder};
use crate::error::{AppError, Result};
use crate::games::{self, SubfolderStatus};
use crate::ledger::{FileRow, FileState, Ledger};
use crate::options::{OptionsCache, OptionsSnapshot};
use crate::queue::rules::SupportedTypes;
use crate::queue::QueueControl;
use crate::secrets::TokenCache;
use crate::watcher::catchup::{self, Since, ARRIVED_WHILE_PAUSED};
use crate::watcher::{scan_existing, Availability, Watchers};

/// How often a watched folder is compared with the ledger even when nothing
/// suggests it needs it. The watch is the primary signal; this is the net under
/// it, for the events a platform drops without saying so.
const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// How often a folder that is scanned rather than watched is looked at. The
/// scan is its only way of noticing anything, so this is how late a new clip
/// can be.
pub const SCAN_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

pub type WatchEvent = (String, PathBuf);

pub struct AppState {
    pub app_data_dir: PathBuf,
    pub settings: Arc<Mutex<Settings>>,
    pub ledger: Arc<Ledger>,
    pub types: Arc<Mutex<SupportedTypes>>,
    /// Fireshare's folders, games and folder-to-game rules. One copy for the
    /// pickers and the queue alike, refreshed whenever either is about to rely
    /// on it.
    pub options: Arc<OptionsCache>,
    pub watchers: Mutex<Watchers>,
    pub queue: Arc<QueueControl>,
    pub token: Arc<TokenCache>,
    /// When each folder was last compared with the ledger.
    swept: Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

impl AppState {
    pub fn new(app_data_dir: PathBuf) -> Result<(Self, UnboundedReceiver<WatchEvent>)> {
        // Read from the keychain once, here, rather than on every pass of the
        // upload loop.
        Self::with_token(app_data_dir, TokenCache::load())
    }

    fn with_token(
        app_data_dir: PathBuf,
        token: TokenCache,
    ) -> Result<(Self, UnboundedReceiver<WatchEvent>)> {
        let settings = config::load(&app_data_dir);
        let ledger = Ledger::open(&app_data_dir.join("ledger.sqlite"))?;
        let (tx, rx): (UnboundedSender<WatchEvent>, _) = tokio::sync::mpsc::unbounded_channel();

        Ok((
            Self {
                app_data_dir,
                settings: Arc::new(Mutex::new(settings)),
                ledger: Arc::new(ledger),
                types: Arc::new(Mutex::new(SupportedTypes::default())),
                options: Arc::new(OptionsCache::new()),
                watchers: Mutex::new(Watchers::new(tx)),
                queue: Arc::new(QueueControl::new()),
                token: Arc::new(token),
                swept: Mutex::new(std::collections::HashMap::new()),
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
                    log::info!("Tidied {old} -> {new} ({moved} rows moved)");
                    folder.path = tidy;
                    changed = true;
                }
                // Leave the pair alone rather than split them.
                Err(e) => log::warn!("Could not move rows for {old}: {e}"),
            }
        }

        if changed {
            if let Err(e) = self.persist(settings) {
                log::warn!("Could not save tidied paths: {e}");
            }
        }
    }

    /// Bring watchers in line with the current folder list. Any folder that
    /// could not be watched is reported rather than silently dropped — a folder
    /// that looks active in the UI but is watching nothing is the worst outcome.
    ///
    /// A folder attached just now has a gap behind it — it was not watched
    /// until this moment — so it is caught up on straight away.
    pub fn resync_watchers(&self) -> Vec<String> {
        let folders = self.snapshot().folders;
        let unreachable: std::collections::HashSet<String> = folders
            .iter()
            .filter(|f| f.enabled && !f.path.is_dir())
            .map(|f| f.id.clone())
            .collect();
        let synced =
            self.watchers.lock().expect("watchers mutex").sync_with(&folders, &unreachable);
        for id in &synced.attached {
            if let Some(folder) = folders.iter().find(|f| &f.id == id) {
                self.catch_up(folder);
            }
        }
        synced.problems.into_iter().map(|(id, e)| format!("{id}: {e}")).collect()
    }

    /// Compare one folder with the ledger and act on the difference: see
    /// `watcher::catchup`. Logged rather than returned when it fails, because
    /// nothing that calls it could do better than try again next time.
    pub fn catch_up(&self, folder: &WatchedFolder) {
        let since = match self.ledger.folder_mark(&folder.id) {
            Ok(mark) => Since::from_mark(mark),
            Err(e) => {
                log::error!("Could not read how far {} was scanned: {e}", folder.path.display());
                return;
            }
        };
        let types = self.types.lock().expect("types mutex").clone();
        let send = self.watchers.lock().expect("watchers mutex").sender();
        match catchup::catch_up(folder, &types, &self.ledger, &send, &since) {
            Ok(_) => {
                self.swept
                    .lock()
                    .expect("swept mutex")
                    .insert(folder.id.clone(), std::time::Instant::now());
            }
            Err(e) => log::error!("Could not catch up on {}: {e}", folder.path.display()),
        }
    }

    /// What the watchers need doing every so often: folders that went away
    /// stopped, folders that came back attached and caught up on, rescans that
    /// a watch asked for, and the periodic sweep.
    pub fn upkeep(&self) {
        let _ = self.resync_watchers();

        let rescans = self.watchers.lock().expect("watchers mutex").take_rescans();
        for folder in self.snapshot().folders {
            let (running, scanning) = {
                let watchers = self.watchers.lock().expect("watchers mutex");
                (watchers.is_running(&folder.id), watchers.is_scanning(&folder.id))
            };
            if !running {
                continue;
            }
            let every = if scanning { SCAN_EVERY } else { SWEEP_EVERY };
            let swept_recently = self
                .swept
                .lock()
                .expect("swept mutex")
                .get(&folder.id)
                .is_some_and(|at| at.elapsed() < every);
            if rescans.contains(&folder.id) || !swept_recently {
                self.catch_up(&folder);
            }
        }
    }

    pub(crate) fn credentials(&self) -> Result<(String, String)> {
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

    let check = match check_token(&base_url, &token).await {
        Ok(check) => check,
        Err(e) => {
            log::warn!("Could not connect to {base_url}: {e}");
            return Err(e);
        }
    };
    log::info!("Connected to {base_url} as {}", check.username);

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

    // Possibly a different library altogether. Whatever was known about the
    // last one's games and folders is no longer worth offering, and the next
    // picker or upload to need it will ask this one.
    state.options.clear();

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

        Err(e) if needs_reconnect(&e) => {
            log::warn!("{server_url} no longer accepts the stored connection: {e}");
            Err(e)
        }

        // Everything else is about reaching the server, not about the token.
        // Carry on with what it told us last time, and say plainly at the top of
        // the window that this is what we are doing.
        Err(e) => match settings.last_check {
            Some(check) => {
                log::warn!("Could not re-check {server_url}; carrying on with its last answer: {e}");
                Ok(Some(Connection {
                    server_url,
                    check,
                    verified: false,
                    problem: Some(e.to_string()),
                }))
            }
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
    state.persist(next)?;
    state.options.clear();
    Ok(())
}

/// What Fireshare last said it will accept, without asking it again.
///
/// Instant, so a picker can open on this at once while `refresh_options`
/// fetches a newer copy behind it.
#[tauri::command]
pub fn upload_options(state: tauri::State<'_, AppState>) -> OptionsSnapshot {
    state.options.snapshot()
}

/// Ask Fireshare again, and answer with whatever is held afterwards: the new
/// list, or the old one along with why it could not be refreshed.
///
/// A failure to reach the server is part of the answer rather than an error,
/// because the picker asking still has a list to show and only wants to know
/// whether it can trust it. Only having nothing to ask with is an error.
#[tauri::command]
pub async fn refresh_options(state: tauri::State<'_, AppState>) -> Result<OptionsSnapshot> {
    let (url, token) = state.credentials()?;
    if let Err(e) = state.options.refresh(&url, &token).await {
        log::warn!("Could not refresh Fireshare's folders and games: {e}");
    }
    Ok(state.options.snapshot())
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
    #[serde(default)]
    pub game_from_subfolder: bool,
    #[serde(default)]
    pub subfolder_games: Vec<SubfolderGame>,
    #[serde(default)]
    pub title_template: Option<String>,
    #[serde(default)]
    pub tag_ids: Vec<i64>,
    #[serde(default)]
    pub watch_mode: crate::config::WatchMode,
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
    /// Files held for review: turned up while this folder was paused, say.
    pub held: i64,
    /// Why they were held, when they all share one reason.
    pub held_reason: Option<String>,
    pub availability: Availability,
    /// Why this folder is not being watched, when it is not.
    pub problem: Option<String>,
    /// Seconds between scans, for a folder scanned on a timer rather than
    /// watched: on a network drive, or set to scan.
    pub scanned_every: Option<u64>,
    /// Whether the folder's path is on a network drive, for the dialog to say
    /// what "automatically" chose.
    pub network: bool,
    /// Each subfolder and its game, for a folder that keeps one per game.
    /// Empty otherwise.
    pub subfolders: Vec<SubfolderStatus>,
}

/// Everything a folder card shows about one folder.
fn summarise(state: &AppState, folder: WatchedFolder, present_count: i64) -> Result<FolderSummary> {
    let subfolders = if folder.game_from_subfolder {
        games::status(&folder, state.options.snapshot().options.as_ref())
    } else {
        Vec::new()
    };
    let counts = state.ledger.counts(&folder.id)?;
    let last_upload_at = state.ledger.last_upload_at(&folder.id)?;
    let reasons = state.ledger.held_reasons(&folder.id)?;
    let held = reasons.iter().map(|(_, n)| n).sum();
    let held_reason = match reasons.as_slice() {
        [(reason, _)] => Some(reason.clone()),
        _ => None,
    };
    let (availability, problem, scanning) = {
        let watchers = state.watchers.lock().expect("watchers mutex");
        (
            watchers.availability(&folder),
            watchers.problem(&folder.id).map(str::to_string),
            watchers.is_scanning(&folder.id),
        )
    };
    let network = crate::watcher::network::is_network_path(&folder.path);
    Ok(FolderSummary {
        folder,
        counts,
        present_count,
        last_upload_at,
        held,
        held_reason,
        availability,
        problem,
        scanned_every: scanning.then_some(SCAN_EVERY.as_secs()),
        network,
        subfolders,
    })
}

/// A folder's subfolders with the game each would send, for the settings
/// dialog, which shows them for a folder not yet added too. `subfolder_games`
/// is the dialog's draft of the choices, so what it shows is what saving
/// would mean.
#[tauri::command]
pub fn list_subfolders(
    state: tauri::State<'_, AppState>,
    path: String,
    subfolder_games: Vec<SubfolderGame>,
) -> Vec<SubfolderStatus> {
    let probe = WatchedFolder {
        id: String::new(),
        path: PathBuf::from(path),
        enabled: true,
        include_subfolders: true,
        media: Vec::new(),
        dest_folder: None,
        game: None,
        min_size_bytes: None,
        max_size_bytes: None,
        after_upload: AfterUpload::Keep,
        auto_sort_by_game: true,
        game_from_subfolder: true,
        subfolder_games: games::tidy_choices(subfolder_games),
        title_template: None,
        tag_ids: Vec::new(),
        watch_mode: crate::config::WatchMode::Auto,
    };
    games::status(&probe, state.options.snapshot().options.as_ref())
}

/// Whether a folder not yet added is on a network drive, for the add dialog.
#[tauri::command]
pub fn detect_network(path: String) -> bool {
    crate::watcher::network::is_network_path(std::path::Path::new(&path))
}

/// What the folder's newest file would be titled, for the dialog's preview.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitlePreview {
    /// None: Fireshare would title it by its file name, which `from` is.
    pub title: Option<String>,
    /// The file the preview was made from, or None for a made-up example.
    pub from: Option<String>,
}

/// Render a title template the way an upload will, against the newest file in
/// the folder, so the preview cannot disagree with what is sent.
#[tauri::command]
pub fn preview_title(
    state: tauri::State<'_, AppState>,
    template: String,
    path: String,
    game: Option<String>,
    include_subfolders: bool,
    game_from_subfolder: Option<bool>,
    subfolder_games: Option<Vec<SubfolderGame>>,
) -> TitlePreview {
    let folder_path = PathBuf::from(&path);
    let per_game = game_from_subfolder.unwrap_or(false);
    let probe = WatchedFolder {
        id: String::new(),
        path: folder_path.clone(),
        enabled: true,
        include_subfolders: include_subfolders || per_game,
        media: vec![MediaKind::Video, MediaKind::Image],
        dest_folder: None,
        game: None,
        min_size_bytes: None,
        max_size_bytes: None,
        after_upload: AfterUpload::Keep,
        auto_sort_by_game: false,
        game_from_subfolder: per_game,
        subfolder_games: games::tidy_choices(subfolder_games.unwrap_or_default()),
        title_template: None,
        tag_ids: Vec::new(),
        watch_mode: crate::config::WatchMode::Auto,
    };
    let types = state.types.lock().expect("types mutex").clone();
    let newest = if folder_path.is_dir() {
        crate::watcher::scan_found(&probe, &types).into_iter().max_by_key(|f| f.mtime)
    } else {
        None
    };
    // In a folder per game, the newest file's game is its subfolder's, the
    // way the upload would have it.
    let game = if per_game {
        let library = state.options.snapshot().options;
        newest
            .as_ref()
            .and_then(|f| games::subfolder_of(&folder_path, std::path::Path::new(&f.path)))
            .and_then(|sub| games::resolve(&probe, &sub, library.as_ref()).game)
    } else {
        game.filter(|g| !g.trim().is_empty())
    };
    match newest {
        Some(file) => {
            let file = PathBuf::from(&file.path);
            TitlePreview {
                title: crate::titles::for_file(Some(&template), game.as_deref(), &folder_path, &file),
                from: file.file_name().map(|n| n.to_string_lossy().to_string()),
            }
        }
        None => TitlePreview {
            title: crate::titles::render(
                &template,
                &crate::titles::Facts {
                    file_stem: "clip",
                    game: game.as_deref(),
                    folder: folder_path.file_name().and_then(|n| n.to_str()).unwrap_or("clips"),
                    finished: chrono::Local::now(),
                },
            ),
            from: None,
        },
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
    // A folder per game is watched with its subfolders, whatever the box says.
    let include_subfolders = folder.include_subfolders || folder.game_from_subfolder;
    if include_subfolders {
        watched_inside(&existing, &path, None)?;
    }

    let media = if folder.media.is_empty() { vec![MediaKind::Video] } else { folder.media };
    let record = WatchedFolder {
        id: uuid::Uuid::new_v4().to_string(),
        path: path.clone(),
        enabled: true,
        include_subfolders,
        media,
        dest_folder: folder.dest_folder,
        game: folder.game,
        min_size_bytes: folder.min_size_bytes,
        max_size_bytes: folder.max_size_bytes,
        after_upload: folder.after_upload,
        auto_sort_by_game: folder.auto_sort_by_game,
        game_from_subfolder: folder.game_from_subfolder,
        subfolder_games: games::tidy_choices(folder.subfolder_games),
        title_template: folder.title_template.filter(|t| !t.trim().is_empty()),
        tag_ids: folder.tag_ids,
        watch_mode: folder.watch_mode,
    };

    // Snapshot what is already here BEFORE watching, so nothing that predates
    // the folder being added can be mistaken for something new.
    let types = state.types.lock().expect("types mutex").clone();
    let listed_at = unix_now();
    let present = scan_existing(&record, &types);
    let baseline_count = state.ledger.record_baseline(&record.id, &present, None)? as i64;
    // Complete as of the listing: anything that appears after it is new.
    state.ledger.mark_watched(&record.id, listed_at)?;

    if folder.upload_existing {
        let paths: Vec<String> = present.iter().map(|(p, _, _)| p.clone()).collect();
        state.ledger.promote_baseline(&record.id, &paths)?;
    }

    let mut next = state.snapshot();
    next.folders.push(record.clone());
    state.persist(next)?;
    state.resync_watchers();

    let present_count = baseline_count.max(present.len() as i64);
    summarise(&state, record, present_count)
}

/// The other way round from the check above: a folder watched with its
/// subfolders, with a folder already watched on its own inside it, would
/// upload that one's files twice too. `except` is the folder being changed,
/// which is of course inside itself.
fn watched_inside(folders: &[WatchedFolder], path: &std::path::Path, except: Option<&str>) -> Result<()> {
    if let Some(child) = folders
        .iter()
        .filter(|f| except != Some(f.id.as_str()))
        .find(|f| f.path != path && f.path.starts_with(path))
    {
        return Err(AppError::Storage(format!(
            "{} is inside this folder and already watched on its own. Stop watching it first, \
             or its clips would be uploaded twice.",
            child.path.display()
        )));
    }
    Ok(())
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
    state.set_folder_enabled(&id, enabled)
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
    #[serde(default)]
    pub game_from_subfolder: bool,
    #[serde(default)]
    pub subfolder_games: Vec<SubfolderGame>,
    #[serde(default)]
    pub title_template: Option<String>,
    #[serde(default)]
    pub tag_ids: Vec<i64>,
    #[serde(default)]
    pub watch_mode: crate::config::WatchMode,
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
    state.update_folder(&id, rules)
}

impl AppState {
    /// Pause or resume a folder.
    pub fn set_folder_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let mut next = self.snapshot();
        let Some(folder) = next.folders.iter_mut().find(|f| f.id == id) else {
            return Err(AppError::Storage("That folder is not being watched.".into()));
        };
        let pausing = folder.enabled && !enabled;
        folder.enabled = enabled;
        self.persist(next)?;
        // A pause is a choice not to upload, so whatever lands while it lasts
        // is held for review when the folder resumes rather than sent. Being
        // closed is not a choice about uploading, which is why that gap is
        // caught up on.
        if pausing {
            self.ledger.hold_next_scan(id, ARRIVED_WHILE_PAUSED)?;
        }
        self.resync_watchers();
        Ok(())
    }

    /// Change a folder's rules in place. See `update_folder`.
    pub fn update_folder(&self, id: &str, rules: FolderRules) -> Result<()> {
        let mut next = self.snapshot();
        let include_subfolders = rules.include_subfolders || rules.game_from_subfolder;
        if include_subfolders {
            let path = next.folders.iter().find(|f| f.id == id).map(|f| f.path.clone());
            if let Some(path) = path {
                watched_inside(&next.folders, &path, Some(id))?;
            }
        }
        let Some(folder) = next.folders.iter_mut().find(|f| f.id == id) else {
            return Err(AppError::Storage("That folder is not being watched.".into()));
        };

        let newly_recursive = !folder.include_subfolders && include_subfolders;
        folder.include_subfolders = include_subfolders;
        folder.media = if rules.media.is_empty() { vec![MediaKind::Video] } else { rules.media };
        folder.dest_folder = rules.dest_folder.filter(|s| !s.trim().is_empty());
        folder.game = rules.game.filter(|s| !s.trim().is_empty());
        folder.min_size_bytes = rules.min_size_bytes.filter(|n| *n > 0);
        folder.max_size_bytes = rules.max_size_bytes.filter(|n| *n > 0);
        folder.after_upload = rules.after_upload;
        folder.auto_sort_by_game = rules.auto_sort_by_game;
        folder.game_from_subfolder = rules.game_from_subfolder;
        folder.subfolder_games = games::tidy_choices(rules.subfolder_games);
        let per_game = folder.game_from_subfolder;
        folder.title_template = rules.title_template.filter(|t| !t.trim().is_empty());
        folder.tag_ids = rules.tag_ids;
        folder.watch_mode = rules.watch_mode;

        // Turning on subfolders brings in everything already in them, which is
        // exactly the situation of adding a folder: present before, so left alone.
        // Recorded before the watch restarts, so its catch-up finds them known.
        if newly_recursive {
            let types = self.types.lock().expect("types mutex").clone();
            let present = scan_existing(folder, &types);
            self.ledger.record_baseline(&folder.id, &present, None)?;
        }

        self.persist(next)?;
        // Recursion may have been turned on or off, which changes what is
        // watched.
        self.resync_watchers();
        // A choice just made may be the one some clip was waiting on.
        if per_game {
            self.release_unmatched(self.options.snapshot().options.as_ref());
        }
        Ok(())
    }

    /// Put back in the queue every upload that was waiting on a game for its
    /// subfolder and now has one: a choice made in the folder's settings, or
    /// a game added to the library since. Only those — one still unmatched
    /// would only fail the same way again.
    pub fn release_unmatched(&self, library: Option<&UploadOptions>) -> usize {
        let mut released = 0;
        for folder in self.snapshot().folders.iter().filter(|f| f.game_from_subfolder) {
            let Ok(waiting) = self.ledger.failed_for_reason(&folder.id, games::UNMATCHED) else {
                continue;
            };
            for row in waiting {
                let Some(subfolder) =
                    games::subfolder_of(&folder.path, std::path::Path::new(&row.path))
                else {
                    continue;
                };
                if games::resolve(folder, &subfolder, library).settled()
                    && self.ledger.retry_now(row.id).unwrap_or(false)
                {
                    released += 1;
                }
            }
        }
        if released > 0 {
            log::info!("{released} upload(s) whose folder now has a game are back in the queue");
        }
        released
    }
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
            let types = state.types.lock().expect("types mutex").clone();
            let present_count = scan_existing(&folder, &types).len() as i64;
            summarise(&state, folder, present_count)
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

/// What the activity feed asks for.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityQuery {
    pub tab: crate::ledger::ActivityTab,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub folder_id: Option<String>,
    pub limit: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPage {
    pub rows: Vec<ActivityRow>,
    pub counts: crate::ledger::ActivityCounts,
}

/// One page of the activity feed, and every tab's count under the same filter.
///
/// Filtered in the ledger rather than in the window: the window only ever has
/// a page, so counts and totals worked out there described the page.
#[tauri::command]
pub fn activity_page(state: tauri::State<'_, AppState>, query: ActivityQuery) -> Result<ActivityPage> {
    let filter = crate::ledger::ActivityFilter { name: query.name, folder_id: query.folder_id };
    let rows = state.ledger.activity_page(query.tab, &filter, query.limit.clamp(1, 1000))?;
    let counts = state.ledger.activity_counts(&filter)?;
    let base = state.settings.lock().expect("settings mutex").server_url.clone();
    let types = state.types.lock().expect("types mutex").clone();

    let rows = rows
        .into_iter()
        .map(|file| {
            let link = link_for(&file, base.as_deref(), &types);
            ActivityRow { file, link }
        })
        .collect();
    Ok(ActivityPage { rows, counts })
}

/// Queue a failed or waiting file now, with its attempts reset.
#[tauri::command]
pub fn retry_file(state: tauri::State<'_, AppState>, id: i64) -> Result<bool> {
    state.ledger.retry_now(id)
}

/// Leave a queued or failed file un-uploaded.
#[tauri::command]
pub fn skip_file(state: tauri::State<'_, AppState>, id: i64) -> Result<bool> {
    state.ledger.skip_by_user(id, "Skipped by you")
}

/// Stop an upload in flight. What it had sent is kept, so uploading it anyway
/// later carries on from there.
#[tauri::command]
pub fn stop_upload(state: tauri::State<'_, AppState>, id: i64) -> bool {
    state.queue.stop(id)
}

/// Queue a skipped file, whatever skipped it: a rule, or somebody.
#[tauri::command]
pub fn upload_anyway(state: tauri::State<'_, AppState>, id: i64) -> Result<bool> {
    state.ledger.upload_anyway(id)
}

/// Show a file in the system's file manager. A file removed after uploading,
/// or moved, opens the folder it was in instead, which is usually what
/// somebody asking to see it wants next.
#[tauri::command]
pub fn reveal_file(app: tauri::AppHandle, state: tauri::State<'_, AppState>, id: i64) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    let Some(row) = state.ledger.file(id)? else {
        return Err(AppError::Storage("Firesync has no record of that file.".into()));
    };
    let path = PathBuf::from(&row.path);
    let failed = |e: tauri_plugin_opener::Error| {
        AppError::Storage(format!("Could not open {}: {e}", path.display()))
    };
    if path.exists() {
        return app.opener().reveal_item_in_dir(&path).map_err(failed);
    }
    match path.parent().filter(|dir| dir.is_dir()) {
        Some(dir) => app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(failed),
        None => Err(AppError::Storage(format!(
            "{} and the folder it was in are no longer there.",
            path.display()
        ))),
    }
}

/// A finished upload, for the tray's Recent block.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentLink {
    pub id: i64,
    pub name: String,
    pub link: String,
    /// When it finished, in unix seconds.
    pub at: i64,
}

/// The newest uploads that can be opened, so a link is one click from the
/// tray instead of a trip through the main window.
#[tauri::command]
pub fn recent_links(state: tauri::State<'_, AppState>, limit: Option<i64>) -> Result<Vec<RecentLink>> {
    let base = state.settings.lock().expect("settings mutex").server_url.clone();
    let types = state.types.lock().expect("types mutex").clone();
    let rows = state.ledger.recent_finished(limit.unwrap_or(3).clamp(1, 20))?;
    Ok(rows
        .into_iter()
        .filter_map(|file| {
            let link = link_for(&file, base.as_deref(), &types)?;
            let name = std::path::Path::new(&file.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| file.path.clone());
            Some(RecentLink { id: file.id, name, link, at: file.updated_at })
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
// Troubleshooting
// ---------------------------------------------------------------------------

fn log_dir(app: &tauri::AppHandle) -> Result<PathBuf> {
    app.path()
        .app_log_dir()
        .map_err(|e| AppError::Storage(format!("Could not resolve the log directory: {e}")))
}

/// Where the log files are, for the Troubleshooting panel to show.
#[tauri::command]
pub fn log_location(app: tauri::AppHandle) -> Result<String> {
    Ok(log_dir(&app)?.display().to_string())
}

/// Open the log folder in the file manager, so the files can be attached to an
/// issue or read directly.
#[tauri::command]
pub fn open_log_dir(app: tauri::AppHandle) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    let dir = log_dir(&app)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Storage(format!("Could not create {}: {e}", dir.display())))?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| AppError::Storage(format!("Could not open {}: {e}", dir.display())))
}

/// The login name, for masking where it shows up outside the home folder.
fn os_user() -> Option<String> {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok()
}

/// Everything a bug report needs, ready to paste, with what identifies the
/// person masked. `include_server` keeps the server address, for the reports
/// where the server or the proxy in front of it is the problem.
#[tauri::command]
pub fn diagnostics_report(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    include_server: bool,
) -> Result<String> {
    use crate::diagnostics::{report, Facts, Masks, QueueFacts};

    let settings = state.snapshot();
    let folder_counts = settings
        .folders
        .iter()
        .map(|f| Ok((f.id.clone(), state.ledger.counts(&f.id)?)))
        .collect::<Result<Vec<_>>>()?;

    let facts = Facts {
        version: app.package_info().version.to_string(),
        os: format!("{} · {}", os_info::get(), std::env::consts::ARCH),
        queue: QueueFacts {
            paused: state.queue.is_paused(),
            pause_reason: state.queue.reason(),
            queued: state.ledger.count_in_state(FileState::Queued)?,
            uploading: state.ledger.count_in_state(FileState::Uploading)?,
            failed: state.ledger.count_in_state(FileState::Failed)?,
        },
        options: state.options.snapshot(),
        folder_counts,
        watcher_problems: state.resync_watchers(),
        log_lines: crate::logging::recent_lines(&log_dir(&app)?, 200),
        now_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        settings: settings.clone(),
    };

    let home = app.path().home_dir().ok();
    let masks = Masks::new(
        home.as_deref(),
        os_user().as_deref(),
        settings.last_check.as_ref().map(|c| c.username.as_str()),
        settings.server_url.as_deref(),
        include_server,
        state.token.get().as_deref(),
    );
    Ok(masks.apply(&report(&facts)))
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStatus {
    pub paused: bool,
    /// Waiting because a game has the screen, which is not a pause anybody
    /// made and has no Resume.
    pub held: bool,
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
        held: state.queue.is_held(),
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
    /// Why it was held for review, for files that did not predate the folder.
    pub held: Option<String>,
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
                held: row.reason,
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

#[cfg(test)]
mod catch_up_tests {
    use super::*;
    use crate::watcher::catchup::{ARRIVED_WHILE_PAUSED, FOUND_ON_UPDATE};

    struct World {
        state: AppState,
        rx: UnboundedReceiver<WatchEvent>,
        root: PathBuf,
        clips: PathBuf,
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// An app with one watched folder, `clips`, and no keychain.
    fn world(clips_exist: bool) -> World {
        let root = std::env::temp_dir().join(format!("firesync-app-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = crate::watcher::canonical(&root);
        let clips = root.join("clips");
        if clips_exist {
            std::fs::create_dir_all(&clips).unwrap();
        }
        let (state, rx) = AppState::with_token(root.join("data"), TokenCache::empty()).unwrap();
        let mut settings = state.snapshot();
        settings.folders.push(WatchedFolder {
            id: "f1".into(),
            path: clips.clone(),
            enabled: true,
            include_subfolders: false,
            media: vec![MediaKind::Video],
            dest_folder: None,
            game: None,
            min_size_bytes: None,
            max_size_bytes: None,
            after_upload: AfterUpload::Keep,
            auto_sort_by_game: false,
            game_from_subfolder: false,
            subfolder_games: Vec::new(),
            title_template: None,
            tag_ids: Vec::new(),
            watch_mode: crate::config::WatchMode::Auto,
        });
        state.save(settings).unwrap();
        World { state, rx, root, clips }
    }

    /// Names of the clips sent into the pipeline so far. Live watch events for
    /// directories can arrive too, and are not what these tests are about.
    fn sent(w: &mut World) -> Vec<String> {
        let mut names = Vec::new();
        while let Ok((_, path)) = w.rx.try_recv() {
            if path.extension().is_some_and(|e| e == "mp4") {
                names.push(path.file_name().unwrap().to_string_lossy().to_string());
            }
        }
        names
    }

    fn held(w: &World) -> Vec<(String, Option<String>)> {
        w.state
            .ledger
            .baseline_files("f1")
            .unwrap()
            .into_iter()
            .map(|r| (std::path::Path::new(&r.path).file_name().unwrap().to_string_lossy().to_string(), r.reason))
            .collect()
    }

    fn recently_complete(w: &World) {
        w.state.ledger.mark_watched("f1", unix_now() - 60).unwrap();
    }

    #[test]
    fn a_clip_written_while_closed_is_caught_up_at_launch() {
        let mut w = world(true);
        recently_complete(&w);
        std::fs::write(w.clips.join("while-closed.mp4"), b"clip").unwrap();

        w.state.resync_watchers();

        assert_eq!(sent(&mut w), vec!["while-closed.mp4"]);
    }

    #[test]
    fn what_arrives_during_a_pause_waits_for_review_when_it_resumes() {
        let mut w = world(true);
        recently_complete(&w);
        w.state.resync_watchers();

        w.state.set_folder_enabled("f1", false).unwrap();
        std::fs::write(w.clips.join("during-pause.mp4"), b"clip").unwrap();
        w.state.set_folder_enabled("f1", true).unwrap();

        assert!(sent(&mut w).is_empty(), "nothing from the pause should upload by itself");
        assert_eq!(held(&w), vec![("during-pause.mp4".into(), Some(ARRIVED_WHILE_PAUSED.into()))]);
    }

    #[test]
    fn the_first_launch_of_this_version_holds_rather_than_uploads() {
        let mut w = world(true);
        std::fs::write(w.clips.join("unseen.mp4"), b"clip").unwrap();

        w.state.resync_watchers();

        assert!(sent(&mut w).is_empty());
        assert_eq!(held(&w), vec![("unseen.mp4".into(), Some(FOUND_ON_UPDATE.into()))]);
    }

    #[test]
    fn a_folder_that_comes_back_is_watched_again_and_caught_up() {
        let mut w = world(false);
        recently_complete(&w);

        assert!(!w.state.resync_watchers().is_empty(), "a missing folder is a problem");
        let folder = w.state.snapshot().folders[0].clone();
        {
            let watchers = w.state.watchers.lock().unwrap();
            assert_eq!(watchers.availability(&folder), Availability::Unavailable);
            assert!(watchers.problem("f1").is_some());
        }

        std::fs::create_dir_all(&w.clips).unwrap();
        std::fs::write(w.clips.join("while-away.mp4"), b"clip").unwrap();
        w.state.upkeep();

        assert_eq!(w.state.watchers.lock().unwrap().availability(&folder), Availability::Watching);
        assert_eq!(sent(&mut w), vec!["while-away.mp4"]);
    }

    #[test]
    fn a_folder_that_goes_away_stops_being_watched() {
        let w = world(true);
        recently_complete(&w);
        w.state.resync_watchers();
        let folder = w.state.snapshot().folders[0].clone();

        std::fs::remove_dir_all(&w.clips).unwrap();
        w.state.upkeep();

        assert_eq!(w.state.watchers.lock().unwrap().availability(&folder), Availability::Unavailable);
    }

    /// No watch at all: the folder is listed every `SCAN_EVERY`, and that is
    /// how a new clip is found.
    #[test]
    fn a_folder_set_to_scan_is_scanned_rather_than_watched() {
        let mut w = world(true);
        recently_complete(&w);
        let mut settings = w.state.snapshot();
        settings.folders[0].watch_mode = crate::config::WatchMode::Scan;
        w.state.save(settings).unwrap();
        w.state.resync_watchers();
        assert!(w.state.watchers.lock().unwrap().is_scanning("f1"));

        std::fs::write(w.clips.join("on-the-nas.mp4"), b"clip").unwrap();
        w.state.upkeep();
        assert!(sent(&mut w).is_empty(), "not due yet, and nothing is watching");

        let long_ago = std::time::Instant::now() - SCAN_EVERY - std::time::Duration::from_secs(1);
        w.state.swept.lock().unwrap().insert("f1".into(), long_ago);
        w.state.upkeep();
        assert_eq!(sent(&mut w), vec!["on-the-nas.mp4"]);
    }

    /// Rules that change nothing about `world`'s folder.
    fn rules() -> FolderRules {
        FolderRules {
            include_subfolders: false,
            media: vec![MediaKind::Video],
            dest_folder: None,
            game: None,
            min_size_bytes: None,
            max_size_bytes: None,
            after_upload: AfterUpload::Keep,
            auto_sort_by_game: false,
            game_from_subfolder: false,
            subfolder_games: Vec::new(),
            title_template: None,
            tag_ids: Vec::new(),
            watch_mode: crate::config::WatchMode::Auto,
        }
    }

    fn choice(subfolder: &str, game: &str) -> SubfolderGame {
        SubfolderGame { subfolder: subfolder.into(), game: Some(game.into()) }
    }

    /// Whatever the box says, a folder per game has to see into its subfolders
    /// or it would see nothing at all.
    #[test]
    fn a_folder_per_game_is_watched_with_its_subfolders() {
        let w = world(true);
        w.state
            .update_folder("f1", FolderRules { game_from_subfolder: true, ..rules() })
            .unwrap();
        let folder = w.state.snapshot().folders[0].clone();
        assert!(folder.game_from_subfolder);
        assert!(folder.include_subfolders);
    }

    /// The clip that waited for its subfolder to have a game goes the moment
    /// it has one — and not before: a choice for some other subfolder is not
    /// about it.
    #[test]
    fn choosing_a_game_for_a_subfolder_releases_the_clips_that_waited_on_it() {
        let w = world(true);
        recently_complete(&w);
        w.state
            .update_folder("f1", FolderRules { game_from_subfolder: true, ..rules() })
            .unwrap();
        let sub = w.clips.join("Mystery Game");
        std::fs::create_dir_all(&sub).unwrap();
        let clip = crate::watcher::canonical(&sub.join("clip.mp4"));
        std::fs::write(&clip, b"clip").unwrap();
        let path = clip.to_string_lossy().to_string();
        w.state.ledger.observe("f1", &path, 4, 0, None).unwrap();
        let id = w.state.ledger.recent(10).unwrap().into_iter().find(|r| r.path == path).unwrap().id;
        w.state.ledger.mark_failed(id, &games::unmatched_reason("Mystery Game")).unwrap();

        w.state
            .update_folder(
                "f1",
                FolderRules {
                    game_from_subfolder: true,
                    subfolder_games: vec![choice("Some Other Game", "VALORANT")],
                    ..rules()
                },
            )
            .unwrap();
        assert_eq!(w.state.ledger.file(id).unwrap().unwrap().state, FileState::Failed, "not about it");

        w.state
            .update_folder(
                "f1",
                FolderRules {
                    game_from_subfolder: true,
                    subfolder_games: vec![choice("mystery game", "VALORANT")],
                    ..rules()
                },
            )
            .unwrap();
        let row = w.state.ledger.file(id).unwrap().unwrap();
        assert_eq!(row.state, FileState::Queued, "back in the queue");
        assert_eq!(row.reason, None);
    }

    /// Two watches on one clip would send it twice.
    #[test]
    fn a_folder_cannot_take_in_a_subfolder_that_is_watched_on_its_own() {
        let w = world(true);
        let inner = w.clips.join("VALORANT");
        std::fs::create_dir_all(&inner).unwrap();
        let mut settings = w.state.snapshot();
        let mut nested = settings.folders[0].clone();
        nested.id = "f2".into();
        nested.path = crate::watcher::canonical(&inner);
        settings.folders.push(nested);
        w.state.save(settings).unwrap();

        let err = w
            .state
            .update_folder("f1", FolderRules { game_from_subfolder: true, ..rules() })
            .unwrap_err();
        assert!(err.to_string().contains("already watched on its own"), "{err}");
        assert!(!w.state.snapshot().folders[0].include_subfolders, "nothing should have changed");
    }

    #[test]
    fn turning_on_subfolders_leaves_what_was_already_in_them() {
        let mut w = world(true);
        recently_complete(&w);
        w.state.resync_watchers();
        std::fs::create_dir_all(w.clips.join("older")).unwrap();
        std::fs::write(w.clips.join("older").join("from-last-year.mp4"), b"clip").unwrap();

        w.state
            .update_folder(
                "f1",
                FolderRules {
                    include_subfolders: true,
                    media: vec![MediaKind::Video],
                    dest_folder: None,
                    game: None,
                    min_size_bytes: None,
                    max_size_bytes: None,
                    after_upload: AfterUpload::Keep,
                    auto_sort_by_game: false,
                    game_from_subfolder: false,
                    subfolder_games: Vec::new(),
                    title_template: None,
                    tag_ids: Vec::new(),
                    watch_mode: crate::config::WatchMode::Auto,
                },
            )
            .unwrap();

        assert!(sent(&mut w).is_empty(), "turning on subfolders must not upload what is in them");
        assert_eq!(held(&w), vec![("from-last-year.mp4".into(), None)]);
    }
}
