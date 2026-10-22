pub mod audio;
pub mod cpu;
pub mod db;
pub mod diar;
pub mod error;
pub mod export;
pub mod job_runner;
pub mod library;
pub mod llm;
pub mod models;
pub mod paths;
pub mod priority;
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
    /// 正在跑说话人分离的子进程（录音 id, pid, 最近进度）。与转写分开记录，两个引擎可以并行；
    /// 分离全局同时只允许一个（整段推理，并行只会互相抢核）。
    /// 进度记在槽里而不是页面状态里：切走页面再回来，状态依然可查。
    pub running_diarize: Arc<std::sync::Mutex<Option<(i64, u32, f64)>>>,
    /// 分离的取消标记：cancel_diarize 置位并杀子进程，工作线程据此回报 canceled
    pub diarize_cancel: Arc<AtomicBool>,
}

/// 前端契约见 `src/types.ts` 的 `AppInfo`：字段必须 camelCase，
/// 否则前端读到的是 undefined（存储卡片会一直显示「…」）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    data_root: String,
    default_data_root: String,
    /// 逻辑核心数：设置页「转写性能」用它换算并展示实际线程数
    logical_cores: usize,
    /// 「自动」档实际会用的线程数（物理核心数，至少留 2 个逻辑核心）。
    /// 由后端算好，避免前端另写一套换算逻辑而与识别器实际取值走偏。
    auto_threads: usize,
}

