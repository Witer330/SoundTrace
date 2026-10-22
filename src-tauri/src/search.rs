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

/// 截取命中位置前后各 context 字节，首尾视截断情况补省略号
fn snippet(text: &str, q: &str, context: usize) -> String {
    let hay = text.to_lowercase();
    let needle = q.to_lowercase();
    if let Some(idx) = hay.find(&needle) {
        // idx 是小写副本里的字节偏移；中英混排或 İ 等长度变化字符会让它
        // 无法直接对上原文，切片前必须钳回字符边界，否则 panic 闪退
        let start = floor_boundary(text, idx.saturating_sub(context));
        let end = ceil_boundary(text, (idx + needle.len() + context).min(text.len()));
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

fn floor_boundary(text: &str, mut i: usize) -> usize {
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(text: &str, mut i: usize) -> usize {
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::snippet;

    #[test]
    fn 混排文本中部命中不panic() {
        // 真实崩溃样本：库中 segment #1646，命中点后 48 字节内有 ASCII「g」，
        // end=66 落在「会」字中间，修复前搜索该词必闪退
        let text = "主线内存泄露九个g现象，看一下，可能后续我们会遇上哪哪个环境给他。这个星期当时候提到一半时机的对";
        let s = snippet(text, "内存泄露", 48);
        assert!(s.starts_with("主线内存泄露"), "命中在开头，不应有前省略号：{s}");
        assert!(s.ends_with('…'));
    }

    #[test]
    fn start侧切进汉字中间不panic() {
        // "a" + 21 个汉字 = 64 字节后命中「目标」，idx-48=16 落在第 6 个汉字中间
        let text = format!("a{}目标{}", "汉".repeat(21), "汉".repeat(30));
        let s = snippet(&text, "目标", 48);
        assert!(s.contains("目标"));
        assert!(s.starts_with('…'));
    }

    #[test]
    fn end侧切进汉字中间不panic() {
        // 命中点前 48 字节内混有 ASCII，idx+len+48 落在汉字中间
        let text = format!("{}目标{}", "汉".repeat(14) + "abc", "汉".repeat(30));
        let s = snippet(&text, "目标", 48);
        assert!(s.contains("目标"));
    }

    #[test]
    fn ascii查询大小写不敏感_() {
        let s = snippet("讨论 gdp 增长目标", "GDP", 48);
        assert!(s.contains("gdp"));
    }

    #[test]
    fn 小写化变长字符不panic() {
        // İ 小写化后字节数变多，hay 偏移与原文错位，修复前可能 panic
        let text = format!("{} alpha", "İ".repeat(30));
        let s = snippet(&text, "alpha", 48);
        assert!(s.contains("alpha"));
    }

    #[test]
    fn 命中在末尾时不加尾省略号_() {
        // 命中点在字节 39 < 48，start 归零；全文 48 字节，end 钳到文末，前后都不加省略号
        let s = snippet("这是一段很长的文本，结尾是关键词", "关键词", 48);
        assert_eq!(s, "这是一段很长的文本，结尾是关键词");
    }
}
