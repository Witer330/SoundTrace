//! 队列快照查询测试：工作台首页的「处理队列」依赖 `query_jobs` 的排序与关联合并。
//! 覆盖 ①running 置顶 → pending 次之 → 终态最后、②录音标题 JOIN、③activeOnly 过滤、
//! ④录音被删除后队列行仍可用（标题为 NULL，不因 INNER JOIN 丢行）。

use soundtrace_lib::db::Db;
use soundtrace_lib::paths;
use std::path::PathBuf;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soundtrace-queue-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 插入一条录音，返回 id
fn insert_recording(db: &Db, title: &str, duration_sec: f64, status: &str) -> i64 {
    let conn = db.conn.lock().unwrap();
    let hash = format!("hash-{title}");
    let fpath = format!("C:/lib/{title}.m4a");
    let fname = format!("{title}.m4a");
    let params: [&dyn rusqlite::ToSql; 6] =
        [&hash, &fpath, &fname, &title, &duration_sec, &status];
    conn.query_row(
        "INSERT INTO recordings(file_hash, file_path, orig_name, title, duration_sec, size_bytes,
                                format, status, notes, participants, created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, 1024, 'm4a', ?6, '', '', '2026-09-17T00:00:00Z', '2026-09-17T00:00:00Z')
         RETURNING id",
        &params[..],
        |r| r.get(0),
    )
    .unwrap()
}

/// 插入一条任务，返回 id
fn insert_job(db: &Db, recording_id: Option<i64>, status: &str, progress: f64, updated_at: &str) -> i64 {
    let conn = db.conn.lock().unwrap();
    conn.query_row(
        "INSERT INTO jobs(recording_id, kind, status, progress, created_at, updated_at)
         VALUES(?1, 'transcribe', ?2, ?3, '2026-09-17T00:00:00Z', ?4) RETURNING id",
        rusqlite::params![recording_id, status, progress, updated_at],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn queue_snapshot_order_join_and_filter() {
    let root = temp_root("order");
    paths::ensure_dirs(&root).unwrap();
    let db = Db::open(&root).unwrap();

    let running_rec = insert_recording(&db, "产品评审会", 3720.0, "transcribing");
    let pending_rec = insert_recording(&db, "客服周会", 2700.0, "queued");
    let done_rec = insert_recording(&db, "架构讨论", 1680.0, "done");

    // 故意按「终态先插、running 最后插」的顺序，验证排序不是靠 id 侥幸正确
    insert_job(&db, Some(done_rec), "done", 1.0, "2026-09-17T10:00:00Z");
    insert_job(&db, Some(pending_rec), "pending", 0.0, "2026-09-17T11:00:00Z");
    let running_job = insert_job(&db, Some(running_rec), "running", 0.42, "2026-09-17T12:00:00Z");

    let rows = soundtrace_lib::query_jobs(&db, false).unwrap();
    let order: Vec<&str> = rows.iter().map(|j| j.status.as_str()).collect();
    assert_eq!(
        order,
        vec!["running", "pending", "done"],
        "队列应把进行中置顶、排队次之、终态最后，实际 {order:?}"
    );

    // 标题与时长随 JOIN 带出（工作台要显示「哪条录音」）
    assert_eq!(rows[0].id, running_job);
    assert_eq!(rows[0].title.as_deref(), Some("产品评审会"));
    assert_eq!(rows[0].duration_sec, 3720.0);
    assert!((rows[0].progress - 0.42).abs() < 1e-9);
    assert_eq!(rows[2].title.as_deref(), Some("架构讨论"));

    // activeOnly 只留排队中/进行中
    let active = soundtrace_lib::query_jobs(&db, true).unwrap();
    assert_eq!(active.len(), 2);
    assert!(active.iter().all(|j| j.status == "pending" || j.status == "running"));
    assert!(
        active.iter().all(|j| j.title.is_some()),
        "活跃任务都应带上录音标题"
    );

    // 无关联录音的任务（recording_id 为 NULL）不能让整条查询失败：LEFT JOIN 保证其余行照常返回
    insert_job(&db, None, "failed", 0.0, "2026-09-17T13:00:00Z");
    let with_orphan = soundtrace_lib::query_jobs(&db, false).unwrap();
    assert_eq!(with_orphan.len(), 4, "孤儿任务应作为一行出现而不是让查询整体失败");
    let orphan = with_orphan.iter().find(|j| j.recording_id.is_none()).unwrap();
    assert!(orphan.title.is_none(), "无关联录音时标题为 NULL");
    assert_eq!(orphan.status, "failed");
    // 仍是「活跃在前、终态在后」；终态之间按更新时间倒序，故它排在较早的 done 之前
    let orphan_pos = with_orphan.iter().position(|j| j.recording_id.is_none()).unwrap();
    let first_terminal = with_orphan
        .iter()
        .position(|j| j.status != "running" && j.status != "pending")
        .unwrap();
    assert!(
        orphan_pos >= first_terminal,
        "孤儿任务属终态，不能挤到 running/pending 之前"
    );
}
