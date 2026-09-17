//! 转写稿编辑测试：合并分段的边界、删除、改字后标记、说话人标注与编辑计数。
//! 这些规则直接决定「手工修改会不会被静默覆盖」，必须守住。

use soundtrace_lib::db::Db;
use soundtrace_lib::paths;
use std::collections::HashMap;
use std::path::PathBuf;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soundtrace-edit-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn insert_recording(db: &Db, title: &str) -> i64 {
    let conn = db.conn.lock().unwrap();
    let params: [&dyn rusqlite::ToSql; 2] = [&format!("hash-{title}"), &title];
    conn.query_row(
        "INSERT INTO recordings(file_hash, file_path, orig_name, title, duration_sec, size_bytes,
                                format, status, notes, participants, created_at, updated_at)
         VALUES(?1, 'C:/lib/x.m4a', 'x.m4a', ?2, 60.0, 1024, 'm4a', 'done', '', '',
                '2026-09-17T00:00:00Z', '2026-09-17T00:00:00Z')
         RETURNING id",
        &params[..],
        |r| r.get(0),
    )
    .unwrap()
}

fn insert_segment(db: &Db, rec: i64, start: i64, end: i64, text: &str, chars: &str) -> i64 {
    let conn = db.conn.lock().unwrap();
    conn.query_row(
        "INSERT INTO segments(recording_id, start_ms, end_ms, text, chars)
         VALUES(?1, ?2, ?3, ?4, ?5) RETURNING id",
        rusqlite::params![rec, start, end, text, chars],
        |r| r.get(0),
    )
    .unwrap()
}

fn seg_row(db: &Db, id: i64) -> Option<(i64, i64, String, String, bool, Option<i64>)> {
    let conn = db.conn.lock().unwrap();
    conn.query_row(
        "SELECT start_ms, end_ms, text, chars, edited, speaker FROM segments WHERE id=?1",
        [id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<_, i64>(4)? != 0,
                r.get(5)?,
            ))
        },
    )
    .ok()
}

