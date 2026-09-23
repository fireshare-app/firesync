mod api;
mod commands;
mod config;
mod error;
mod ledger;
mod notify;
mod queue;
mod secrets;
mod tray;
mod updater;
mod watcher;

use tauri::{Emitter, Manager};

use commands::AppState;
use watcher::settle::SettleConfig;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

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

            let (state, rx) = AppState::new(app_data_dir)?;
            let ledger = state.ledger.clone();
            let settings = state.settings.clone();
            let types = state.types.clone();

            // Decisions are pushed rather than polled: a clip can settle minutes
            // after the event that started the wait, long after any request the
            // UI made would have returned.
            let notifier = notify::Notifier::new(app.handle().clone(), state.settings.clone());

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
                ledger: state.ledger.clone(),
                settings: state.settings.clone(),
                control: state.queue.clone(),
                token: state.token.clone(),
                folder_rules: state.folder_rules.clone(),
                types: state.types.clone(),
                on_event: std::sync::Arc::new(move |event: queue::UploadEvent| {
                    // Progress ticks are for the window only. A toast every half
                    // second would be its own kind of failure.
                    if let Some(note) = note_for(&event) {
                        queue_notifier.post(note);
                    }
                    let _ = queue_handle.emit("firesync://upload", event);
                }),
            });

            app.manage(state);
            // So the "send a test notification" button can reach the same
            // notifier the queue uses, rather than testing a different one.
            app.manage(notifier.clone());
            // Before anything watches or scans, so both see the same spelling.
            app.state::<AppState>().tidy_stored_paths();
            let problems = app.state::<AppState>().resync_watchers();
            for problem in problems {
                eprintln!("firesync: {problem}");
            }

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
                    eprintln!("firesync: could not add the login item: {e}");
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
            commands::recent_activity,
            commands::watcher_problems,
            commands::test_notification,
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
