use crate::db::Db;
use crate::error::{err, AppResult};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use xxhash_rust::xxh3::Xxh3;

/// WebView 可直接播放的扩展名（小写）
const PLAYABLE_EXTS: &[&str] = &[
    "m4a", "mp4", "mp3", "wav", "flac", "ogg", "oga", "opus", "aac", "webm",
];

/// 无法直接播放、导入时自动转码为 m4a 的扩展名
const CONVERT_EXTS: &[&str] = &["amr", "3gp", "3gpp", "awb", "wma", "aiff", "aif"];

pub fn is_audio_file(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()) {
        Some(ext) => PLAYABLE_EXTS.contains(&ext.as_str()) || CONVERT_EXTS.contains(&ext.as_str()),
        None => false,
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub id: i64,
    pub file_hash: String,
    pub file_path: String,
    pub orig_name: String,
    pub title: String,
    pub recorded_at: Option<String>,
    pub duration_sec: f64,
    pub size_bytes: i64,
    pub format: String,
    pub status: String,
    pub notes: String,
    pub participants: String,
    pub summary_md: Option<String>,
    pub llm_model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub tags: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedItem {
    pub path: String,
    pub reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub imported: Vec<Recording>,
    pub skipped: Vec<SkippedItem>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RecordingPatch {
    pub title: Option<String>,
    pub notes: Option<String>,
    pub participants: Option<String>,
    pub recorded_at: Option<String>,
    pub tags: Option<Vec<String>>,
}

pub struct Importer<'a> {
    pub db: &'a Db,
    pub library_root: PathBuf,
}

/// 展开路径列表：目录递归收集音频文件，文件原样保留
pub fn expand_audio_paths(paths: &[String]) -> AppResult<Vec<String>> {
    let mut out = Vec::new();
    for raw in paths {
        let p = Path::new(raw);
        if p.is_dir() {
            collect_audio_files(p, &mut out)?;
        } else if is_audio_file(p) {
            out.push(raw.clone());
        }
    }
    Ok(out)
}

fn collect_audio_files(dir: &Path, out: &mut Vec<String>) -> AppResult<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_audio_files(&path, out)?;
        } else if is_audio_file(&path) {
            out.push(path.to_string_lossy().to_string());
        }
    }
    Ok(())
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 流式计算 xxh3_128，避免大文件整读内存
fn hash_file(path: &Path) -> AppResult<String> {
    let f = fs::File::open(path)?;
    let mut reader = BufReader::new(f);
    let mut hasher = Xxh3::new();
    let mut buf = [0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:032x}", hasher.digest()))
}

#[derive(Deserialize)]
struct FfprobeOut {
    #[serde(default)]
    format: Option<FfprobeFormat>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    tags: Option<FfprobeTags>,
}

#[derive(Deserialize)]
struct FfprobeTags {
    #[serde(default)]
    creation_time: Option<String>,
}

struct MediaMeta {
    duration_sec: f64,
    recorded_at: Option<String>,
}

fn probe_media(path: &Path) -> MediaMeta {
    let out = Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_format"])
        .arg(path)
        .output();
    let mut meta = MediaMeta {
        duration_sec: 0.0,
        recorded_at: None,
    };
    if let Ok(out) = out {
        if out.status.success() {
            if let Ok(parsed) = serde_json::from_slice::<FfprobeOut>(&out.stdout) {
                if let Some(fmt) = parsed.format {
                    if let Some(d) = fmt.duration.and_then(|d| d.parse::<f64>().ok()) {
                        meta.duration_sec = d;
                    }
                    if let Some(ct) = fmt.tags.and_then(|t| t.creation_time) {
                        meta.recorded_at = DateTime::parse_from_rfc3339(&ct)
                            .ok()
                            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                    }
                }
            }
        }
    }
    if meta.recorded_at.is_none() {
        // 回退：文件修改时间（手机传输通常会保留）
        meta.recorded_at = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .map(DateTime::<Utc>::from)
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    }
    meta
}

