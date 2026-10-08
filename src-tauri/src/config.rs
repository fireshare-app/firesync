use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

const CONFIG_FILE: &str = "settings.json";

/// What media a watched folder picks up. The server decides video or image from
/// the extension, but the client still needs to know which it is willing to
/// send — and which folder list to offer, since the two trees are separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Video,
    Image,
}

/// What happens to the local file once the server has it.
///
/// A capture folder fills a drive faster than anything else on a gaming
/// machine, so clearing it out is the point. `Trash` is the default of the two
/// removing options because it is recoverable; `Delete` is for when the reason
/// you turned this on was disk space, and a full trash does not give you any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterUpload {
    /// Leave it where it is.
    Keep,
    /// Move it to the OS trash.
    Trash,
    /// Remove it outright.
    Delete,
}

impl Default for AfterUpload {
    fn default() -> Self {
        AfterUpload::Keep
    }
}

/// One watched folder and the rules applied to everything it sends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedFolder {
    pub id: String,
    pub path: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub include_subfolders: bool,
    /// Which media kinds this folder sends. Empty is treated as "video only".
    #[serde(default)]
    pub media: Vec<MediaKind>,
    /// Destination on the server. Single level only: Fireshare's
    /// sanitize_upload_folder turns a `/` into a `-`, so `uploads/clips` would
    /// be filed as `uploads-clips`. Offering nesting would be a lie.
    #[serde(default)]
    pub dest_folder: Option<String>,
    /// Matched case-insensitively against games already in the library. A name
    /// that matches nothing is rejected by the server, so it comes from
    /// /options rather than being typed.
    #[serde(default)]
    pub game: Option<String>,
    #[serde(default)]
    pub min_size_bytes: Option<u64>,
    #[serde(default)]
    pub max_size_bytes: Option<u64>,
    /// Only ever acted on after the server has confirmed it has the file — a
    /// 201, meaning the bytes are written to its media directory, or a 409,
    /// meaning it already had them.
    #[serde(default)]
    pub after_upload: AfterUpload,
    /// Put uploads in whichever folder Fireshare already associates with this
    /// folder's game, rather than in `dest_folder`.
    ///
    /// On by default, because it is what somebody almost always means: the
    /// server tags anything scanned in a game's folder with that game, so
    /// filing a clip there gets it sorted and tagged without the upload naming
    /// a game at all — and without the `unknown_game` failure that naming one
    /// can cause. Falls back to `dest_folder` when the game has no folder of
    /// its own, since a guess would be worse than the explicit choice.
    #[serde(default = "default_true")]
    pub auto_sort_by_game: bool,
    /// Take each file's game from the name of the subfolder it is in, rather
    /// than from `game`. For a recorder that keeps one subfolder per game,
    /// which is most of them: watching the folder above them as one means a
    /// game played for the first time needs nothing added here. Implies
    /// `include_subfolders`.
    #[serde(default)]
    pub game_from_subfolder: bool,
    /// Subfolders whose game was chosen by hand, for when the name alone
    /// would pick the wrong game or none. Everything else is matched by name;
    /// see `games`.
    #[serde(default)]
    pub subfolder_games: Vec<SubfolderGame>,
    /// How uploads from this folder are titled, e.g. `{game} — {date}`. None
    /// leaves it to Fireshare, which uses the file name. See `titles`.
    #[serde(default)]
    pub title_template: Option<String>,
    /// Fireshare tags every upload from this folder gets. Ids, because that is
    /// what the upload takes; names come from Fireshare each time they are
    /// shown, so a rename there shows up here.
    #[serde(default)]
    pub tag_ids: Vec<i64>,
    #[serde(default)]
    pub watch_mode: WatchMode,
}

/// A subfolder's game, chosen by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubfolderGame {
    /// The subfolder's name, as it is on disk. Matched without regard to case,
    /// since Windows does not regard it either.
    pub subfolder: String,
    /// None: its files are sent without a game. Not the same as leaving the
    /// subfolder out of this list, which matches it by name.
    #[serde(default)]
    pub game: Option<String>,
}

