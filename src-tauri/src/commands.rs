use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use tauri::Manager;

use crate::api::client::normalize_base_url;
use crate::api::discovery::{check_token, fetch_options, TokenCheck, UploadOptions};
use crate::config::{self, Settings};
use crate::error::{AppError, Result};
use crate::secrets;

pub struct AppState {
    pub app_data_dir: PathBuf,
    pub settings: Mutex<Settings>,
}

impl AppState {
    pub fn new(app_data_dir: PathBuf) -> Self {
        let settings = config::load(&app_data_dir);
        Self { app_data_dir, settings: Mutex::new(settings) }
    }

    fn snapshot(&self) -> Settings {
        self.settings.lock().expect("settings mutex poisoned").clone()
    }

    fn persist(&self, next: Settings) -> Result<()> {
        config::save(&self.app_data_dir, &next)?;
        *self.settings.lock().expect("settings mutex poisoned") = next;
        Ok(())
    }

    /// The URL and token to talk to the instance with, or a NotConnected error.
    fn credentials(&self) -> Result<(String, String)> {
        let url = self
            .snapshot()
            .server_url
            .ok_or_else(|| AppError::NotConnected("No Fireshare instance is set up yet.".into()))?;
        let token = secrets::load_token()?.ok_or_else(|| {
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
}

/// Validate a URL and token, and keep them only if they actually work.
///
/// Storing first and validating later would leave a broken instance configured
/// after a typo, and the person with no signal about which half was wrong.
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

    secrets::store_token(&token)?;
    let mut next = state.snapshot();
    next.server_url = Some(base_url.clone());
    state.persist(next)?;

    Ok(Connection { server_url: base_url, check })
}

/// Re-check the stored credentials. `Ok(None)` means nothing is set up yet,
/// which the UI shows as the first-run Connect screen rather than an error.
#[tauri::command]
pub async fn connection_status(state: tauri::State<'_, AppState>) -> Result<Option<Connection>> {
    let Some(server_url) = state.snapshot().server_url else {
        return Ok(None);
    };
    let Some(token) = secrets::load_token()? else {
        return Ok(None);
    };

    let check = check_token(&server_url, &token).await?;
    Ok(Some(Connection { server_url, check }))
}

#[tauri::command]
pub async fn disconnect(state: tauri::State<'_, AppState>) -> Result<()> {
    secrets::clear_token()?;
    let mut next = state.snapshot();
    next.server_url = None;
    state.persist(next)
}

/// The folders and games a watched folder may name.
#[tauri::command]
pub async fn upload_options(state: tauri::State<'_, AppState>) -> Result<UploadOptions> {
    let (url, token) = state.credentials()?;
    fetch_options(&url, &token).await
}

#[tauri::command]
pub fn get_settings(state: tauri::State<'_, AppState>) -> Settings {
    state.snapshot()
}

#[tauri::command]
pub fn save_settings(state: tauri::State<'_, AppState>, settings: Settings) -> Result<()> {
    state.persist(settings)
}

/// Where the config file lives, for the diagnostics view and for support.
#[tauri::command]
pub fn config_location(app: tauri::AppHandle) -> Result<String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Storage(format!("Could not resolve the app data directory: {e}")))?;
    Ok(config::config_path(&dir).display().to_string())
}
