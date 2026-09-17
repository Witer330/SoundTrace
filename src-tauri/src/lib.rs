mod audio;
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

// ---------- 播放器（M2） ----------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PeaksResult {
    peaks: Vec<f32>,
    duration_sec: f64,
}

/// 确保波形峰值已生成（无则现场解码计算），阻塞至完成；前端加载详情页时调用。
#[tauri::command]
async fn ensure_peaks(state: State<'_, AppState>, id: i64) -> AppResult<PeaksResult> {
    let (file_path, blob, duration) = {
        let conn = state.db.conn.lock().unwrap();
        conn.query_row(
            "SELECT file_path, peaks, duration_sec FROM recordings WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<Vec<u8>>>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            },
        )
        .map_err(|_| error::err("录音不存在"))?
    };

    if let Some(b) = blob {
        return Ok(PeaksResult {
            peaks: audio::blob_to_peaks(&b),
            duration_sec: duration,
        });
    }

    let path = file_path.clone();
    let peaks = tauri::async_runtime::spawn_blocking(move || audio::compute_peaks(std::path::Path::new(&path)))
        .await
        .map_err(|e| error::err(format!("峰值任务失败: {e}")))??;

    let blob = audio::peaks_to_blob(&peaks);
    {
        let conn = state.db.conn.lock().unwrap();
        conn.execute(
            "UPDATE recordings SET peaks = ?1 WHERE id = ?2",
            rusqlite::params![blob, id],
        )?;
    }
    Ok(PeaksResult { peaks, duration_sec: duration })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Bookmark {
    id: i64,
    recording_id: i64,
    time_ms: i64,
    label: String,
    note: String,
}

fn bookmark_cols() -> &'static str {
    "id, recording_id, time_ms, label, note"
}

#[tauri::command]
fn list_bookmarks(state: State<AppState>, recording_id: i64) -> AppResult<Vec<Bookmark>> {
    let conn = state.db.conn.lock().unwrap();
    let sql = format!(
        "SELECT {} FROM bookmarks WHERE recording_id = ?1 ORDER BY time_ms",
        bookmark_cols()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([recording_id], |r| {
            Ok(Bookmark {
                id: r.get(0)?,
                recording_id: r.get(1)?,
                time_ms: r.get(2)?,
                label: r.get(3)?,
                note: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[tauri::command]
fn add_bookmark(
    state: State<AppState>,
    recording_id: i64,
    time_ms: i64,
    label: String,
    note: String,
) -> AppResult<Bookmark> {
    let conn = state.db.conn.lock().unwrap();
    let id = conn.query_row(
        "INSERT INTO bookmarks(recording_id, time_ms, label, note) VALUES(?1,?2,?3,?4) RETURNING id",
        rusqlite::params![recording_id, time_ms, label, note],
        |r| r.get::<_, i64>(0),
    )?;
    Ok(Bookmark {
        id,
        recording_id,
        time_ms,
        label,
        note,
    })
}

#[tauri::command]
fn delete_bookmark(state: State<AppState>, id: i64) -> AppResult<()> {
    let conn = state.db.conn.lock().unwrap();
    conn.execute("DELETE FROM bookmarks WHERE id = ?1", [id])?;
    Ok(())
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
            expand_audio_paths,
            ensure_peaks,
            list_bookmarks,
            add_bookmark,
            delete_bookmark
        ])
        .setup(|app| {
            // 允许 webview 通过 asset 协议读取库目录中的音频（数据根目录可配置，运行时扩展作用域）
            use tauri::Manager;
            let data_root = app.state::<AppState>().data_root.clone();
            let lib_dir = data_root.join("library");
            let _ = app.asset_protocol_scope().allow_directory(&lib_dir, true);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
