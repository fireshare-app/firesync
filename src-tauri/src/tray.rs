//! The tray icon, which is the app's real face.
//!
//! Firesync spends almost all of its life with no window open, so the tray is
//! not a shortcut to the UI — it is the UI, most of the time. It has to answer
//! "is it working?" without being clicked.

use std::time::Duration;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

use crate::commands::AppState;
use crate::ledger::FileState;

pub const OPEN: &str = "open";
pub const PAUSE: &str = "pause";
pub const QUIT: &str = "quit";

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Firesync", false, None::<&str>)?;
    let open = MenuItem::with_id(app, OPEN, "Open Firesync", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, PAUSE, "Pause all uploads", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT, "Quit Firesync", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &pause,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    TrayIconBuilder::with_id("firesync")
        .icon(app.default_window_icon().cloned().expect("bundled icon"))
        .menu(&menu)
        // The menu is the tray's whole interface on Linux, where a left click is
        // not reliably distinct from a right one.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            OPEN => show_window(app),
            PAUSE => {
                let state = app.state::<AppState>();
                if state.queue.is_paused() {
                    state.queue.resume();
                } else {
                    state.queue.pause(None);
                }
            }
            QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // A plain left click opens the window, which is what people expect
            // on Windows and costs nothing elsewhere.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

pub fn show_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Keep the tray's label and tooltip describing what the queue is doing.
///
/// Polled rather than pushed: the queue's state changes for reasons that are
/// not events — a backoff coming due, a folder being paused — and a line of
/// text refreshed every couple of seconds is cheaper than making every one of
/// those a notification.
pub fn spawn_status_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            let Some(state) = app.try_state::<AppState>() else { continue };
            let ledger = state.ledger.clone();
            let paused = state.queue.is_paused();

            let uploading = ledger.count_in_state(FileState::Uploading).unwrap_or(0);
            let queued = ledger.count_in_state(FileState::Queued).unwrap_or(0);
            let failed = ledger.count_in_state(FileState::Failed).unwrap_or(0);

            let line = describe(paused, uploading, queued, failed);

            if let Some(tray) = app.tray_by_id("firesync") {
                let _ = tray.set_tooltip(Some(&format!("Firesync — {line}")));
            }
        }
    });
}

/// One line that answers "is it working?" without being clicked.
pub fn describe(paused: bool, uploading: i64, queued: i64, failed: i64) -> String {
    if paused {
        return "Paused".to_string();
    }
    let mut parts = Vec::new();
    if uploading > 0 {
        parts.push(format!("Uploading {uploading}"));
    }
    if queued > 0 {
        parts.push(format!("{queued} queued"));
    }
    if failed > 0 {
        parts.push(format!("{failed} need attention"));
    }
    if parts.is_empty() {
        "Up to date".to_string()
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::describe;

    #[test]
    fn idle_says_so_rather_than_saying_nothing() {
        assert_eq!(describe(false, 0, 0, 0), "Up to date");
    }

    #[test]
    fn paused_overrides_everything_because_nothing_is_moving() {
        assert_eq!(describe(true, 3, 9, 1), "Paused");
    }

    #[test]
    fn work_in_progress_reads_left_to_right() {
        assert_eq!(describe(false, 1, 4, 0), "Uploading 1 · 4 queued");
    }

    /// A tray that looks healthy while failures pile up is the wrong way round.
    #[test]
    fn failures_are_always_mentioned() {
        assert_eq!(describe(false, 0, 0, 2), "2 need attention");
        assert!(describe(false, 1, 1, 1).contains("need attention"));
    }
}