fn run_ffmpeg(args: &[&str]) -> AppResult<()> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .map_err(|e| err(format!("调用 ffmpeg 失败（请确认已安装并在 PATH）: {e}")))?;
    if !out.status.success() {
        return Err(err(format!(
            "ffmpeg 转码失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// 文件名安全化：去除路径非法字符，限长
fn sanitize_stem(stem: &str) -> String {
    let cleaned: String = stem
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "recording".to_string()
    } else {
        trimmed.chars().take(80).collect()
    }
}

impl<'a> Importer<'a> {
    /// 批量导入：去重 → 探测元数据 → 归档（必要时转码）→ 入库
    pub fn import(&self, paths: &[String]) -> AppResult<ImportResult> {
        let mut imported = Vec::new();
        let mut skipped = Vec::new();

        for raw in paths {
            let path = PathBuf::from(raw);
            let display = path.to_string_lossy().to_string();

            if !path.is_file() {
                skipped.push(SkippedItem { path: display, reason: "文件不存在".into() });
                continue;
            }
            if !is_audio_file(&path) {
                skipped.push(SkippedItem { path: display, reason: "不支持的音频格式".into() });
                continue;
            }

            let hash = match hash_file(&path) {
                Ok(h) => h,
                Err(e) => {
                    skipped.push(SkippedItem { path: display, reason: e.msg });
                    continue;
                }
            };
            if let Some(existing_title) = self.find_by_hash(&hash)? {
                skipped.push(SkippedItem {
                    path: display,
                    reason: format!("已导入过（库中：{existing_title}）"),
                });
                continue;
            }

            match self.import_one(&path, &hash) {
                Ok(rec) => imported.push(rec),
                Err(e) => skipped.push(SkippedItem { path: display, reason: e.msg }),
            }
        }

        Ok(ImportResult { imported, skipped })
    }

    fn import_one(&self, path: &Path, hash: &str) -> AppResult<Recording> {
        let meta = probe_media(path);
        let orig_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("recording");
        let src_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let recorded = meta
            .recorded_at
            .clone()
            .unwrap_or_else(now_rfc3339);
        let month = &recorded[..recorded.len().min(7)]; // YYYY-MM
        let need_convert = CONVERT_EXTS.contains(&src_ext.as_str());

        let dest_ext = if need_convert { "m4a" } else { &src_ext };
        let base = format!("{}_{}", sanitize_stem(stem), &hash[..12]);
        let month_dir = self.library_root.join(month);
        fs::create_dir_all(&month_dir)?;

        // 目标文件（若同名已存在则追加序号）
        let mut dest = month_dir.join(format!("{base}.{dest_ext}"));
        let mut n = 1;
        while dest.exists() {
            dest = month_dir.join(format!("{base}-{n}.{dest_ext}"));
            n += 1;
        }

        let dest_display = dest.to_string_lossy().to_string();
        if need_convert {
            run_ffmpeg(&[
                "-i", &path.to_string_lossy(),
                "-vn", "-c:a", "aac", "-b:a", "192k",
                &dest_display,
            ])?;
        } else {
            fs::copy(path, &dest)?;
        }

        let size = fs::metadata(&dest).map(|m| m.len() as i64).unwrap_or(0);
        let format = if need_convert {
            format!("{src_ext}→m4a")
        } else {
            src_ext.clone()
        };
        let title = sanitize_stem(stem);
        let now = now_rfc3339();

        let id = {
            let conn = self.db.conn.lock().unwrap();
            conn.query_row(
                "INSERT INTO recordings(file_hash, file_path, orig_name, title, recorded_at,
                    duration_sec, size_bytes, format, status, created_at, updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'imported',?9,?9)
                 RETURNING id",
                params![
                    hash,
                    dest.to_string_lossy(),
                    orig_name,
                    title,
                    recorded,
                    meta.duration_sec,
                    size,
                    format,
                    now
                ],
                |r| r.get::<_, i64>(0),
            )?
        };

        let rec = self.get_recording(id)?.ok_or_else(|| err("导入后读取失败"))?;
        Ok(rec)
    }

    fn find_by_hash(&self, hash: &str) -> AppResult<Option<String>> {
        let conn = self.db.conn.lock().unwrap();
        let v = conn
            .query_row("SELECT title FROM recordings WHERE file_hash = ?1", [hash], |r| {
                r.get::<_, String>(0)
            })
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })?;
        Ok(v)
    }

    fn row_to_recording(row: &rusqlite::Row<'_>, tags: String) -> rusqlite::Result<Recording> {
        Ok(Recording {
            id: row.get(0)?,
            file_hash: row.get(1)?,
            file_path: row.get(2)?,
            orig_name: row.get(3)?,
            title: row.get(4)?,
            recorded_at: row.get(5)?,
            duration_sec: row.get(6)?,
            size_bytes: row.get(7)?,
            format: row.get(8)?,
            status: row.get(9)?,
            notes: row.get(10)?,
            participants: row.get(11)?,
            summary_md: row.get(12)?,
            llm_model: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
            tags: if tags.is_empty() {
                Vec::new()
            } else {
                tags.split('\x1f').map(|s| s.to_string()).collect()
            },
        })
    }

    const REC_COLS: &'static str =
        "r.id, r.file_hash, r.file_path, r.orig_name, r.title, r.recorded_at, r.duration_sec, \
         r.size_bytes, r.format, r.status, r.notes, r.participants, r.summary_md, r.llm_model, \
         r.created_at, r.updated_at, \
         COALESCE((SELECT GROUP_CONCAT(t.name, char(31)) FROM recording_tags rt \
           JOIN tags t ON t.id = rt.tag_id WHERE rt.recording_id = r.id), '')";

    pub fn list_recordings(&self) -> AppResult<Vec<Recording>> {
        let conn = self.db.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM recordings r ORDER BY r.recorded_at DESC, r.id DESC",
            Self::REC_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], |row| Self::row_to_recording(row, row.get(16)?))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_recording(&self, id: i64) -> AppResult<Option<Recording>> {
        let conn = self.db.conn.lock().unwrap();
        let sql = format!("SELECT {} FROM recordings r WHERE r.id = ?1", Self::REC_COLS);
        let v = conn
            .query_row(&sql, [id], |row| Self::row_to_recording(row, row.get(16)?))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })?;
        Ok(v)
    }

    pub fn update_recording(&self, id: i64, patch: RecordingPatch) -> AppResult<Option<Recording>> {
        {
            let mut conn = self.db.conn.lock().unwrap();
            let tx = conn.transaction()?;

            let mut sets = vec!["updated_at = ?1".to_string()];
            let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> =
                vec![Box::new(now_rfc3339())];
            let mut idx = 2;
            if let Some(v) = patch.title {
                sets.push(format!("title = ?{idx}"));
                binds.push(Box::new(v));
                idx += 1;
            }
            if let Some(v) = patch.notes {
                sets.push(format!("notes = ?{idx}"));
                binds.push(Box::new(v));
                idx += 1;
            }
            if let Some(v) = patch.participants {
                sets.push(format!("participants = ?{idx}"));
                binds.push(Box::new(v));
                idx += 1;
            }
            if let Some(v) = patch.recorded_at {
                sets.push(format!("recorded_at = ?{idx}"));
                binds.push(Box::new(v));
                idx += 1;
            }

            let sql = format!(
                "UPDATE recordings SET {} WHERE id = ?{idx}",
                sets.join(", ")
            );
            binds.push(Box::new(id));
            let refs: Vec<&dyn rusqlite::types::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
            let changed = tx.execute(&sql, refs.as_slice())?;
            if changed == 0 {
                return Ok(None);
            }

            if let Some(tags) = patch.tags {
                let clean: Vec<String> = tags
                    .into_iter()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                tx.execute("DELETE FROM recording_tags WHERE recording_id = ?1", [id])?;
                for name in clean {
                    tx.execute(
                        "INSERT INTO tags(name) VALUES(?1) ON CONFLICT(name) DO NOTHING",
                        [&name],
                    )?;
                    let tag_id: i64 = tx.query_row(
                        "SELECT id FROM tags WHERE name = ?1",
                        [&name],
                        |r| r.get(0),
                    )?;
                    tx.execute(
                        "INSERT OR IGNORE INTO recording_tags(recording_id, tag_id) VALUES(?1, ?2)",
                        [id, tag_id],
                    )?;
                }
                // 清理无引用的孤儿标签
                tx.execute(
                    "DELETE FROM tags WHERE id NOT IN (SELECT DISTINCT tag_id FROM recording_tags)",
                    [],
                )?;
            }
            tx.commit()?;
        }
        self.get_recording(id)
    }

    /// 删除录音；delete_file 为 true 时同时删除归档文件
    pub fn delete_recording(&self, id: i64, delete_file: bool) -> AppResult<()> {
        let file_path: Option<String> = {
            let conn = self.db.conn.lock().unwrap();
            let v = conn
                .query_row("SELECT file_path FROM recordings WHERE id = ?1", [id], |r| {
                    r.get::<_, String>(0)
                })
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })?;
            if v.is_some() {
                conn.execute("DELETE FROM recordings WHERE id = ?1", [id])?;
            }
            v
        };
        if delete_file {
            if let Some(p) = file_path {
                let _ = fs::remove_file(p);
            }
        }
        Ok(())
    }
}
