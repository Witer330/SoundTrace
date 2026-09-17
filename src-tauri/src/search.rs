use crate::db::Db;
use crate::error::AppResult;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingHit {
    pub id: i64,
    pub title: String,
    pub recorded_at: Option<String>,
    pub duration_sec: f64,
    pub status: String,
    pub tags: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentHit {
    pub recording_id: i64,
    pub recording_title: String,
    pub recorded_at: Option<String>,
    pub start_ms: i64,
    pub snippet: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub recordings: Vec<RecordingHit>,
    pub segments: Vec<SegmentHit>,
}

/// 全局搜索：标题/标签/笔记 + 转写全文（子串匹配，符合中文习惯）
pub fn search(db: &Db, query: &str) -> AppResult<SearchResults> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(SearchResults {
            recordings: vec![],
            segments: vec![],
        });
    }
    let conn = db.conn.lock().unwrap();

    let recordings = search_recordings(&conn, q)?;
    let segments = search_segments(&conn, q)?;
    Ok(SearchResults { recordings, segments })
}

fn like_pattern(q: &str) -> String {
    // 转义 LIKE 通配符，做字面子串匹配
    let escaped: String = q
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

fn search_recordings(conn: &Connection, q: &str) -> AppResult<Vec<RecordingHit>> {
    let pat = like_pattern(q);
    let mut stmt = conn.prepare(
        "SELECT DISTINCT r.id, r.title, r.recorded_at, r.duration_sec, r.status,
                COALESCE((SELECT GROUP_CONCAT(t.name, char(31)) FROM recording_tags rt
                  JOIN tags t ON t.id = rt.tag_id WHERE rt.recording_id = r.id), '')
         FROM recordings r
         LEFT JOIN recording_tags rt2 ON rt2.recording_id = r.id
         LEFT JOIN tags t2 ON t2.id = rt2.tag_id
         WHERE r.title LIKE ?1 ESCAPE '\\'
            OR r.notes LIKE ?1 ESCAPE '\\'
            OR r.participants LIKE ?1 ESCAPE '\\'
            OR t2.name LIKE ?1 ESCAPE '\\'
         ORDER BY r.recorded_at DESC
         LIMIT 100",
    )?;
    let rows = stmt
        .query_map([&pat], |r| {
            let tags: String = r.get(5)?;
            Ok(RecordingHit {
                id: r.get(0)?,
                title: r.get(1)?,
                recorded_at: r.get(2)?,
                duration_sec: r.get(3)?,
                status: r.get(4)?,
                tags: if tags.is_empty() {
                    vec![]
                } else {
                    tags.split('\x1f').map(String::from).collect()
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn search_segments(conn: &Connection, q: &str) -> AppResult<Vec<SegmentHit>> {
    let pat = like_pattern(q);
    let mut stmt = conn.prepare(
        "SELECT s.recording_id, r.title, r.recorded_at, s.start_ms, s.text
         FROM segments s
         JOIN recordings r ON r.id = s.recording_id
         WHERE s.text LIKE ?1 ESCAPE '\\'
         ORDER BY r.recorded_at DESC, s.start_ms
         LIMIT 200",
    )?;
    let rows = stmt
        .query_map([&pat], |r| {
            let text: String = r.get(4)?;
            Ok(SegmentHit {
                recording_id: r.get(0)?,
                recording_title: r.get(1)?,
                recorded_at: r.get(2)?,
                start_ms: r.get(3)?,
                snippet: snippet(&text, q, 48),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 截取命中位置前后 context 字符
fn snippet(text: &str, q: &str, context: usize) -> String {
    let hay = text.to_lowercase();
    let needle = q.to_lowercase();
    if let Some(idx) = hay.find(&needle) {
        // 对齐字符边界（to_lowercase 不改变长度时才可靠；中文场景安全）
        let start = idx.saturating_sub(context);
        let end = (idx + needle.len() + context).min(text.len());
        let mut s = String::new();
        if start > 0 {
            s.push('…');
        }
        s.push_str(&text[start..end]);
        if end < text.len() {
            s.push('…');
        }
        s
    } else {
        text.chars().take(context * 2).collect()
    }
}
