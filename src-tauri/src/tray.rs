//! The tray icon, which is the app's real face.
//!
//! Firesync spends almost all of its life with no window open, so the tray is
//! not a shortcut to the UI — it is the UI, most of the time. It has to answer
//! "is it working?" without being clicked, and offer the handful of things
//! somebody would open the window for.
//!
//! It is built two different ways, because the platforms genuinely differ:
//!
//! * **Windows and macOS** get the styled panel on either mouse button and no
//!   native menu at all. Attaching a menu is precisely what makes Windows pop
//!   its own on right click, so having the panel on both buttons means having
//!   no menu to attach.
//! * **Linux** gets the native menu and only that. Tauri does not emit tray
//!   click events there — "Unsupported. The event is not emitted even though
//!   the icon is shown" — and the icon can fail to appear at all without a menu
//!   set, so the menu is not a fallback there, it is the whole interface.
//!
//! Which is why everything below that builds or updates a menu is Linux-only,
//! while the tooltip, the one part every platform shows, is not.

#[cfg(target_os = "linux")]
use std::sync::Mutex;
use std::time::Duration;

#[cfg(target_os = "linux")]
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::AppState;
use crate::ledger::FileState;

pub const OPEN: &str = "open";
pub const PAUSE: &str = "pause";
pub const BACKLOG: &str = "backlog";
pub const NOTIFY: &str = "notify";
pub const SETTINGS: &str = "settings";
pub const UPDATES: &str = "updates";
pub const QUIT: &str = "quit";

/// Handles to the items whose text or state changes while the app runs.
///
/// Kept rather than rebuilt: replacing the whole menu on a timer makes it
/// flicker and drops it if somebody has it open.
///
/// Linux only, because that is the only platform with a menu to keep current.
#[cfg(target_os = "linux")]
pub struct TrayItems {
    pub status: MenuItem<tauri::Wry>,
    pub pause: MenuItem<tauri::Wry>,
    pub notifications: CheckMenuItem<tauri::Wry>,
    pub updates: MenuItem<tauri::Wry>,
    pub last_line: Mutex<String>,
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    #[cfg(target_os = "linux")]
    let status = MenuItem::with_id(app, "status", "Firesync", false, None::<&str>)?;
    #[cfg(target_os = "linux")]
    let open = MenuItem::with_id(app, OPEN, "Open Firesync", true, None::<&str>)?;
    #[cfg(target_os = "linux")]
    let pause = MenuItem::with_id(app, PAUSE, "Pause all uploads", true, None::<&str>)?;
    #[cfg(target_os = "linux")]
    let backlog = MenuItem::with_id(app, BACKLOG, "Upload existing files…", true, None::<&str>)?;
    #[cfg(target_os = "linux")]
    let notifications = CheckMenuItem::with_id(app, NOTIFY, "Notifications", true, true, None::<&str>)?;
    #[cfg(target_os = "linux")]
    let settings = MenuItem::with_id(app, SETTINGS, "Settings", true, None::<&str>)?;
    #[cfg(target_os = "linux")]
    let updates = MenuItem::with_id(app, UPDATES, "Check for updates", true, None::<&str>)?;
    #[cfg(target_os = "linux")]
    let quit = MenuItem::with_id(app, QUIT, "Quit Firesync", true, None::<&str>)?;

    // Only Linux ever shows this. Tauri does not emit tray click events there
    // at all — "Unsupported. The event is not emitted even though the icon is
    // shown and will still show a context menu on right click" — and the icon
    // can fail to appear without a menu attached, so on Linux the native menu is
    // not a fallback, it is the entire interface.
    #[cfg(target_os = "linux")]
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &pause,
            &backlog,
            &PredefinedMenuItem::separator(app)?,
            &notifications,
            &settings,
            &updates,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    #[cfg(target_os = "linux")]
    app.manage(TrayItems {
        status: status.clone(),
        pause: pause.clone(),
        notifications: notifications.clone(),
        updates: updates.clone(),
        last_line: Mutex::new(String::new()),
    });