/// How a folder learns that a file has arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchMode {
    /// Change events on a local disk, a timed scan on a network one.
    #[default]
    Auto,
    /// Change events, wherever the folder is.
    Events,
    /// List the folder on a timer and compare it with the ledger.
    Scan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSettings {
    #[serde(default = "default_true")]
    pub on_complete: bool,
    #[serde(default = "default_true")]
    pub on_needs_attention: bool,
    #[serde(default = "default_true")]
    pub quiet_in_fullscreen: bool,
    #[serde(default)]
    pub group_bursts: bool,
    /// Put a finished upload's link on the clipboard. Off by default: it
    /// replaces whatever was there, which nobody should have happen unasked.
    #[serde(default)]
    pub copy_link_on_complete: bool,
}

/// What uploads do while a game has the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhilePlaying {
    /// Carry on as usual.
    #[default]
    Full,
    /// Hold to `while_playing_cap`.
    Limit,
    /// Start nothing new until the game lets go of the screen.
    Pause,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferSettings {
    #[serde(default = "default_concurrency")]
    pub max_concurrent: u8,
    /// Bytes per second, or None for unlimited. Total, across every upload.
    #[serde(default)]
    pub speed_cap: Option<u64>,
    #[serde(default)]
    pub while_playing: WhilePlaying,
    /// Bytes per second while playing, when `while_playing` is `Limit`.
    #[serde(default)]
    pub while_playing_cap: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupSettings {
    #[serde(default)]
    pub launch_at_login: bool,
    #[serde(default = "default_true")]
    pub start_in_tray: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSettings {
    #[serde(default = "default_true")]
    pub auto_install: bool,
    // A `prerelease` switch used to live here: stored, never shown, never read,
    // and unable to work — the updater reads releases/latest, which GitHub never
    // points at a pre-release. Settings files that still carry it load fine;
    // unknown keys are ignored.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Normalised base URL. The token that goes with it lives in the keychain.
    #[serde(default)]
    pub server_url: Option<String>,
    /// What the server last said about this token.
    ///
    /// Kept so a launch that cannot reach the server can still open the app
    /// somebody has already set up. Being unable to re-check a token is not the
    /// same as the token being wrong, and treating it as such asked people to
    /// enter credentials that were never lost.
    #[serde(default)]
    pub last_check: Option<crate::api::discovery::TokenCheck>,
    #[serde(default)]
    pub folders: Vec<WatchedFolder>,
    #[serde(default)]
    pub notifications: NotificationSettings,
    #[serde(default)]
    pub transfers: TransferSettings,
    #[serde(default)]
    pub startup: StartupSettings,
    #[serde(default)]
    pub updates: UpdateSettings,
}

fn default_true() -> bool {
    true
}

fn default_concurrency() -> u8 {
    2
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            on_complete: true,
            on_needs_attention: true,
            quiet_in_fullscreen: true,
            group_bursts: false,
            copy_link_on_complete: false,
        }
    }
}

impl Default for TransferSettings {
    fn default() -> Self {
        Self {
            max_concurrent: default_concurrency(),
            speed_cap: None,
            while_playing: WhilePlaying::Full,
            while_playing_cap: None,
        }
    }
}

impl Default for StartupSettings {
    fn default() -> Self {
        // On by default: an uploader that only runs when you remember to start
        // it is not doing the job. Registered with the OS on first run only, so
        // turning it off afterwards stays off.
        Self { launch_at_login: true, start_in_tray: true }
    }
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { auto_install: true }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_url: None,
            last_check: None,
            folders: Vec::new(),
            notifications: NotificationSettings::default(),
            transfers: TransferSettings::default(),
            startup: StartupSettings::default(),
            updates: UpdateSettings::default(),
        }
    }
}

