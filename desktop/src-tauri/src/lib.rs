mod bridge;
mod commands;
mod dto;
mod state;

use state::AppState;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::open,
            commands::close,
            commands::parse_progress,
            commands::children,
            commands::node_value,
            commands::node_path,
            commands::node_detail,
            commands::node_ancestors,
            commands::run_query,
            commands::text_search,
            commands::query_schema,
            commands::render_query,
            commands::export_query,
            commands::format_query,
            commands::tokenize,
            commands::completion_context,
            commands::grammar_manifest,
            commands::engine_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
