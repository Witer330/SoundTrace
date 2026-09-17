mod db;
mod error;
mod library;
mod paths;

use error::AppResult;
use library::{Importer, RecordingPatch};
use std::collections::HashMap;
use std::path::PathBuf;
use tauri::State;

pub struct AppState {
    pub db: db::Db,
    pub data_root: PathBuf,
}

#[derive(serde::Serialize)]
struct AppInfo {
    version: String,
    data_root: String,
    default_data_root: String,
}

#[tauri::command]
fn get_app_info(state: State<AppState>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        data_root: state.data_root.to_string_lossy().to_string(),
        default_data_root: paths::default_root().to_string_lossy().to_string(),
    }
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> AppResult<HashMap<String, String>> {
    state.db.all_settings()
}

#[tauri::command]
fn set_settings(state: State<AppState>, entries: HashMap<String, String>) -> AppResult<()> {
    state.db.set_settings(&entries)
}

/// 修改数据根目录（写入 bootstrap，重启后生效）。不迁移数据，由用户自行拷贝旧目录。
#[tauri::command]
fn change_data_root(new_root: String) -> AppResult<()> {
    let path = PathBuf::from(&new_root);
    if !path.is_absolute() {
        return Err(error::err("数据目录必须是绝对路径"));
    }
    paths::set_data_root(&path)?;
    Ok(())
}

// ---------- 录音库管理（M1） ----------

#[tauri::command]
fn import_files(state: State<AppState>, paths: Vec<String>) -> AppResult<library::ImportResult> {
    let importer = Importer {
        db: &state.db,
        library_root: paths::library_dir(&state.data_root),
    };
    importer.import(&paths)
}

#[tauri::command]
fn list_recordings(state: State<AppState>) -> AppResult<Vec<library::Recording>> {
    let importer = Importer {
        db: &state.db,
        library_root: paths::library_dir(&state.data_root),
    };
    importer.list_recordings()
}

#[tauri::command]
fn get_recording(state: State<AppState>, id: i64) -> AppResult<Option<library::Recording>> {
    let importer = Importer {
        db: &state.db,
        library_root: paths::library_dir(&state.data_root),
    };
    importer.get_recording(id)
}

#[tauri::command]
fn update_recording(
    state: State<AppState>,
    id: i64,
    patch: RecordingPatch,
) -> AppResult<Option<library::Recording>> {
    let importer = Importer {
        db: &state.db,
        library_root: paths::library_dir(&state.data_root),
    };
    importer.update_recording(id, patch)
}

#[tauri::command]
fn delete_recording(state: State<AppState>, id: i64, delete_file: bool) -> AppResult<()> {
    let importer = Importer {
        db: &state.db,
        library_root: paths::library_dir(&state.data_root),
    };
    importer.delete_recording(id, delete_file)
}

/// 展开文件/目录路径为音频文件列表（供前端拖拽或选择文件夹后调用）
#[tauri::command]
fn expand_audio_paths(paths: Vec<String>) -> AppResult<Vec<String>> {
    library::expand_audio_paths(&paths)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_root = paths::resolve_data_root().expect("解析数据目录失败");
    paths::ensure_dirs(&data_root).expect("初始化数据目录失败");
    let database = db::Db::open(&data_root).expect("打开数据库失败");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState { db: database, data_root })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            get_settings,
            set_settings,
            change_data_root,
            import_files,
            list_recordings,
            get_recording,
            update_recording,
            delete_recording,
            expand_audio_paths
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
