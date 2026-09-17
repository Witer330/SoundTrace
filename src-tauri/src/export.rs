use crate::db::Db;
use crate::error::{err, AppResult};
use std::fs;
use std::path::Path;

/// 导出转写内容：md（含纪要）/ srt（字幕）/ txt（纯文本）
pub fn export(db: &Db, recording_id: i64, kind: &str, dest: &Path) -> AppResult<()> {
    let conn = db.conn.lock().unwrap();
    let rec: (String, Option<String>, String, String, Option<String>) = conn
        .query_row(
            "SELECT title, recorded_at, participants, notes, summary_md
             FROM recordings WHERE id=?1",
            [recording_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|_| err("录音不存在"))?;
    let tags = query_strings(
        &conn,
        "SELECT t.name FROM recording_tags rt JOIN tags t ON t.id=rt.tag_id
         WHERE rt.recording_id=?1 ORDER BY t.name",
        recording_id,
    )?;
    let segments: Vec<(i64, i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT start_ms, end_ms, text FROM segments WHERE recording_id=?1 ORDER BY start_ms",
        )?;
        let rows = stmt
            .query_map([recording_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    drop(conn);

    let (title, recorded_at, participants, notes, summary) = rec;

    if segments.is_empty() {
        return Err(err("尚无转写内容，无法导出"));
    }

    let safe_title: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .take(60)
        .collect();
    let date = recorded_at
        .as_deref()
        .map(|s| s[..s.len().min(10)].to_string())
        .unwrap_or_default();

    let participants_disp = if participants.is_empty() {
        "未记录".to_string()
    } else {
        participants.clone()
    };
    let tags_disp = if tags.is_empty() {
        "无".to_string()
    } else {
        tags.join("、")
    };

    let content = match kind {
        "md" => {
            let mut md = format!(
                "# {safe_title}\n\n- 日期：{date}\n- 参会人：{participants_disp}\n- 标签：{tags_disp}\n\n",
            );
            if let Some(s) = &summary {
                md.push_str("# AI 会议纪要\n\n");
                md.push_str(s.trim());
                md.push_str("\n\n---\n\n");
            }
            if !notes.is_empty() {
                md.push_str(&format!("# 笔记\n\n{}\n\n---\n\n", notes.trim()));
            }
            md.push_str("# 转写全文\n\n");
            for (start_ms, _, text) in &segments {
                md.push_str(&format!("- {} {}\n", fmt_ts(*start_ms), text));
            }
            md
        }
        "srt" => segments
            .iter()
            .enumerate()
            .map(|(i, (s, e, text))| {
                format!("{}\n{} --> {}\n{}\n\n", i + 1, fmt_srt(*s), fmt_srt(*e), text)
            })
            .collect(),
        "txt" => {
            let mut t = format!("{safe_title}（{date}）\n\n");
            for (start_ms, _, text) in &segments {
                t.push_str(&format!("{} {}\n", fmt_ts(*start_ms), text));
            }
            t
        }
        other => return Err(err(format!("不支持的导出类型: {other}"))),
    };

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dest, content)?;
    Ok(())
}

fn fmt_ts(ms: i64) -> String {
    let s = ms / 1000;
    format!("[{:02}:{:02}:{:02}]", s / 3600, (s / 60) % 60, s % 60)
}

fn query_strings(conn: &rusqlite::Connection, sql: &str, id: i64) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn fmt_srt(ms: i64) -> String {
    let s = ms / 1000;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        s / 3600,
        (s / 60) % 60,
        s % 60,
        ms % 1000
    )
}