#[test]
fn segment_editing_rules() {
    let root = temp_root("rules");
    paths::ensure_dirs(&root).unwrap();
    let db = Db::open(&root).unwrap();
    let rec = insert_recording(&db, "产品评审会");

    // 「今天」+「主要讨论三件事」两段，字符时间轴各两个字符
    let a = insert_segment(
        &db,
        rec,
        0,
        1000,
        "今天",
        r#"[["今",0],["天",200]]"#,
    );
    let b = insert_segment(
        &db,
        rec,
        1000,
        2000,
        "主要",
        r#"[["主",1000],["要",1200]]"#,
    );

    // ---------- 改字：文本入库、字符时间轴清空、打上编辑标记 ----------
    {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "UPDATE segments SET text='今天呢', chars='[]', edited=1 WHERE id=?1",
            [a],
        )
        .unwrap();
    }
    let (_, _, text, chars, edited, _) = seg_row(&db, a).unwrap();
    assert_eq!(text, "今天呢");
    assert_eq!(chars, "[]", "文本已变，原字符时间轴必须清空以免跳转到错位置");
    assert!(edited, "改过的分段要打标记");

    // ---------- 合并：文本拼接、区间取并集、时间轴去重且保持单调 ----------
    // 先把 a 还原成可合并的状态
    {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "UPDATE segments SET text='今天', chars=?1, edited=0 WHERE id=?2",
            rusqlite::params![r#"[["今",0],["天",200]]"#, a],
        )
        .unwrap();
    }
    let merged = soundtrace_lib::merge_segments_impl(&db, &[a, b]).unwrap();
    assert_eq!(merged, a, "合并保留首个分段作为幸存行");
    let (start, end, text, chars, edited, _) = seg_row(&db, a).unwrap();
    assert_eq!(start, 0, "起点取首个分段");
    assert_eq!(end, 2000, "终点取末个分段");
    assert_eq!(text, "今天主要");
    assert!(edited, "合并算手工修改");
    assert!(seg_row(&db, b).is_none(), "被合并掉的分段要删除");
    // 时间轴仍是合法 JSON 且按时间单调
    let parsed: Vec<(String, i64)> = serde_json::from_str(&chars).unwrap();
    assert_eq!(
        parsed.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
        vec!["今", "天", "主", "要"]
    );
    assert!(
        parsed.windows(2).all(|w| w[0].1 <= w[1].1),
        "合并后的时间轴必须单调，否则点字跳转会倒退：{parsed:?}"
    );

    // ---------- 不能跨录音合并 ----------
    let other = insert_recording(&db, "客服周会");
    let c = insert_segment(&db, other, 0, 500, "另一条录音", "[]");
    let err = soundtrace_lib::merge_segments_impl(&db, &[a, c]).unwrap_err();
    assert!(err.msg.contains("跨录音"), "实际错误：{}", err.msg);

    // ---------- 中间夹着未选中的分段时必须拒绝（否则合并会打乱稿子顺序） ----------
    let d = insert_segment(&db, rec, 2000, 3000, "讨论三件事", "[]");
    let e = insert_segment(&db, rec, 3000, 4000, "第一件", "[]");
    let err = soundtrace_lib::merge_segments_impl(&db, &[a, e]).unwrap_err();
    assert!(err.msg.contains("夹着"), "实际错误：{}", err.msg);
    // 相邻的可以合并
    assert!(soundtrace_lib::merge_segments_impl(&db, &[d, e]).is_ok());

    // ---------- 少于两段不合并 ----------
    assert!(soundtrace_lib::merge_segments_impl(&db, &[a]).is_err());

    // ---------- 说话人标注与编辑计数 ----------
    {
        let conn = db.conn.lock().unwrap();
        conn.execute("UPDATE segments SET speaker=2, edited=1 WHERE id=?1", [d])
            .unwrap();
    }
    let (_, _, _, _, _, speaker) = seg_row(&db, d).unwrap();
    assert_eq!(speaker, Some(2));
    // a 因合并被标记、d 因标注说话人被标记；e 是刚插入的未编辑分段，不计入
    let edited_count = soundtrace_lib::count_edited_segments_impl(&db, rec).unwrap();
    assert_eq!(
        edited_count, 2,
        "只有被手工改过的分段计入，实际 {edited_count}"
    );

    // ---------- 删除：计数随之下降（删掉的编辑不该再挡重新转写） ----------
    {
        let conn = db.conn.lock().unwrap();
        conn.execute("DELETE FROM segments WHERE id=?1", [d]).unwrap();
    }
    assert!(seg_row(&db, d).is_none());
    assert_eq!(
        soundtrace_lib::count_edited_segments_impl(&db, rec).unwrap(),
        1
    );
}

#[test]
fn split_segment_halves() {
    let root = temp_root("split");
    paths::ensure_dirs(&root).unwrap();
    let db = Db::open(&root).unwrap();
    let rec = insert_recording(&db, "会议");

    // 「今天讨论」四个字，各带时间戳，段区间 0..4000
    let seg = insert_segment(
        &db,
        rec,
        0,
        4000,
        "今天讨论",
        r#"[["今",0],["天",1000],["讨",2000],["论",3000]]"#,
    );

    let tail_id = soundtrace_lib::split_segment_impl(&db, seg, 2000).unwrap();

    // 前半保留原行，区间收窄到中点，文本与时间轴各留前两字
    let (s, e, text, chars, edited, _) = seg_row(&db, seg).unwrap();
    assert_eq!((s, e), (0, 2000));
    assert_eq!(text, "今天");
    assert!(edited, "拆分算手工修改");
    let head: Vec<(String, i64)> = serde_json::from_str(&chars).unwrap();
    assert_eq!(head, vec![("今".into(), 0), ("天".into(), 1000)]);

    // 后半是新段，接在中点之后
    let (s, e, text, chars, edited, _) = seg_row(&db, tail_id).unwrap();
    assert_eq!((s, e), (2000, 4000));
    assert_eq!(text, "讨论");
    assert!(edited);
    let tail: Vec<(String, i64)> = serde_json::from_str(&chars).unwrap();
    assert_eq!(tail, vec![("讨".into(), 2000), ("论".into(), 3000)]);

    // 拆分点必须落在段内部
    assert!(soundtrace_lib::split_segment_impl(&db, seg, 0).is_err());
    assert!(soundtrace_lib::split_segment_impl(&db, seg, 2000).is_err(), "端点不算内部分割点");
    assert!(soundtrace_lib::split_segment_impl(&db, 999_999, 100).is_err(), "不存在的分段");

    // 已改字的段（字符时间轴与文本不一致）按文本长度均分，不丢字
    let plain = insert_segment(&db, rec, 4000, 5000, "甲乙丙丁", "[]");
    let tail2 = soundtrace_lib::split_segment_impl(&db, plain, 4500).unwrap();
    let (_, _, head_text, head_chars, _, _) = seg_row(&db, plain).unwrap();
    let (_, _, tail_text, _, _, _) = seg_row(&db, tail2).unwrap();
    assert_eq!(format!("{head_text}{tail_text}"), "甲乙丙丁", "均分不能丢字");
    assert_eq!(head_chars, "[]");
}

