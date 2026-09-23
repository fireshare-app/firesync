//! The release history, so "what changed" is answerable without leaving the app.
//!
//! Read from GitHub's releases API rather than from the updater: the updater
//! only ever knows about the one version it is offering, and the question
//! "what did I miss?" is usually about the three before that. Unauthenticated,
//! which is worth 60 requests an hour per address — far more than a window
//! somebody opens occasionally needs, and it keeps a token out of a client that
//! has no business holding one.

use serde::{Deserialize, Serialize};

use crate::api::client::http_client;
use crate::error::{AppError, Result};

const RELEASES_API: &str = "https://api.github.com/repos/fireshare-app/firesync/releases";

/// One published release, as the window shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    /// The tag with any leading `v` removed, so it compares with the running
    /// version without every caller having to remember to strip it.
    pub version: String,
    pub name: String,
    pub notes: String,
    pub published_at: Option<String>,
    pub url: String,
    pub prerelease: bool,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// The most recent releases, newest first.
pub async fn history(limit: u8) -> Result<Vec<Release>> {
    let client = http_client()?;
    let response = client
        .get(format!("{RELEASES_API}?per_page={}", limit.clamp(1, 30)))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| AppError::Unreachable("Could not reach GitHub for the release notes.".into()))?;

    if !response.status().is_success() {
        let code = response.status().as_u16();
        // The rate limit is the one failure worth naming, since it passes on
        // its own and telling somebody to try later is actionable.
        return Err(AppError::Server(if code == 403 || code == 429 {
            "GitHub is rate limiting this address. The notes should load again shortly.".into()
        } else {
            format!("GitHub answered {code} when asked for the release notes.")
        }));
    }

    let raw: Vec<GhRelease> = response
        .json()
        .await
        .map_err(|e| AppError::Server(format!("Could not read the release list: {e}")))?;

    Ok(shape(raw))
}

/// GitHub's shape into ours, kept apart from the request so it can be tested.
fn shape(raw: Vec<GhRelease>) -> Vec<Release> {
    raw.into_iter()
        // A draft is not a release anybody is running; unauthenticated requests
        // should not see one at all, but the flag is free to honour.
        .filter(|r| !r.draft)
        .map(|r| Release {
            version: r.tag_name.trim_start_matches('v').to_string(),
            name: r.name.unwrap_or_else(|| r.tag_name.clone()),
            notes: r.body.unwrap_or_default().trim().to_string(),
            published_at: r.published_at,
            url: r.html_url,
            prerelease: r.prerelease,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Vec<Release> {
        shape(serde_json::from_str(json).expect("fixture parses"))
    }

    /// The tag carries a `v`, the running version does not. Somewhere has to
    /// strip it, and doing it here means "is this the one I am running?" is a
    /// string comparison everywhere else.
    #[test]
    fn the_tag_loses_its_v_so_versions_compare() {
        let out = parse(r#"[{"tag_name":"v0.1.4","html_url":"u"}]"#);
        assert_eq!(out[0].version, "0.1.4");
    }

    #[test]
    fn a_draft_is_not_a_release_anybody_is_running() {
        let out = parse(
            r#"[{"tag_name":"v0.2.0","html_url":"u","draft":true},
                {"tag_name":"v0.1.4","html_url":"u"}]"#,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].version, "0.1.4");
    }

    /// A release with no notes is normal, not an error; the panel says so
    /// rather than showing an empty space somebody has to interpret.
    #[test]
    fn a_release_without_notes_is_kept_with_none() {
        let out = parse(r#"[{"tag_name":"v0.1.1","html_url":"u","body":"  \n "}]"#);
        assert_eq!(out.len(), 1);
        assert!(out[0].notes.is_empty());
        assert_eq!(out[0].name, "v0.1.1", "falls back to the tag when unnamed");
    }
}
