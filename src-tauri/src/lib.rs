pub mod audio;
pub mod db;
pub mod error;
pub mod export;
pub mod job_runner;
pub mod library;
pub mod llm;
pub mod models;
pub mod paths;
pub mod search;
pub mod transcribe;

use error::AppResult;
use library::{Importer, RecordingPatch};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::State;

pub struct AppState {
    pub db: Arc<db::Db>,
    pub data_root: PathBuf,
    pub cancel_flag: Arc<AtomicBool>,
    /// 正在跑转写的子进程（录音 id, pid）；取消时要杀掉它才能立即停止
    pub running_child: Arc<std::sync::Mutex<Option<(i64, u32)>>>,
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

// ---------- 转写（M3） ----------

#[tauri::command]
fn models_status(state: State<AppState>) -> Vec<models::ModelStatus> {
    models::ModelLayout::new(paths::models_dir(&state.data_root)).status()
}

#[tauri::command]
fn download_models(app: tauri::AppHandle, state: State<AppState>, ids: Vec<String>) -> AppResult<()> {
    let layout = models::ModelLayout::new(paths::models_dir(&state.data_root));
    models::download_models(app, layout, ids);
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Segment {
    id: i64,
    recording_id: i64,
    start_ms: i64,
    end_ms: i64,
    text: String,
    chars: String,
    speaker: Option<i64>,
    /// 是否被手工改过（重新转写会覆盖分段，用它提示用户）
    edited: bool,
}

#[tauri::command]
fn list_segments(state: State<AppState>, recording_id: i64) -> AppResult<Vec<Segment>> {
    let conn = state.db.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, recording_id, start_ms, end_ms, text, chars, speaker, edited
         FROM segments WHERE recording_id = ?1 ORDER BY start_ms",
    )?;
    let rows = stmt
        .query_map([recording_id], |r| {
            Ok(Segment {
                id: r.get(0)?,
                recording_id: r.get(1)?,
                start_ms: r.get(2)?,
                end_ms: r.get(3)?,
                text: r.get(4)?,
                chars: r.get(5)?,
                speaker: r.get(6)?,
                edited: r.get::<_, i64>(7)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---------- 转写稿编辑与说话人标注（M7） ----------

/// 解析字符时间轴 JSON：[[char, ms], ...]
fn parse_chars(json: &str) -> Vec<(String, i64)> {
    serde_json::from_str::<Vec<(String, i64)>>(json).unwrap_or_default()
}

/// 合并多个分段的时间轴：按字符是否重复去重，再统一偏移到合并后文本的坐标。
/// 分段重叠区（VAD 切分的尾巴）会重复同一句话，去重可避免合并后在交界处出现重复字符。
fn merge_chars(parts: &[String]) -> String {
    let mut all: Vec<(String, i64)> = Vec::new();
    for p in parts {
        all.extend(parse_chars(p));
    }
    let mut deduped: Vec<(String, i64)> = Vec::with_capacity(all.len());
    for (ch, ms) in all {
        if let Some((prev_ch, _)) = deduped.last() {
            if *prev_ch == ch {
                continue;
            }
        }
        deduped.push((ch, ms));
    }
    serde_json::to_string(&deduped).unwrap_or_else(|_| "[]".into())
}

/// 改写某分段文本（手工修正错字）。文本已变，原字符级时间轴不再对应，清空之。
#[tauri::command]
fn update_segment(state: State<AppState>, id: i64, text: String) -> AppResult<()> {
    let conn = state.db.conn.lock().unwrap();
    let n = conn.execute(
        "UPDATE segments SET text=?1, chars='[]', edited=1 WHERE id=?2",
        rusqlite::params![text, id],
    )?;
    if n == 0 {
        return Err(error::err("分段不存在（可能已被重新转写覆盖）"));
    }
    Ok(())
}

/// 删除某分段（识别成噪音或纯静音的空段）
#[tauri::command]
fn delete_segment(state: State<AppState>, id: i64) -> AppResult<()> {
    let conn = state.db.conn.lock().unwrap();
    conn.execute("DELETE FROM segments WHERE id=?1", [id])?;
    Ok(())
}

/// 合并若干相邻分段为一段：起点取首个、终点取末个，文本拼接，时间轴去重合并。
/// 要求同属一条录音且按时间相邻（允许中间有空隙，不允许跨越其它分段）。
pub fn merge_segments_impl(db: &db::Db, ids: &[i64]) -> AppResult<i64> {
    if ids.len() < 2 {
        return Err(error::err("至少选择两段才能合并"));
    }
    let conn = db.conn.lock().unwrap();
    let tx = conn.unchecked_transaction()?;

    let mut rows: Vec<(i64, i64, i64, i64, String, String)> = Vec::new();
    for id in ids {
        let row = tx
            .query_row(
                "SELECT id, recording_id, start_ms, end_ms, text, chars FROM segments WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .map_err(|_| error::err("分段不存在（可能已被重新转写覆盖）"))?;
        rows.push(row);
    }
    rows.sort_by_key(|r| r.2);

    let rec_id = rows[0].1;
    if rows.iter().any(|r| r.1 != rec_id) {
        return Err(error::err("不能跨录音合并分段"));
    }
    // 合并区间内不得夹着未选中的分段，否则会打乱稿子顺序
    let start = rows[0].2;
    let end = rows.iter().map(|r| r.3).max().unwrap_or(rows[0].3);
    let between: i64 = tx.query_row(
        "SELECT COUNT(*) FROM segments
         WHERE recording_id=?1 AND start_ms >= ?2 AND end_ms <= ?3 AND id NOT IN (SELECT value FROM json_each(?4))",
        rusqlite::params![rec_id, start, end, serde_json::to_string(ids).unwrap()],
        |r| r.get(0),
    )?;
    if between > 0 {
        return Err(error::err("所选分段之间夹着其它分段，请选择连续的分段"));
    }

    let keep = rows[0].0;
    let text = rows
        .iter()
        .map(|r| r.4.trim())
        .collect::<Vec<_>>()
        .join("");
    let chars = merge_chars(&rows.iter().map(|r| r.5.clone()).collect::<Vec<_>>());
    tx.execute(
        "UPDATE segments SET text=?1, chars=?2, start_ms=?3, end_ms=?4, edited=1 WHERE id=?5",
        rusqlite::params![text, chars, start, end, keep],
    )?;
    for r in rows.iter().skip(1) {
        tx.execute("DELETE FROM segments WHERE id=?1", [r.0])?;
    }
    tx.commit()?;
    Ok(keep)
}

#[tauri::command]
fn merge_segments(state: State<AppState>, ids: Vec<i64>) -> AppResult<i64> {
    merge_segments_impl(&state.db, &ids)
}

/// 标注某分段的说话人（1 起编号；传 null 清除）
#[tauri::command]
fn set_segment_speaker(state: State<AppState>, id: i64, speaker: Option<i64>) -> AppResult<()> {
    let conn = state.db.conn.lock().unwrap();
    conn.execute(
        "UPDATE segments SET speaker=?1, edited=1 WHERE id=?2",
        rusqlite::params![speaker, id],
    )?;
    Ok(())
}

/// 说话人显示名（内存字典，按录音存 settings）；未命名的按「说话人 N」展示
#[tauri::command]
fn get_speaker_names(
    state: State<AppState>,
    recording_id: i64,
) -> AppResult<HashMap<String, String>> {
    let key = format!("speakers.{recording_id}");
    match state.db.get_setting(&key)? {
        Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
        None => Ok(HashMap::new()),
    }
}

#[tauri::command]
fn set_speaker_names(
    state: State<AppState>,
    recording_id: i64,
    names: HashMap<String, String>,
) -> AppResult<()> {
    let mut m = HashMap::new();
    m.insert(
        format!("speakers.{recording_id}"),
        serde_json::to_string(&names).unwrap_or_else(|_| "{}".into()),
    );
    state.db.set_settings(&m)
}

/// 从时间中点把一段拆成两段（前端「拆」按钮）。
/// 有字符时间轴时按最接近中点的字符切分，两半各自保留自己的时间轴；
/// 没有时间轴（已被改过字的段）时按字符数均分文本。
/// 两半都会标记 edited —— 拆分本身也是手工修改。
pub fn split_segment_impl(db: &db::Db, id: i64, at_ms: i64) -> AppResult<i64> {
    let conn = db.conn.lock().unwrap();
    let tx = conn.unchecked_transaction()?;
    let (rec_id, start, end, text, chars): (i64, i64, i64, String, String) = tx
        .query_row(
            "SELECT recording_id, start_ms, end_ms, text, chars FROM segments WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|_| error::err("分段不存在（可能已被重新转写覆盖）"))?;
    if at_ms <= start || at_ms >= end {
        return Err(error::err("拆分点必须落在分段内部"));
    }

    let all = parse_chars(&chars);
    let (head_text, tail_text, head_chars, tail_chars) = if all.len() > 1 {
        let cut = all
            .iter()
            .position(|(_, ms)| *ms >= at_ms)
            .filter(|i| *i > 0 && *i < all.len())
            .unwrap_or(all.len() / 2);
        // 时间轴的字符数与文本一致时才用它做切分依据，否则退化为按文本长度均分
        let joined: String = all.iter().map(|(c, _)| c.as_str()).collect();
        if joined == text {
            (
                all[..cut].iter().map(|(c, _)| c.as_str()).collect::<String>(),
                all[cut..].iter().map(|(c, _)| c.as_str()).collect::<String>(),
                serde_json::to_string(&all[..cut]).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&all[cut..]).unwrap_or_else(|_| "[]".into()),
            )
        } else {
            let mid = text.char_indices().nth(text.chars().count() / 2).map(|(i, _)| i).unwrap_or(0);
            (text[..mid].to_string(), text[mid..].to_string(), "[]".into(), "[]".into())
        }
    } else {
        let mid = text.char_indices().nth(text.chars().count() / 2).map(|(i, _)| i).unwrap_or(0);
        (text[..mid].to_string(), text[mid..].to_string(), "[]".into(), "[]".into())
    };

    tx.execute(
        "UPDATE segments SET text=?1, chars=?2, end_ms=?3, edited=1 WHERE id=?4",
        rusqlite::params![head_text, head_chars, at_ms, id],
    )?;
    let tail_id = tx.query_row(
        "INSERT INTO segments(recording_id, start_ms, end_ms, text, chars, edited)
         VALUES(?1, ?2, ?3, ?4, ?5, 1) RETURNING id",
        rusqlite::params![rec_id, at_ms, end, tail_text, tail_chars],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok(tail_id)
}

#[tauri::command]
fn split_segment(state: State<AppState>, id: i64, at_ms: i64) -> AppResult<i64> {
    split_segment_impl(&state.db, id, at_ms)
}

/// 该录音有多少段被手工改过（重新转写前提示用）
pub fn count_edited_segments_impl(db: &db::Db, recording_id: i64) -> AppResult<i64> {
    let conn = db.conn.lock().unwrap();
    let n = conn.query_row(
        "SELECT COUNT(*) FROM segments WHERE recording_id=?1 AND edited=1",
        [recording_id],
        |r| r.get(0),
    )?;
    Ok(n)
}

#[tauri::command]
fn count_edited_segments(state: State<AppState>, recording_id: i64) -> AppResult<i64> {
    count_edited_segments_impl(&state.db, recording_id)
}

/// 入队一条转写任务（若该录音无未完成任务）
#[tauri::command]
fn transcribe(app: tauri::AppHandle, state: State<AppState>, recording_id: i64) -> AppResult<i64> {
    if !models::ModelLayout::new(paths::models_dir(&state.data_root)).all_ready() {
        return Err(error::err("转写模型未就绪，请先在设置中下载模型"));
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let conn = state.db.conn.lock().unwrap();
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE recording_id = ?1 AND kind='transcribe' AND status IN ('pending','running')",
        [recording_id],
        |r| r.get(0),
    )?;
    if active > 0 {
        return Err(error::err("该录音已有转写任务在进行"));
    }
    let job_id = conn.query_row(
        "INSERT INTO jobs(recording_id, kind, status, created_at, updated_at)
         VALUES(?1,'transcribe','pending',?2,?2) RETURNING id",
        rusqlite::params![recording_id, now],
        |r| r.get(0),
    )?;
    conn.execute(
        "UPDATE recordings SET status='queued', updated_at=?1 WHERE id=?2",
        rusqlite::params![now, recording_id],
    )?;
    drop(conn);
    use tauri::Emitter;
    let _ = app.emit(
        "job://progress",
        transcribe::JobEvent {
            recording_id,
            status: "queued".into(),
            progress: 0.0,
            error: String::new(),
        },
    );
    Ok(job_id)
}

/// 取消该录音的转写（排队中的直接出队，正在跑的下一次进度检查点退出）
#[tauri::command]
fn cancel_transcribe(app: tauri::AppHandle, state: State<AppState>, recording_id: i64) -> AppResult<()> {
    state.cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    // 正在跑的是这条录音时，直接杀掉它的子进程——否则要等当前这一大段解码完才停
    if let Some((running_rec, pid)) = *state.running_child.lock().unwrap() {
        if running_rec == recording_id {
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .output();
            }
            #[cfg(not(windows))]
            {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .output();
            }
        }
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let conn = state.db.conn.lock().unwrap();
    let affected = conn.execute(
        "UPDATE jobs SET status='canceled', updated_at=?1
         WHERE recording_id=?2 AND kind='transcribe' AND status IN ('pending','running')",
        rusqlite::params![now, recording_id],
    )?;
    if affected > 0 {
        // 回写录音状态，否则队列里取消一条会一直显示「排队中」；
        // 若该录音此刻已是 done（取消与 worker 完成竞争），不要把它降级回 imported
        conn.execute(
            "UPDATE recordings SET status='imported', updated_at=?1
             WHERE id=?2 AND status IN ('queued','transcribing')",
            rusqlite::params![now, recording_id],
        )?;
    }
    drop(conn);
    if affected > 0 {
        use tauri::Emitter;
        let _ = app.emit(
            "job://progress",
            transcribe::JobEvent {
                recording_id,
                status: "canceled".into(),
                progress: 0.0,
                error: String::new(),
            },
        );
    }
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: i64,
    pub recording_id: Option<i64>,
    pub kind: String,
    pub status: String,
    pub progress: f64,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// 关联录音标题（工作台队列要显示「哪条录音」）
    pub title: Option<String>,
    pub duration_sec: f64,
}

/// 队列快照：工作台展示「正在转写 / 排队中 / 最近完成」。
/// 排序把 running 置顶、pending 次之，其余按更新时间倒序 —— 与 worker 的 FIFO 取件一致。
pub fn query_jobs(db: &db::Db, active_only: bool) -> AppResult<Vec<JobInfo>> {
    let conn = db.conn.lock().unwrap();
    let filter = if active_only {
        "AND j.status IN ('pending','running')"
    } else {
        ""
    };
    let sql = format!(
        "SELECT j.id, j.recording_id, j.kind, j.status, j.progress, j.error,
                j.created_at, j.updated_at, r.title, r.duration_sec
         FROM jobs j
         LEFT JOIN recordings r ON r.id = j.recording_id
         WHERE 1=1 {filter}
         ORDER BY CASE j.status
                    WHEN 'running' THEN 0
                    WHEN 'pending' THEN 1
                    ELSE 2
                  END,
                  j.updated_at DESC,
                  j.id DESC
         LIMIT 200"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(JobInfo {
                id: r.get(0)?,
                recording_id: r.get(1)?,
                kind: r.get(2)?,
                status: r.get(3)?,
                progress: r.get(4)?,
                error: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
                title: r.get(8)?,
                duration_sec: r.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[tauri::command]
fn list_jobs(state: State<AppState>, active_only: bool) -> AppResult<Vec<JobInfo>> {
    query_jobs(&state.db, active_only)
}

#[tauri::command]
fn get_job(state: State<AppState>, recording_id: i64) -> AppResult<Option<JobInfo>> {
    let conn = state.db.conn.lock().unwrap();
    let v = conn
        .query_row(
            "SELECT j.id, j.recording_id, j.kind, j.status, j.progress, j.error,
                    j.created_at, j.updated_at, r.title, r.duration_sec
             FROM jobs j
             LEFT JOIN recordings r ON r.id = j.recording_id
             WHERE j.recording_id=?1 AND j.kind='transcribe'
             ORDER BY j.id DESC LIMIT 1",
            [recording_id],
            |r| {
                Ok(JobInfo {
                    id: r.get(0)?,
                    recording_id: r.get(1)?,
                    kind: r.get(2)?,
                    status: r.get(3)?,
                    progress: r.get(4)?,
                    error: r.get(5)?,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                    title: r.get(8)?,
                    duration_sec: r.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
                })
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    Ok(v)
}

// ---------- 检索 / AI 复盘 / 导出（M4） ----------

#[tauri::command]
fn search_all(state: State<AppState>, query: String) -> AppResult<search::SearchResults> {
    search::search(&state.db, &query)
}

#[tauri::command]
fn generate_summary(app: tauri::AppHandle, state: State<AppState>, recording_id: i64) -> AppResult<()> {
    llm::summarize(state.db.clone(), app, recording_id);
    Ok(())
}

#[tauri::command]
fn test_llm(state: State<AppState>) -> AppResult<String> {
    let conn = state.db.conn.lock().unwrap();
    let get = |key: &str| -> String {
        conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
            r.get::<_, String>(0)
        })
        .unwrap_or_default()
    };
    let (base_url, api_key, model) = (get("llm.baseUrl"), get("llm.apiKey"), get("llm.model"));
    drop(conn);
    llm::test_llm(&base_url, &api_key, &model)
}

#[tauri::command]
fn export_recording(state: State<AppState>, recording_id: i64, kind: String, dest: String) -> AppResult<()> {
    export::export(&state.db, recording_id, &kind, std::path::Path::new(&dest))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_root = paths::resolve_data_root().expect("解析数据目录失败");
    paths::ensure_dirs(&data_root).expect("初始化数据目录失败");
    let database = std::sync::Arc::new(db::Db::open(&data_root).expect("打开数据库失败"));
    let cancel_flag = Arc::new(AtomicBool::new(false));
    // 转写在子进程里跑（原生识别引擎的崩溃不会带走应用），这里记录当前子进程以便取消
    let running_child: Arc<std::sync::Mutex<Option<(i64, u32)>>> =
        Arc::new(std::sync::Mutex::new(None));

    // 应用上次异常退出遗留的任务：running → pending，录音状态归位
    {
        let conn = database.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let _ = conn.execute(
            "UPDATE jobs SET status='pending', updated_at=?1 WHERE kind='transcribe' AND status='running'",
            [&now],
        );
        let _ = conn.execute(
            "UPDATE recordings SET status='queued', updated_at=?1 WHERE status='transcribing'",
            [&now],
        );
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            db: database.clone(),
            data_root: data_root.clone(),
            cancel_flag: cancel_flag.clone(),
            running_child: running_child.clone(),
        })
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
            delete_bookmark,
            models_status,
            download_models,
            list_segments,
            transcribe,
            cancel_transcribe,
            get_job,
            list_jobs,
            update_segment,
            delete_segment,
            merge_segments,
            split_segment,
            set_segment_speaker,
            get_speaker_names,
            set_speaker_names,
            count_edited_segments,
            search_all,
            generate_summary,
            test_llm,
            export_recording
        ])
        .setup(move |app| {
            use tauri::Manager;
            // 允许 webview 通过 asset 协议读取库目录中的音频（数据根目录可配置，运行时扩展作用域）
            let state = app.state::<AppState>();
            let lib_dir = state.data_root.join("library");
            let _ = app.asset_protocol_scope().allow_directory(&lib_dir, true);

            // 启动长驻转写工作线程
            transcribe::TranscribeWorker::spawn(transcribe::TranscribeWorker {
                db: state.db.clone(),
                data_root: state.data_root.clone(),
                app: app.handle().clone(),
                cancel: state.cancel_flag.clone(),
                running: state.running_child.clone(),
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
