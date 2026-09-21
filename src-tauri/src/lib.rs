pub mod commands;
pub mod corpus;
pub mod dict;
pub mod engine;
pub mod error;
pub mod model;
pub mod task;
pub mod translate;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::load_dictionary,
            commands::set_config,
            commands::start_trans,
            commands::stop_trans,
            commands::start_translate_file,
            commands::start_collect,
            commands::collect_offline,
            commands::list_injections,
            commands::detect_engine,
            commands::install_injection,
            commands::uninstall_injection,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
