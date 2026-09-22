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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferSettings {
    #[serde(default = "default_concurrency")]
    pub max_concurrent: u8,
    /// Bytes per second, or None for unlimited.
    #[serde(default)]
    pub speed_cap: Option<u64>,
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
    #[serde(default)]
    pub prerelease: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Normalised base URL. The token that goes with it lives in the keychain.
    #[serde(default)]
    pub server_url: Option<String>,
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
        }
    }
}

impl Default for TransferSettings {
    fn default() -> Self {
        Self { max_concurrent: default_concurrency(), speed_cap: None }
    }
}

impl Default for StartupSettings {
    fn default() -> Self {
        Self { launch_at_login: false, start_in_tray: true }
    }
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { auto_install: true, prerelease: false }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_url: None,
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

/// A missing or unreadable config is not fatal: the app starts with defaults and
/// the person reconnects. Refusing to launch because one JSON file went bad would
/// strand every watched folder over a problem that takes ten seconds to fix.
pub fn load(app_data_dir: &Path) -> Settings {
    let path = config_path(app_data_dir);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Settings::default();
    };
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        eprintln!("firesync: {} is not readable ({e}); starting with defaults", path.display());
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
