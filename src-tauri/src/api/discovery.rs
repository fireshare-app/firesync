use serde::{Deserialize, Serialize};

use super::client::{describe_status_error, describe_transport_error, http_client};
use crate::error::{AppError, Result};

/// `GET /api/upload/token` — validates a token and reports what it may do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCheck {
    pub ok: bool,
    pub username: String,
    #[serde(default)]
    pub default_folder: Option<String>,
    #[serde(default)]
    pub images_enabled: bool,
    #[serde(default)]
    pub supported_video_types: Vec<String>,
    #[serde(default)]
    pub supported_image_types: Vec<String>,
}

/// `GET /api/upload/token/options` — the folders and games an upload may name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadOptions {
    #[serde(default)]
    pub default_folder: Option<String>,
    #[serde(default)]
    pub folders: FolderLists,
    #[serde(default)]
    pub games: Vec<Game>,
}

/// Video and image folders are listed separately because they are separate
/// directory trees on the server. A name valid for one is not necessarily valid
/// for the other, so the picker has to know which list it is offering.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FolderLists {
    #[serde(default)]
    pub video: Vec<String>,
    #[serde(default)]
    pub image: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub steamgriddb_id: Option<i64>,
}

fn host_of(base_url: &str) -> String {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| base_url.to_string())
}

async fn get_json<T: serde::de::DeserializeOwned>(
    base_url: &str,
    token: &str,
    path: &str,
) -> Result<T> {
    let client = http_client()?;
    let url = format!("{base_url}{path}");

    let response = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| describe_transport_error(&e, base_url))?;

    if !response.status().is_success() {
        return Err(describe_status_error(response, &host_of(base_url)).await);
    }

    // A reverse proxy or a captive portal will happily answer 200 with HTML.
    // Without this the failure surfaces as a serde message about expecting a
    // value, which tells the person nothing about what they got wrong.
    let body = response
        .text()
        .await
        .map_err(|e| describe_transport_error(&e, base_url))?;

    serde_json::from_str::<T>(&body).map_err(|_| {
        AppError::NotFireshare(format!(
            "{} answered, but not with Fireshare's API. Check that the address points at \
             Fireshare itself and not at a proxy or a login page in front of it.",
            host_of(base_url)
        ))
    })
}

pub async fn check_token(base_url: &str, token: &str) -> Result<TokenCheck> {
    get_json(base_url, token, "/api/upload/token").await
}

pub async fn fetch_options(base_url: &str, token: &str) -> Result<UploadOptions> {
    get_json(base_url, token, "/api/upload/token/options").await
}
