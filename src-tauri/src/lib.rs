pub mod commands;
pub mod config;
pub mod corpus;
pub mod dict;
pub mod engine;
pub mod error;
pub mod model;
pub mod sysfont;
pub mod task;
pub mod translate;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::load_config,
            commands::set_config,
            commands::load_dictionary,
            commands::start_translation,
            commands::start_collect,
            commands::start_translate_file,
            commands::stop_trans,
            commands::collect_offline,
            commands::convert_corpus,
            commands::convert_dict,
            commands::list_injections,
            commands::detect_engine,
            commands::list_system_fonts,
            commands::install_injection,
            commands::uninstall_injection,
            commands::generate_launcher,
            commands::remove_launcher,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app, event| {
        // 临时注入在 wtool 关闭时还原（持久注入由 cleanup_on_exit 自己跳过）。
        if let tauri::RunEvent::Exit = event {
            commands::cleanup_on_exit();
        }
    });
}
