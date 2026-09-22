mod api;
mod commands;
mod config;
mod error;
mod ledger;
mod queue;
mod secrets;
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
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;

            let (state, rx) = AppState::new(app_data_dir)?;
            let ledger = state.ledger.clone();
            let settings = state.settings.clone();
            let types = state.types.clone();

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
            queue::spawn(queue::QueueDeps {
                ledger: state.ledger.clone(),
                settings: state.settings.clone(),
                control: state.queue.clone(),
                token: state.token.clone(),
                on_event: std::sync::Arc::new(move |event| {
                    let _ = queue_handle.emit("firesync://upload", event);
                }),
            });

            app.manage(state);
            let problems = app.state::<AppState>().resync_watchers();
            for problem in problems {
                eprintln!("firesync: {problem}");
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
            commands::list_folders,
            commands::upload_existing,
            commands::recent_activity,
            commands::watcher_problems,
            commands::get_settings,
            commands::save_settings,
            commands::queue_status,
            commands::pause_queue,
            commands::resume_queue,
            commands::retry_failed,
            commands::config_location,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
