//! 转写任务的收尾逻辑测试。
//!
//! 这些用例守住子进程架构的关键性质：识别引擎在子进程里崩溃后，父进程仍要把任务
//! 状态正确落库——否则用户会看到「转写中」永远不结束，或者队列卡死。
//! 崩溃本身由 tests/real_library.rs 的 `child_process_isolates_native_crash` 验证。

use soundtrace_lib::db::Db;
use soundtrace_lib::error;
use soundtrace_lib::job_runner::SegRow;
use soundtrace_lib::paths;
use soundtrace_lib::transcribe::{finalize_job, JobInfo};
use std::path::PathBuf;
use std::sync::Arc;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soundtrace-finalize-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(db: &Db) -> (i64, i64) {
    let conn = db.conn.lock().unwrap();
    let rec: i64 = conn
        .query_row(
            "INSERT INTO recordings(file_hash, file_path, orig_name, title, duration_sec, size_bytes,
                                    format, status, notes, participants, created_at, updated_at)
             VALUES('h1', 'C:/lib/a.m4a', 'a.m4a', '会议', 60.0, 1024, 'm4a', 'transcribing', '', '',
                    '2026-09-18T00:00:00Z', '2026-09-18T00:00:00Z') RETURNING id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let job: i64 = conn
        .query_row(
            "INSERT INTO jobs(recording_id, kind, status, created_at, updated_at)
             VALUES(?1, 'transcribe', 'running', '2026-09-18T00:00:00Z', '2026-09-18T00:00:00Z')
             RETURNING id",
            [rec],
            |r| r.get(0),
        )
        .unwrap();
    (rec, job)
}

fn status_of(db: &Db, rec: i64, job: i64) -> (String, String, Option<String>) {
    let conn = db.conn.lock().unwrap();
    let rec_status: String = conn
        .query_row("SELECT status FROM recordings WHERE id=?1", [rec], |r| {
            r.get(0)
        })
        .unwrap();
    let (job_status, job_error): (String, Option<String>) = conn
        .query_row("SELECT status, error FROM jobs WHERE id=?1", [job], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    (rec_status, job_status, job_error)
}

fn seg_count(db: &Db, rec: i64) -> i64 {
    let conn = db.conn.lock().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM segments WHERE recording_id=?1",
        [rec],
        |r| r.get(0),
    )
    .unwrap()
}

/// 正常完成：分段入库、两边状态都置 done
#[test]
fn finalize_success_writes_segments_and_marks_done() {
    let root = temp_root("ok");
    paths::ensure_dirs(&root).unwrap();
    let db = Arc::new(Db::open(&root).unwrap());
    let (rec, job) = fixture(&db);

    let rows = vec![
        SegRow {
            start_ms: 0,
            end_ms: 1200,
            text: "今天主要".into(),
            chars: r#"[["今",0],["天",200]]"#.into(),
        },
        SegRow {
            start_ms: 1200,
            end_ms: 2600,
            text: "讨论三件事".into(),
            chars: "[]".into(),
        },
    ];
    let (job_status, rec_status, progress, err) = finalize_job(
        &db,
        &JobInfo {
            id: job,
            recording_id: rec,
            file_path: "C:/lib/a.m4a".into(),
        },
        Ok(rows),
        false,
    );

    assert_eq!(job_status, "done");
    assert_eq!(rec_status, "done");
    assert_eq!(progress, 1.0);
    assert!(err.is_empty());
    assert_eq!(seg_count(&db, rec), 2, "分段应入库");
    let (rs, js, je) = status_of(&db, rec, job);
    assert_eq!((rs.as_str(), js.as_str()), ("done", "done"));
    assert!(je.is_none(), "成功路径不该留错误信息");
}

/// 引擎崩溃：任务置 failed 并留下可读原因，录音状态也要归到 failed（不是永远「转写中」）
#[test]
fn finalize_engine_crash_marks_failed_with_reason() {
    let root = temp_root("crash");
    paths::ensure_dirs(&root).unwrap();
    let db = Arc::new(Db::open(&root).unwrap());
    let (rec, job) = fixture(&db);

    // 子进程崩溃后，父进程拿到的就是这条可读原因
    let reason = soundtrace_lib::job_runner::explain_child_exit(Some(0xc0000409u32 as i32), "");
    let (job_status, rec_status, progress, err) = finalize_job(
        &db,
        &JobInfo {
            id: job,
            recording_id: rec,
            file_path: "C:/lib/a.m4a".into(),
        },
        Err(error::err(&reason)),
        false,
    );

    assert_eq!(job_status, "failed");
    assert_eq!(rec_status, "failed");
    assert_eq!(progress, 0.0);
    assert!(!err.is_empty());
    let (rs, js, je) = status_of(&db, rec, job);
    assert_eq!((rs.as_str(), js.as_str()), ("failed", "failed"));
    let saved = je.expect("失败原因必须落库，否则用户不知道发生了什么");
    assert!(
        saved.contains("0xC0000409") || saved.contains("崩溃"),
        "落库原因要能说明是引擎崩溃：{saved}"
    );
    assert!(
        saved.contains("隔离") || saved.contains("不受影响"),
        "应告诉用户崩溃已被隔离、其他录音不受影响：{saved}"
    );
}

/// 用户取消：录音回到 imported（可重新排队），任务置 canceled
#[test]
fn finalize_cancel_returns_recording_to_imported() {
    let root = temp_root("cancel");
    paths::ensure_dirs(&root).unwrap();
    let db = Arc::new(Db::open(&root).unwrap());
    let (rec, job) = fixture(&db);

    let (job_status, rec_status, _, err) = finalize_job(
        &db,
        &JobInfo {
            id: job,
            recording_id: rec,
            file_path: "C:/lib/a.m4a".into(),
        },
        Err(error::err("已取消")),
        true,
    );

    assert_eq!(job_status, "canceled");
    assert_eq!(rec_status, "imported", "取消后应能重新排队");
    assert_eq!(err, "已取消");
    let (rs, js, _) = status_of(&db, rec, job);
    assert_eq!((rs.as_str(), js.as_str()), ("imported", "canceled"));
}

/// 重新转写成功时应整批替换旧分段，而不是叠加（否则稿子会出现重复内容）
#[test]
fn finalize_success_replaces_previous_segments() {
    let root = temp_root("replace");
    paths::ensure_dirs(&root).unwrap();
    let db = Arc::new(Db::open(&root).unwrap());
    let (rec, job) = fixture(&db);
    {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO segments(recording_id, start_ms, end_ms, text, chars)
             VALUES(?1, 0, 900, '旧的转写结果', '[]')",
            [rec],
        )
        .unwrap();
    }
    assert_eq!(seg_count(&db, rec), 1);

    let rows = vec![SegRow {
        start_ms: 0,
        end_ms: 1100,
        text: "新的转写结果".into(),
        chars: "[]".into(),
    }];
    let (js, _, _, _) = finalize_job(
        &db,
        &JobInfo {
            id: job,
            recording_id: rec,
            file_path: "C:/lib/a.m4a".into(),
        },
        Ok(rows),
        false,
    );
    assert_eq!(js, "done");
    assert_eq!(seg_count(&db, rec), 1, "旧分段应被替换而不是叠加");
    let conn = db.conn.lock().unwrap();
    let text: String = conn
        .query_row(
            "SELECT text FROM segments WHERE recording_id=?1",
            [rec],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(text, "新的转写结果");
}
