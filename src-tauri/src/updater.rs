//! Checking for, and installing, new versions.
//!
//! The one rule that shapes this: an update must never interrupt an upload.
//! Firesync exists to move files that are often gigabytes and often on a slow
//! upstream, and restarting to apply a patch release halfway through one would
//! throw away more than the update is worth.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::commands::AppState;
use crate::error::{AppError, Result};
use crate::ledger::FileState;

/// Checked on launch, then once a day. More often would be noise: releases are
/// not frequent and nothing here is urgent enough to poll for.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// A short grace period after launch, so a check never competes with the first
/// folder scan or a queue that has work waiting from last time.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

/// Ask the release feed whether there is something newer.
pub async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>> {
    let updater = app
        .updater()
        .map_err(|e| AppError::Server(format!("Could not start the updater: {e}")))?;

    match updater.check().await {
        Ok(Some(update)) => Ok(Some(UpdateInfo {
            version: update.version.clone(),
            current_version: update.current_version.clone(),
            notes: update.body.clone(),
            date: update.date.map(|d| d.to_string()),
        })),
        Ok(None) => Ok(None),
        // Being offline, or a release feed that is not there yet, is not a
        // failure worth shouting about — it is the normal state of a machine
        // that has not been online today.
        Err(e) => Err(AppError::Unreachable(format!(
            "Could not check for updates: {e}"
        ))),
    }
}

/// Is anything in flight that an install would destroy?
pub fn busy_uploading(state: &AppState) -> bool {
    state.ledger.count_in_state(FileState::Uploading).unwrap_or(0) > 0
}

/// Download and install, then restart.
///
/// Refuses while an upload is in progress. A chunked upload can resume, but a
/// single-shot one cannot, and neither is worth losing to a patch release that
/// will still be there in ten minutes.
pub async fn install(app: AppHandle) -> Result<()> {
    {
        let state = app.state::<AppState>();
        if busy_uploading(&state) {
            return Err(AppError::Server(
                "An upload is still in progress. The update will be installed once it finishes."
                    .into(),
            ));
        }
    }

    let updater = app
        .updater()
        .map_err(|e| AppError::Server(format!("Could not start the updater: {e}")))?;

    let Some(update) = updater
        .check()
        .await
        .map_err(|e| AppError::Unreachable(format!("Could not check for updates: {e}")))?
    else {
        return Err(AppError::Server("There is no update to install.".into()));
    };

    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| AppError::Server(format!("Could not install the update: {e}")))?;

    app.restart();
}

/// Check on launch and daily thereafter, and install unattended when the
/// setting allows it and nothing is in flight.
pub fn spawn_check_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_AFTER).await;
        loop {
            let found = check(&app).await;

            match found {
                Ok(Some(info)) => {
                    let _ = app.emit("firesync://update", &info);

                    let auto = app
                        .try_state::<AppState>()
                        .map(|s| s.snapshot().updates.auto_install)
                        .unwrap_or(false);

                    if auto {
                        wait_for_idle_then_install(app.clone()).await;
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("firesync: {e}"),
            }

            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// Hold an automatic install until the queue is quiet.
///
/// An unattended update that waits is strictly better than one that interrupts:
/// nobody is watching, so there is nothing to be gained by being prompt, and a
/// three gigabyte transfer to lose by being impatient.
async fn wait_for_idle_then_install(app: AppHandle) {
    for _ in 0..(6 * 60) {
        let busy = app
            .try_state::<AppState>()
            .map(|s| busy_uploading(&s))
            .unwrap_or(true);
        if !busy {
            if let Err(e) = install(app.clone()).await {
                eprintln!("firesync: automatic update did not install: {e}");
            }
            return;
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    // Ten hours of continuous uploading is not a state to keep a pending
    // install alive through; the next daily check will offer it again.
    eprintln!("firesync: gave up waiting for the queue to be idle before updating");
}