#[tauri::command]
fn get_app_info(state: State<AppState>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        data_root: state.data_root.to_string_lossy().to_string(),
        default_data_root: paths::default_root().to_string_lossy().to_string(),
        logical_cores: cpu::logical_cores(),
        auto_threads: cpu::auto_threads(),
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

/// 导入一组文件/录音。
///
/// 必须是 async：同步命令在 Tauri 主线程上执行，而导入要做「全量哈希 + ffprobe +
/// 必要时 ffmpeg 转码 + 复制文件」，几 GB 的目录会让界面整段时间无法重绘（Windows
/// 直接标记为未响应）。放到阻塞线程池后界面保持可用。
#[tauri::command]
async fn import_files(
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> AppResult<library::ImportResult> {
    let db = state.db.clone();
    let library_root = paths::library_dir(&state.data_root);
    tauri::async_runtime::spawn_blocking(move || {
        let importer = Importer {
            db: &db,
            library_root,
        };
        importer.import(&paths)
    })
    .await?
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

/// 展开文件/目录路径为音频文件列表（供前端拖拽或选择文件夹后调用）。
/// 递归扫描可能很慢（大目录树），同样放阻塞线程池，避免冻住界面。
#[tauri::command]
async fn expand_audio_paths(paths: Vec<String>) -> AppResult<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || library::expand_audio_paths(&paths)).await?
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
    /// 说话人是否被手工标/改/清过：自动分离只回填 speaker_edited=0 的段
    speaker_edited: bool,
}

#[tauri::command]
fn list_segments(state: State<AppState>, recording_id: i64) -> AppResult<Vec<Segment>> {
    let conn = state.db.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, recording_id, start_ms, end_ms, text, chars, speaker, edited, speaker_edited
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
                speaker_edited: r.get::<_, i64>(8)? != 0,
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

/// 标注某分段的说话人（1 起编号；传 null 清除）。
/// speaker_edited 一并打标：用户标过的段不被之后的自动分离静默覆盖。
#[tauri::command]
fn set_segment_speaker(state: State<AppState>, id: i64, speaker: Option<i64>) -> AppResult<()> {
    let conn = state.db.conn.lock().unwrap();
    conn.execute(
        "UPDATE segments SET speaker=?1, speaker_edited=1, edited=1 WHERE id=?2",
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

// ---------- 说话人自动分离（V2） ----------

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarEvent {
    pub recording_id: i64,
    /// running | done | failed | canceled
    pub status: String,
    pub progress: f64,
    pub error: String,
    /// 成功时的互异说话人数（done 才有意义）
    pub num_speakers: usize,
}

/// 把分离结果映射回转写分段并落库（可测试）。
/// 只回填 `speaker_edited=0` 的段：手工标/改/清过的说话人不被静默覆盖；
/// 上一轮的自动标注先整体清掉，避免残留旧簇编号。
/// 返回本次成功标注的分段数。
pub fn finalize_diarize(
    db: &db::Db,
    recording_id: i64,
    segs: &[diar::DiarSeg],
) -> AppResult<usize> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, start_ms, end_ms FROM segments WHERE recording_id = ?1 ORDER BY start_ms",
    )?;
    let rows = stmt
        .query_map([recording_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let spans: Vec<(i64, i64)> = rows.iter().map(|(_, s, e)| (*s, *e)).collect();
    let assign = diar::map_speakers(segs, &spans, diar::MAP_MIN_SHARE);

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE segments SET speaker = NULL WHERE recording_id = ?1 AND speaker_edited = 0",
        [recording_id],
    )?;
    let mut applied = 0;
    for ((id, _, _), spk) in rows.iter().zip(&assign) {
        if let Some(s) = spk {
            // 影响行数：手工保护段（speaker_edited=1）不会被更新，不计入
            applied += tx.execute(
                "UPDATE segments SET speaker = ?1 WHERE id = ?2 AND speaker_edited = 0",
                rusqlite::params![s, id],
            )?;
        }
    }
    tx.commit()?;
    Ok(applied)
}

/// 分离运行状态：全局同时最多一个，None 表示空闲。
/// 页面挂载时查询它来恢复「进行中」显示——进度在后端槽里，不依赖页面存活。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DiarizeRunState {
    recording_id: Option<i64>,
    progress: f64,
}

#[tauri::command]
fn diarize_status(state: State<AppState>) -> DiarizeRunState {
    match *state.running_diarize.lock().unwrap() {
        Some((rec, _, progress)) => DiarizeRunState { recording_id: Some(rec), progress },
        None => DiarizeRunState { recording_id: None, progress: 0.0 },
    }
}

/// 对一条录音跑说话人自动分离：起独立子进程（崩溃隔离），完成后映射回转写分段。
/// 人数必填（锁人数是产品路径，自动阈值实测不可用，见 diar 模块顶部说明）。
/// 任务进 jobs 表（kind='diarize'）由常驻分离线程串行执行；转写收尾会按
/// 预设人数自动入队同样的任务（见 finalize_job）。
#[tauri::command]
fn run_diarize(state: State<AppState>, recording_id: i64, num_clusters: i64) -> AppResult<()> {
    if !(1..=32).contains(&num_clusters) {
        return Err(error::err("说话人数必须是 1~32"));
    }
    if !models::ModelLayout::new(paths::models_dir(&state.data_root)).speaker_ready() {
        return Err(error::err("说话人模型未就绪，请先在设置中下载模型"));
    }
    // 记住人数选择，下次打开这条录音时沿用
    let _ = state.db.set_settings(&HashMap::from([(
        diar::clusters_setting_key(recording_id),
        num_clusters.to_string(),
    )]));
    enqueue_diarize(&state.db, recording_id)
}

/// 校验并把分离任务写入队列（命令入口与转写收尾的自动接续共用）。
pub fn enqueue_diarize(db: &db::Db, recording_id: i64) -> AppResult<()> {
    let conn = db.conn.lock().unwrap();
    let seg_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM segments WHERE recording_id = ?1",
        [recording_id],
        |r| r.get(0),
    )?;
    if seg_count == 0 {
        return Err(error::err("该录音还没有转写稿，请先完成转写"));
    }
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE recording_id = ?1 AND kind='diarize' AND status IN ('pending','running')",
        [recording_id],
        |r| r.get(0),
    )?;
    if active > 0 {
        return Err(error::err("该录音已有分离任务在排队或进行"));
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    conn.execute(
        "INSERT INTO jobs(recording_id, kind, status, created_at, updated_at)
         VALUES(?1,'diarize','pending',?2,?2)",
        rusqlite::params![recording_id, now],
    )?;
    Ok(())
}

/// 常驻分离任务线程：轮询 kind='diarize' 的 pending 任务，串行执行。
/// 与转写 worker 同构——队列落在 jobs 表里，进度可查、可取消、重启后 running → pending 续跑。
struct DiarizeWorker {
    db: Arc<db::Db>,
    data_root: PathBuf,
    app: tauri::AppHandle,
    running: Arc<std::sync::Mutex<Option<(i64, u32, f64)>>>,
    cancel: Arc<AtomicBool>,
}

impl DiarizeWorker {
    fn spawn(worker: Self) {
        std::thread::spawn(move || worker.run_loop());
    }

    fn run_loop(self) {
        loop {
            match self.pick_job() {
                Some((job_id, recording_id)) => self.run_job(job_id, recording_id),
                None => std::thread::sleep(std::time::Duration::from_millis(700)),
            }
        }
    }

    /// 取最早的 pending 分离任务并置为 running
    fn pick_job(&self) -> Option<(i64, i64)> {
        let conn = self.db.conn.lock().unwrap();
        let job: Option<(i64, i64)> = conn
            .query_row(
                "SELECT id, recording_id FROM jobs
                 WHERE kind='diarize' AND status='pending' ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })
            .ok()
            .flatten();
        let (job_id, rec_id) = job?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let _ = conn.execute(
            "UPDATE jobs SET status='running', updated_at=?1 WHERE id=?2",
            rusqlite::params![now, job_id],
        );
        Some((job_id, rec_id))
    }

    fn run_job(&self, job_id: i64, recording_id: i64) {
        self.cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // 人数取这条录音记住的预设；没有或非法 → 任务失败并写明原因
        let num_clusters = self
            .db
            .get_setting(&diar::clusters_setting_key(recording_id))
            .ok()
            .flatten()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .filter(|n| (1..=32).contains(n));
        let Some(num_clusters) = num_clusters else {
            self.finish(
                job_id,
                recording_id,
                Err(error::err("该录音没有预设有效的分离人数")),
                false,
            );
            return;
        };
        let result = diarize_job(
            &self.app,
            &self.db,
            &self.data_root,
            &self.running,
            &self.cancel,
            recording_id,
            num_clusters,
        );
        let canceled = self.cancel.load(std::sync::atomic::Ordering::Relaxed);
        self.finish(job_id, recording_id, result, canceled);
    }

    /// 收尾：任务状态落库 + 终态事件
    fn finish(&self, job_id: i64, recording_id: i64, result: AppResult<usize>, canceled: bool) {
        use tauri::Emitter;
        let (status, error, n) = match &result {
            Ok(num_speakers) => ("done".to_string(), String::new(), *num_speakers),
            Err(e) => {
                if canceled {
                    ("canceled".to_string(), String::new(), 0)
                } else {
                    ("failed".to_string(), e.msg.clone(), 0)
                }
            }
        };
        let progress = if status == "done" { 1.0 } else { 0.0 };
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        {
            let conn = self.db.conn.lock().unwrap();
            let _ = conn.execute(
                "UPDATE jobs SET status=?1, error=?2, progress=?3, updated_at=?4 WHERE id=?5",
                rusqlite::params![status, error, progress, now, job_id],
            );
        }
        let _ = self.app.emit(
            "diar://progress",
            DiarEvent { recording_id, status, progress, error, num_speakers: n },
        );
    }
}