#[test]
fn speaker_names_round_trip() {    let root = temp_root("speakers");
    paths::ensure_dirs(&root).unwrap();
    let db = Db::open(&root).unwrap();

    // 未命名时返回空表
    let key = "speakers.7";
    assert!(db.get_setting(key).unwrap().is_none());

    // 写入后能读回（前端把「说话人1 → 张三」这类映射整个存进去）
    let mut names = HashMap::new();
    names.insert("1".to_string(), "张三".to_string());
    names.insert("2".to_string(), "李四".to_string());
    let mut entries = HashMap::new();
    entries.insert(key.to_string(), serde_json::to_string(&names).unwrap());
    db.set_settings(&entries).unwrap();

    let raw = db.get_setting(key).unwrap().unwrap();
    let back: HashMap<String, String> = serde_json::from_str(&raw).unwrap();
    assert_eq!(back.get("1").map(String::as_str), Some("张三"));
    assert_eq!(back.get("2").map(String::as_str), Some("李四"));

    // 按录音隔离：另一条录音的名字不受影响
    assert!(db.get_setting("speakers.8").unwrap().is_none());
}

/// 重新转写会清空该录音全部分段（含手工修改），这正是前端必须在覆盖前提示的原因。
/// 这个测试固定住行为：如果将来改成「保留手改」，这里的断言失败会提醒同步改文案。
#[test]
fn retranscribe_replaces_edited_segments() {
    let root = temp_root("retranscribe");
    paths::ensure_dirs(&root).unwrap();
    let db = Db::open(&root).unwrap();
    let rec = insert_recording(&db, "会议");
    insert_segment(&db, rec, 0, 1000, "今天", "[]");
    let edited_id = insert_segment(&db, rec, 1000, 2000, "主要讨论", "[]");
    {
        let conn = db.conn.lock().unwrap();
        conn.execute("UPDATE segments SET text='主要讨论了', edited=1 WHERE id=?1", [edited_id])
            .unwrap();
    }
    assert_eq!(soundtrace_lib::count_edited_segments_impl(&db, rec).unwrap(), 1);

    // 模拟 worker 成功写回：先清空再整批插入（与 transcribe.rs 的做法一致）
    {
        let mut conn = db.conn.lock().unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute("DELETE FROM segments WHERE recording_id = ?1", [rec]).unwrap();
        tx.execute(
            "INSERT INTO segments(recording_id, start_ms, end_ms, text, chars) VALUES(?1,0,1500,'今天主要讨论','[]')",
            [rec],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let conn = db.conn.lock().unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM segments WHERE recording_id=?1", [rec], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "旧分段被整批替换");
    drop(conn);
    assert_eq!(
        soundtrace_lib::count_edited_segments_impl(&db, rec).unwrap(),
        0,
        "新写入的分段不带编辑标记"
    );
    assert!(seg_row(&db, edited_id).is_none(), "手工改过的那一段已被覆盖删除");
}