    #[allow(unused_mut)]
    let mut builder = TrayIconBuilder::with_id("firesync")
        .icon(app.default_window_icon().cloned().expect("bundled icon"))
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| on_menu(app, event.id.as_ref()))
        .on_tray_icon_event(|tray, event| {
            // Either button opens the panel the design draws.
            //
            // Attaching a menu is what makes Windows pop the native one on right
            // click, so the way to have the panel on both buttons is to not have
            // a menu at all — which is only survivable because Linux, the one
            // platform that needs the menu, never delivers these events anyway.
            if let TrayIconEvent::Click {
                button: MouseButton::Left | MouseButton::Right,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                // The rect can come back logical or physical depending on the
                // platform; the panel is placed in physical pixels either way.
                let scale = tray
                    .app_handle()
                    .get_webview_window("tray")
                    .and_then(|w| w.scale_factor().ok())
                    .unwrap_or(1.0);
                toggle_panel(
                    tray.app_handle(),
                    rect.position.to_physical(scale),
                    rect.size.to_physical(scale),
                );
            }
        });

    #[cfg(target_os = "linux")]
    {
        builder = builder.menu(&menu);
    }

    builder.build(app)?;

    Ok(())
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        OPEN => show_window(app),

        PAUSE => {
            let state = app.state::<AppState>();
            if state.queue.is_paused() {
                state.queue.resume();
            } else {
                state.queue.pause(None);
            }
        }

        // These open the window at the place that does the thing, rather than
        // dropping somebody on the front page to find it themselves.
        BACKLOG => {
            show_window(app);
            let _ = app.emit("firesync://navigate", "folders");
        }
        SETTINGS => {
            show_window(app);
            let _ = app.emit("firesync://navigate", "settings");
        }

        NOTIFY => {
            let state = app.state::<AppState>();
            let mut settings = state.snapshot();
            // One switch for "notify me at all", which is what somebody reaching
            // for the tray in the middle of a game actually wants.
            let on = !(settings.notifications.on_complete
                || settings.notifications.on_needs_attention);
            settings.notifications.on_complete = on;
            settings.notifications.on_needs_attention = on;
            let _ = state.save(settings);
        }

        UPDATES => {
            show_window(app);
            let _ = app.emit("firesync://navigate", "settings");
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                match crate::updater::check(&handle).await {
                    Ok(Some(info)) => {
                        let _ = handle.emit("firesync://update", info);
                    }
                    Ok(None) => {
                        let _ = handle.emit("firesync://update-none", ());
                    }
                    Err(e) => eprintln!("firesync: {e}"),
                }
            });
        }

        QUIT => app.exit(0),
        _ => {}
    }
}

pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Keep the tray describing what the queue is doing.
///
/// Polled rather than pushed: the queue's state changes for reasons that are
/// not events — a backoff coming due, a folder being paused — and a line of text
/// refreshed every couple of seconds is cheaper than making each of those an
/// event of its own.
pub fn spawn_status_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last_tooltip = String::new();
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            let Some(state) = app.try_state::<AppState>() else { continue };

            let paused = state.queue.is_paused();
            let uploading = state.ledger.count_in_state(FileState::Uploading).unwrap_or(0);
            let queued = state.ledger.count_in_state(FileState::Queued).unwrap_or(0);
            let failed = state.ledger.count_in_state(FileState::Failed).unwrap_or(0);
            let line = describe(paused, uploading, queued, failed);

            // The tooltip is the one part of this every platform shows, so it
            // is written before anything that only some of them have.
            if last_tooltip != line {
                if let Some(tray) = app.tray_by_id("firesync") {
                    let _ = tray.set_tooltip(Some(&format!("Firesync — {line}")));
                }
                last_tooltip = line.clone();
            }

            // The rest is the native menu, which only Linux has.
            #[cfg(target_os = "linux")]
            if let Some(items) = app.try_state::<TrayItems>() {
                // Only touch the menu when something actually changed: writing
                // the same text twice a second makes some tray implementations
                // redraw.
                let mut last = items.last_line.lock().expect("tray line mutex");
                if *last != line {
                    let _ = items.status.set_text(&line);
                    let _ = items
                        .pause
                        .set_text(if paused { "Resume uploads" } else { "Pause all uploads" });
                    *last = line.clone();
                }
                drop(last);

                let notifications = state.snapshot().notifications;
                let _ = items
                    .notifications
                    .set_checked(notifications.on_complete || notifications.on_needs_attention);
            }
        }
    });
}