/// 一次分离任务的完整生命周期：spawn 子进程 → 转发进度 → 等待 → 映射入库。
/// 返回成功时的说话人数。崩溃被隔离在子进程里，这里只见到非零退出码。
fn diarize_job(
    app: &tauri::AppHandle,
    db: &Arc<db::Db>,
    data_root: &std::path::Path,
    running: &Arc<std::sync::Mutex<Option<(i64, u32, f64)>>>,
    _cancel: &Arc<AtomicBool>,
    recording_id: i64,
    num_clusters: i64,
) -> AppResult<usize> {
    use tauri::Emitter;
    let emit = |status: &str, progress: f64, error: &str, n: usize| {
        let _ = app.emit(
            "diar://progress",
            DiarEvent {
                recording_id,
                status: status.into(),
                progress,
                error: error.into(),
                num_speakers: n,
            },
        );
    };
    emit("running", 0.05, "", 0);

    let cache = paths::cache_dir(data_root);
    std::fs::create_dir_all(&cache)?;
    let out_json = cache.join(format!("diar-{recording_id}.json"));
    let _ = std::fs::remove_file(&out_json);

    let exe = job_runner::child_exe_path("st-diarize")?;
    let mut cmd = std::process::Command::new(&exe);
    crate::priority::background(&mut cmd);
    let mut child = cmd
        .arg(recording_id.to_string())
        .arg({
            let conn = db.conn.lock().unwrap();
            let p: String = conn
                .query_row("SELECT file_path FROM recordings WHERE id=?1", [recording_id], |r| r.get(0))
                .map_err(|_| error::err("录音不存在"))?;
            p
        })
        .arg(paths::models_dir(data_root))
        .arg(&cache)
        .arg(&out_json)
        .arg(num_clusters.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| error::err(format!("启动分离子进程失败：{e}（预期位置 {}）", exe.display())))?;
    *running.lock().unwrap() = Some((recording_id, child.id(), 0.1));

    // 进度行独立线程读取；父线程阻塞在 wait 上（取消靠外部杀进程）。
    // 进度同时写回运行槽：页面切走再挂载时，diarize_status 查到的就是最新值
    let stdout = child.stdout.take();
    let app2 = app.clone();
    let running2 = running.clone();
    let reader = std::thread::spawn(move || {
        let Some(out) = stdout else { return };
        use std::io::BufRead;
        for line in std::io::BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<job_runner::ProgressLine>(line.trim()) {
                Ok(job_runner::ProgressLine::Progress { value }) => {
                    if let Some((_, _, p)) = running2.lock().unwrap().as_mut() {
                        *p = value;
                    }
                    use tauri::Emitter;
                    let _ = app2.emit(
                        "diar://progress",
                        DiarEvent {
                            recording_id,
                            status: "running".into(),
                            progress: value,
                            error: String::new(),
                            num_speakers: 0,
                        },
                    );
                }
                Ok(job_runner::ProgressLine::Error { msg }) => eprintln!("[分离] {msg}"),
                Err(_) => {}
            }
        }
    });

    let status = child.wait()?;
    *running.lock().unwrap() = None;
    let _ = reader.join();
    let stderr_tail = child
        .stderr
        .take()
        .map(|mut s| {
            use std::io::Read;
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf.chars().take(400).collect::<String>()
        })
        .unwrap_or_default();

    // 先读结果文件：失败时子进程也会写下可读原因
    let payload = std::fs::read_to_string(&out_json).ok();
    let _ = std::fs::remove_file(&out_json);
    if let Some(raw) = payload {
        if let Ok(out) = serde_json::from_str::<diar::DiarOut>(&raw) {
            match out {
                diar::DiarOut::Ok(result) => {
                    let applied = finalize_diarize(db, recording_id, &result.segments)?;
                    let _ = applied;
                    return Ok(result.num_speakers);
                }
                diar::DiarOut::Failure { error } => return Err(error::err(error)),
            }
        }
    }
    Err(error::err(job_runner::explain_child_exit(status.code(), &stderr_tail)))
}

