mod api;
mod commands;
mod config;
mod diagnostics;
mod error;
mod ledger;
mod logging;
mod notify;
mod options;
mod queue;
mod releases;
mod secrets;
mod tray;
mod updater;
mod watcher;

use tauri::{Emitter, Manager};

use commands::AppState;
use watcher::settle::SettleConfig;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    logging::install_panic_hook();

    // First, so everything after it — the other plugins' setup included — has
    // somewhere to write.
    let mut builder = tauri::Builder::default().plugin(logging::plugin());

    // A second launch should surface the running copy, not start a rival that
    // watches the same folders and uploads everything twice.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // Launched by the OS, so it starts the way it will spend most of its
            // life: in the tray, with no window.
            Some(vec!["--tray"]),
        ));
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            let first_run = config::is_first_run(&app_data_dir);
            log::info!(
                "Firesync {} starting on {} ({})",
                app.package_info().version,
                os_info::get(),
                std::env::consts::ARCH
            );

            let (state, rx) = AppState::new(app_data_dir)?;

            // Managing the state is the very first thing done with it, because
            // the window is already up by the time setup runs and its first IPC
            // call was arriving before this line. Everything below took long
            // enough — opening the ledger, reading the keychain, starting two
            // loops — that `connection_status` could land in the gap and come
            // back "state not managed", which the window then had no choice but
            // to read as a connection that no longer worked.
            //
            // `manage` takes ownership, so the handles the loops need are taken
            // first. They are all Arcs; these are clones of the handle, not of
            // anything behind it.
            let ledger = state.ledger.clone();
            let settings = state.settings.clone();
            let types = state.types.clone();
            let queue_ledger = state.ledger.clone();
            let queue_settings = state.settings.clone();
            let queue_control = state.queue.clone();
            let queue_token = state.token.clone();
            let queue_options = state.options.clone();
            let queue_types = state.types.clone();

            let notifier = notify::Notifier::new(app.handle().clone(), settings.clone());
            app.manage(notifier.clone());
            app.manage(state);

            // Before anything watches or scans, so both see the same spelling.
            app.state::<AppState>().tidy_stored_paths();

            // Every refresh reaches whichever windows are open, whoever asked
            // for it — a dialog shows the game the queue's refresh just found.
            let options_handle = app.handle().clone();
            app.state::<AppState>().options.on_change(move |snapshot| {
                let _ = options_handle.emit("firesync://options", snapshot);
            });
            options::spawn_refresh_loop(app.handle().clone());

            // Decisions are pushed rather than polled: a clip can settle minutes
            // after the event that started the wait, long after any request the
            // UI made would have returned.
            let handle = app.handle().clone();
            watcher::spawn_event_loop(
                rx,
                ledger,
                settings,
                types,
                SettleConfig::default(),
                move |decision| {
                    let _ = handle.emit("firesync://decision", decision);
                },
            );

            // The upload queue reads the same ledger the watcher writes to, so
            // a clip settles, becomes queued, and is picked up without either
            // side knowing about the other.
            let queue_handle = app.handle().clone();
            let queue_notifier = notifier.clone();
            queue::spawn(queue::QueueDeps {
                ledger: queue_ledger,
                settings: queue_settings,
                control: queue_control,
                token: queue_token,
                options: queue_options,
                types: queue_types,
                on_event: std::sync::Arc::new(move |event: queue::UploadEvent| {
                    // Progress ticks are for the window only. A toast every half
                    // second would be its own kind of failure.
                    if let Some(note) = note_for(&event) {
                        queue_notifier.post(note);
                    }
                    let _ = queue_handle.emit("firesync://upload", event);
                }),
            });

            // Each folder that could not be watched is logged by the watcher
            // itself, once, rather than every time this list is asked for.
            // Attaching also catches each folder up on whatever arrived while
            // Firesync was not running.
            let _ = app.state::<AppState>().resync_watchers();
            spawn_upkeep(app.handle().clone());

            tray::build(app.handle())?;
            tray::spawn_status_loop(app.handle().clone());
            updater::spawn_check_loop(app.handle().clone());

            // Register the login item once, on the first run, so the default
            // actually takes effect rather than only being written down. Never
            // on later runs: by then the setting is a choice, and re-applying it
            // would undo somebody turning it off.
            if first_run {
                use tauri_plugin_autostart::ManagerExt;
                if let Err(e) = app.autolaunch().enable() {
                    log::warn!("Could not add the login item: {e}");
                }
            }

            // Hidden only when the OS started it, never when a person did.
            // Double-clicking an app and getting no window is hostile, and if
            // the tray ever fails to appear it would leave no way in at all —
            // so "start in the tray" governs the login launch, which is the
            // only one it was ever about.
            let launched_by_os = std::env::args().any(|a| a == "--tray");
            let start_hidden =
                launched_by_os && app.state::<AppState>().snapshot().startup.start_in_tray;
            if start_hidden {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::connect,
            commands::connection_status,
            commands::disconnect,
            commands::upload_options,
            commands::refresh_options,
            commands::add_folder,
            commands::remove_folder,
            commands::set_folder_enabled,
            commands::set_folder_after_upload,
            commands::update_folder,
            commands::list_folders,
            commands::upload_existing,
            commands::list_backlog,
            commands::check_backlog_against_library,
            commands::queue_backlog,
            commands::activity_page,
            commands::retry_file,
            commands::skip_file,
            commands::stop_upload,
            commands::upload_anyway,
            commands::reveal_file,
            commands::test_notification,
            commands::release_history,
            commands::get_settings,
            commands::save_settings,
            commands::queue_status,
            commands::pause_queue,
            commands::resume_queue,
            commands::retry_failed,
            commands::set_launch_at_login,
            commands::launch_at_login_state,
            commands::check_for_updates,
            commands::install_update,
            commands::update_blocked_by_upload,
            commands::open_main_window,
            commands::open_main_at,
            commands::quit_app,
            commands::config_location,
            commands::log_location,
            commands::open_log_dir,
            commands::diagnostics_report,
        ])
        .on_window_event(|window, event| {
            // Closing the window means "get out of my way", not "stop
            // uploading". Quitting is the tray's Quit item, which is the only
            // thing that should end a transfer in progress.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Every 30 seconds: reattach what came back, drop what went away, and give each
/// watched folder its rescans and sweeps. Blocking work — directory listings, and
/// on a share that has vanished a network timeout — so it runs off the async
/// workers.
fn spawn_upkeep(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let app = app.clone();
            let _ = tokio::task::spawn_blocking(move || app.state::<AppState>().upkeep()).await;
        }
    });
}

/// Which upload outcomes are worth interrupting somebody for.
fn note_for(event: &queue::UploadEvent) -> Option<notify::Note> {
    let name = std::path::Path::new(&event.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&event.path)
        .to_string();

    match event.state.as_str() {
        "done" => Some(notify::Note {
            title: "Upload complete".into(),
            body: event
                .landed_as
                .clone()
                .map(|landed| format!("{name} → {landed}"))
                .unwrap_or(name),
            needs_attention: false,
        }),
        "failed" | "paused" => Some(notify::Note {
            title: if event.state == "paused" {
                "Uploads paused".into()
            } else {
                "Upload needs your attention".into()
            },
            body: event
                .reason
                .clone()
                .map(|r| format!("{name} — {r}"))
                .unwrap_or(name),
            needs_attention: true,
        }),
        // A duplicate is a success with nothing to say, and a retry is still in
        // progress. Neither is worth a toast.
        _ => None,
    }
}