/// Show a found version in the menu, so the tray carries the badge the design
/// draws rather than making somebody open the window to find out.
pub fn note_update(app: &AppHandle, version: &str) {
    #[cfg(target_os = "linux")]
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.updates.set_text(format!("Update to {version}"));
    }
    // Everywhere else the panel carries the badge, so there is nothing to mark.
    #[cfg(not(target_os = "linux"))]
    let _ = (app, version);
}

/// One line that answers "is it working?" without being clicked.
pub fn describe(paused: bool, uploading: i64, queued: i64, failed: i64) -> String {
    if paused {
        return "Paused".to_string();
    }
    let mut parts = Vec::new();
    if uploading > 0 {
        let total = uploading + queued;
        parts.push(if queued > 0 {
            format!("Uploading {uploading} of {total}")
        } else {
            format!("Uploading {uploading}")
        });
    } else if queued > 0 {
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

    /// The design's wording: how many of how many, not a bare count.
    #[test]
    fn work_in_progress_counts_the_whole_batch() {
        assert_eq!(describe(false, 1, 2, 0), "Uploading 1 of 3");
        assert_eq!(describe(false, 2, 0, 0), "Uploading 2");
        assert_eq!(describe(false, 0, 4, 0), "4 queued");
    }

    /// A tray that looks healthy while failures pile up is the wrong way round.
    #[test]
    fn failures_are_always_mentioned() {
        assert_eq!(describe(false, 0, 0, 2), "2 need attention");
        assert!(describe(false, 1, 1, 1).contains("need attention"));
    }
}

/// Show or hide the tray panel, positioned against the tray icon itself.
///
/// A native menu cannot be styled — it is drawn by the OS on every platform —
/// so the panel the design draws is a small borderless window instead. That
/// buys the appearance at the cost of having to do what a menu does for free:
/// place itself sensibly, and close when it loses focus.
fn toggle_panel(
    app: &AppHandle,
    position: tauri::PhysicalPosition<f64>,
    size: tauri::PhysicalSize<f64>,
) {
    let Some(panel) = app.get_webview_window("tray") else { return };

    if panel.is_visible().unwrap_or(false) {
        let _ = panel.hide();
        return;
    }

    if let Ok(panel_size) = panel.outer_size() {
        let (x, y) = anchor(app, &panel, position, size, panel_size);
        let _ = panel.set_position(tauri::PhysicalPosition::new(x, y));
    }

    let _ = panel.show();
    let _ = panel.set_focus();
}

/// Where the panel sits relative to the tray icon.
///
/// The tray is at the top on macOS and the bottom on Windows, and a panel that
/// assumed either would hang off the screen on the other — so the side is
/// decided by which half of the display the icon is actually in, and the
/// horizontal position is clamped so it never runs past an edge.
fn anchor(
    app: &AppHandle,
    panel: &tauri::WebviewWindow,
    icon_at: tauri::PhysicalPosition<f64>,
    icon_size: tauri::PhysicalSize<f64>,
    panel_size: tauri::PhysicalSize<u32>,
) -> (i32, i32) {
    const GAP: i32 = 6;

    let monitor = panel
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten());

    let icon_x = icon_at.x as i32;
    let icon_y = icon_at.y as i32;
    let icon_w = icon_size.width as i32;
    let icon_h = icon_size.height as i32;
    let pw = panel_size.width as i32;
    let ph = panel_size.height as i32;

    let mut x = icon_x + icon_w / 2 - pw / 2;
    let mut y = icon_y + icon_h + GAP;

    if let Some(monitor) = monitor {
        let area = monitor.size();
        let origin = monitor.position();
        let left = origin.x;
        let top = origin.y;
        let right = left + area.width as i32;
        let bottom = top + area.height as i32;

        // Tray at the bottom of the screen: the panel belongs above it.
        if icon_y > top + (area.height as i32 / 2) {
            y = icon_y - ph - GAP;
        }
        x = x.clamp(left + GAP, (right - pw - GAP).max(left + GAP));
        y = y.clamp(top + GAP, (bottom - ph - GAP).max(top + GAP));
    }

    (x, y)
}