/// 取消分离：排队中的直接置 canceled；正在跑的杀子进程，分离线程醒来后收尾为 canceled
#[tauri::command]
fn cancel_diarize(app: tauri::AppHandle, state: State<AppState>, recording_id: i64) -> AppResult<()> {
    state
        .diarize_cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some((running_rec, pid, _)) = *state.running_diarize.lock().unwrap() {
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
    let affected = {
        let conn = state.db.conn.lock().unwrap();
        conn.execute(
            "UPDATE jobs SET status='canceled', updated_at=?1
             WHERE recording_id=?2 AND kind='diarize' AND status IN ('pending','running')",
            rusqlite::params![now, recording_id],
        )?
    };
    if affected > 0 {
        use tauri::Emitter;
        let _ = app.emit(
            "diar://progress",
            DiarEvent {
                recording_id,
                status: "canceled".into(),
                progress: 0.0,
                error: String::new(),
                num_speakers: 0,
            },
        );
    }
    Ok(())
}

/// 合并说话人：把 `from` 簇的所有段并入 `to` 簇（用户修正过拆错的人）。
/// 移动的段标记 speaker_edited=1：这是手工修正，之后的自动分离不覆盖。
#[tauri::command]
fn merge_speakers(state: State<AppState>, recording_id: i64, from: i64, to: i64) -> AppResult<usize> {
    if from == to {
        return Err(error::err("不能把说话人并进它自己"));
    }
    let conn = state.db.conn.lock().unwrap();
    let n = conn.execute(
        "UPDATE segments SET speaker=?1, speaker_edited=1, edited=1
         WHERE recording_id=?2 AND speaker=?3",
        rusqlite::params![to, recording_id, from],
    )?;
    if n == 0 {
        return Err(error::err("「说话人 {from}」没有已标注的分段"));
    }
    // 显示名跟着合并：保留 to 的名字，丢掉 from 的
    let key = format!("speakers.{recording_id}");
    if let Ok(Some(json)) = conn
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [&key],
            |r| r.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })
    {
        if let Ok(mut names) = serde_json::from_str::<HashMap<String, String>>(&json) {
            if names.remove(&from.to_string()).is_some() {
                let mut m = HashMap::new();
                m.insert(key.clone(), serde_json::to_string(&names).unwrap_or_else(|_| "{}".into()));
                drop(conn);
                state.db.set_settings(&m)?;
                return Ok(n);
            }
        }
    }
    Ok(n)
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