pub fn config_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(CONFIG_FILE)
}

/// Whether this machine has run Firesync before.
///
/// The first run is the only moment defaults may be acted on rather than merely
/// stored — after that, the config is somebody's choices, and re-applying a
/// default over the top would quietly undo them.
pub fn is_first_run(app_data_dir: &Path) -> bool {
    !config_path(app_data_dir).exists()
}

/// A missing or unreadable config is not fatal: the app starts with defaults and
/// the person reconnects. Refusing to launch because one JSON file went bad would
/// strand every watched folder over a problem that takes ten seconds to fix.
pub fn load(app_data_dir: &Path) -> Settings {
    let path = config_path(app_data_dir);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Settings::default();
    };
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        log::warn!("{} is not readable ({e}); starting with defaults", path.display());
        Settings::default()
    })
}

/// Written through a temp file and renamed, so a crash mid-write cannot leave a
/// truncated settings file behind — the same reason the server stages its
/// reassembled uploads.
pub fn save(app_data_dir: &Path, settings: &Settings) -> Result<()> {
    std::fs::create_dir_all(app_data_dir).map_err(|e| {
        AppError::Storage(format!("Could not create {}: {e}", app_data_dir.display()))
    })?;

    let path = config_path(app_data_dir);
    let staging = path.with_extension("json.tmp");

    let body = serde_json::to_string_pretty(settings)
        .map_err(|e| AppError::Storage(format!("Could not serialise settings: {e}")))?;

    std::fs::write(&staging, body)
        .map_err(|e| AppError::Storage(format!("Could not write {}: {e}", staging.display())))?;

    std::fs::rename(&staging, &path)
        .map_err(|e| AppError::Storage(format!("Could not save {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every install before this one saved `prerelease` into its settings. It
    /// must not stop them loading, or everybody's folders would reset.
    #[test]
    fn a_settings_file_with_the_retired_prerelease_key_still_loads() {
        let dir = std::env::temp_dir().join(format!("firesync-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            config_path(&dir),
            r#"{"server_url":"https://f.example","updates":{"auto_install":false,"prerelease":true},
                "folders":[{"id":"f1","path":"/w"}]}"#,
        )
        .unwrap();

        let settings = load(&dir);
        assert_eq!(settings.server_url.as_deref(), Some("https://f.example"));
        assert!(!settings.updates.auto_install);
        assert_eq!(settings.folders.len(), 1);
        assert!(!settings.folders[0].game_from_subfolder, "a folder from before is one game");
        assert!(settings.folders[0].subfolder_games.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A choice of "no game" and a subfolder left to match by name are
    /// different things, and both have to survive the round trip.
    #[test]
    fn a_subfolder_s_chosen_game_and_a_chosen_no_game_both_survive_saving() {
        let dir = std::env::temp_dir().join(format!("firesync-config-{}", uuid::Uuid::new_v4()));
        let mut settings = Settings::default();
        settings.folders.push(WatchedFolder {
            id: "f1".into(),
            path: PathBuf::from("/w"),
            enabled: true,
            include_subfolders: true,
            media: vec![MediaKind::Video],
            dest_folder: None,
            game: None,
            min_size_bytes: None,
            max_size_bytes: None,
            after_upload: AfterUpload::Keep,
            auto_sort_by_game: true,
            game_from_subfolder: true,
            subfolder_games: vec![
                SubfolderGame { subfolder: "cs2".into(), game: Some("Counter-Strike 2".into()) },
                SubfolderGame { subfolder: "Desktop".into(), game: None },
            ],
            title_template: None,
            tag_ids: Vec::new(),
            watch_mode: WatchMode::Auto,
        });
        save(&dir, &settings).unwrap();

        let back = load(&dir);
        assert!(back.folders[0].game_from_subfolder);
        assert_eq!(back.folders[0].subfolder_games, settings.folders[0].subfolder_games);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
