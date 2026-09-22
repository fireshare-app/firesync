mod api;
mod commands;
mod config;
mod error;
mod secrets;

use tauri::Manager;

use commands::AppState;

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
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            app.manage(AppState::new(app_data_dir));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::connect,
            commands::connection_status,
            commands::disconnect,
            commands::upload_options,
            commands::get_settings,
            commands::save_settings,
            commands::config_location,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