// async 命令跑在异步线程池上：检索逻辑万一 panic 也不会炸掉主线程
#[tauri::command]
async fn search_all(
    state: State<'_, AppState>,
    query: String,
) -> AppResult<search::SearchResults> {
    search::search(&state.db, &query)
}

#[tauri::command]
fn generate_summary(app: tauri::AppHandle, state: State<AppState>, recording_id: i64) -> AppResult<()> {
    llm::summarize(state.db.clone(), app, recording_id);
    Ok(())
}

#[tauri::command]
async fn test_llm(state: State<'_, AppState>) -> AppResult<String> {
    let (base_url, api_key, model, thinking_setting) = {
        let conn = state.db.conn.lock().unwrap();
        let get = |key: &str| -> String {
            conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .unwrap_or_default()
        };
        (
            get("llm.baseUrl"),
            get("llm.apiKey"),
            get("llm.model"),
            get("llm.thinking"),
        )
    };
    let thinking = llm::thinking_param(&thinking_setting);
    // 阻塞式 HTTP（超时 600 秒），放阻塞线程池，别冻住界面
    tauri::async_runtime::spawn_blocking(move || llm::test_llm(&base_url, &api_key, &model, thinking)).await?
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
    let diarize_cancel = Arc::new(AtomicBool::new(false));
    // 转写在子进程里跑（原生识别引擎的崩溃不会带走应用），这里记录当前子进程以便取消
    let running_child: Arc<std::sync::Mutex<Option<(i64, u32)>>> =
        Arc::new(std::sync::Mutex::new(None));
    let running_diarize: Arc<std::sync::Mutex<Option<(i64, u32, f64)>>> =
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
            "UPDATE jobs SET status='pending', updated_at=?1 WHERE kind='diarize' AND status='running'",
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
            running_diarize: running_diarize.clone(),
            diarize_cancel: diarize_cancel.clone(),
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
            run_diarize,
            cancel_diarize,
            diarize_status,
            merge_speakers,
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
            // 启动常驻分离任务线程（队列在 jobs 表：kind='diarize'）
            DiarizeWorker::spawn(DiarizeWorker {
                db: state.db.clone(),
                data_root: state.data_root.clone(),
                app: app.handle().clone(),
                running: state.running_diarize.clone(),
                cancel: state.diarize_cancel.clone(),
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 前端契约：`get_app_info` 的字段必须是 camelCase（见 src/types.ts 的 AppInfo）。
    /// 缺 `rename_all` 时前端读到的是 undefined——存储卡片曾因此一直显示「…」。
    #[test]
    fn app_info_serializes_as_camel_case() {
        let info = AppInfo {
            version: "0.2.0".into(),
            data_root: "C:/x".into(),
            default_data_root: "C:/y".into(),
            logical_cores: 16,
            auto_threads: 14,
        };
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["dataRoot"], "C:/x");
        assert_eq!(v["defaultDataRoot"], "C:/y");
        assert_eq!(v["logicalCores"], 16);
        assert_eq!(v["autoThreads"], 14);
        assert!(v.get("data_root").is_none(), "不应出现 snake_case 字段");
    }
}
