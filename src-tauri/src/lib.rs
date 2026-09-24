pub mod commands;
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
            commands::load_dictionary,
            commands::set_config,
            commands::start_trans,
            commands::stop_trans,
            commands::start_translate_file,
            commands::start_collect,
            commands::collect_offline,
            commands::list_injections,
            commands::detect_engine,
            commands::list_system_fonts,
            commands::install_injection,
            commands::uninstall_injection,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app, event| {
        // 注入是**临时**的：wtool 一关就把游戏目录还原。否则 Patch 常驻在那儿，
        // 每次开游戏都要白载一遍词典、装一遍钩子，启动平白慢几秒 —— 而且跟
        // wtool 在不在没关系，用户没处关。
        if let tauri::RunEvent::Exit = event {
            commands::cleanup_on_exit();
        }
    });
}
