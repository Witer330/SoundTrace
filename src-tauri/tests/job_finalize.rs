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

/// 成功路径的**父子进程协议**：真实音频 → 子进程 → 结果文件 → 解析 → 入库。
/// 崩溃路径由 real_library.rs 的隔离测试覆盖，这里补上正常完成的整条链。
#[test]
fn child_process_success_round_trip() {
    use soundtrace_lib::job_runner::PipelineOutput;

    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../testdata/会议录音_产品评审.wav");
    if !source.is_file() {
        eprintln!("跳过：缺少测试音频 {}", source.display());
        return;
    }
    let mroot = std::env::var("ST_MODELS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap().join("SoundTrace").join("models"));
    if !soundtrace_lib::models::ModelLayout::new(mroot.clone()).all_ready() {
        eprintln!("跳过：模型未就绪");
        return;
    }

    // 找子进程可执行文件：测试二进制在 target/<profile>/deps/ 下，bin 在上一级
    let test_exe = std::env::current_exe().unwrap();
    let bin_dir = test_exe.parent().unwrap().parent().unwrap();
    let child_exe = bin_dir.join(if cfg!(windows) {
        "st-transcribe.exe"
    } else {
        "st-transcribe"
    });
    if !child_exe.is_file() {
        eprintln!("跳过：找不到子进程 {}（先 cargo build）", child_exe.display());
        return;
    }

    let root = temp_root("roundtrip");
    paths::ensure_dirs(&root).unwrap();
    let db = Arc::new(Db::open(&root).unwrap());
    let (rec, job) = fixture(&db);

    let cache = root.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let out_json = cache.join("out.json");

    let output = std::process::Command::new(&child_exe)
        .arg(rec.to_string())
        .arg(&source)
        .arg(&mroot)
        .arg(&cache)
        .arg(&out_json)
        .output()
        .expect("无法启动子进程");
    assert!(
        output.status.success(),
        "子进程应正常退出，实际 {:?}，stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    // 进度协议：每行一个 JSON，取值在 0..=1
    let stdout = String::from_utf8_lossy(&output.stdout);
    let progress: Vec<f64> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("progress"))
        .filter_map(|v| v.get("value").and_then(|x| x.as_f64()))
        .collect();
    println!("收到 {} 条进度，最后一条 {:?}", progress.len(), progress.last());
    assert!(!progress.is_empty(), "成功路径也应输出进度");
    assert!(
        progress.iter().all(|p| (0.0..=1.0).contains(p)),
        "进度必须在 0..=1：{progress:?}"
    );

    // 结果文件 → 解析 → 入库
    let raw = std::fs::read_to_string(&out_json).expect("结果文件应存在");
    let parsed: PipelineOutput = serde_json::from_str(&raw).expect("结果文件应符合协议");
    let rows = match parsed {
        PipelineOutput::Rows(rows) => rows,
        PipelineOutput::Failure { error } => panic!("应成功却返回失败：{error}"),
    };
    println!("解析出 {} 段分段", rows.len());
    assert!(!rows.is_empty(), "不应出现零分段");
    for r in &rows {
        assert!(r.end_ms > r.start_ms, "分段区间非法：{}..{}", r.start_ms, r.end_ms);
        assert!(!r.text.trim().is_empty(), "分段文本不应为空");
    }

    let (js, rs, _, err) = finalize_job(
        &db,
        &JobInfo {
            id: job,
            recording_id: rec,
            file_path: source.to_string_lossy().into_owned(),
        },
        Ok(rows.clone()),
        false,
    );
    assert_eq!(js, "done");
    assert_eq!(rs, "done");
    assert!(err.is_empty());
    assert_eq!(seg_count(&db, rec), rows.len() as i64, "分段应全部入库");

    // 入库的首段文本应与结果文件一致（确认链路中没有变形）
    let conn = db.conn.lock().unwrap();
    let first: String = conn
        .query_row(
            "SELECT text FROM segments WHERE recording_id=?1 ORDER BY start_ms LIMIT 1",
            [rec],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let expected = rows
        .iter()
        .min_by_key(|r| r.start_ms)
        .map(|r| r.text.clone())
        .unwrap();
    assert_eq!(first, expected);
    println!("首段：{first}");
}

/// 子进程参数协议：第 6 个参数（`asr_threads`）可选——省略时与旧版父进程兼容，
/// 给了就透传给识别器；参数个数明显不对则必须以退出码 2 + 用法提示拒绝。
///
/// 这里用 1 秒**数字静音**验证边界（不产生语音分段，也就不会走识别器解码），
/// 完整成功路径由 `child_process_success_round_trip` 覆盖。
#[test]
fn child_accepts_optional_thread_arg() {
    use soundtrace_lib::job_runner::PipelineOutput;

    let test_exe = std::env::current_exe().unwrap();
    let bin_dir = test_exe.parent().unwrap().parent().unwrap();
    let child_exe = bin_dir.join(if cfg!(windows) {
        "st-transcribe.exe"
    } else {
        "st-transcribe"
    });
    if !child_exe.is_file() {
        eprintln!("跳过：找不到子进程 {}（先 cargo build）", child_exe.display());
        return;
    }

    let root = temp_root("argprotocol");
    paths::ensure_dirs(&root).unwrap();
    let cache = root.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let wav = cache.join("silence.wav");
    write_silence_wav(&wav, 1);
    let out_json = cache.join("out.json");

    // 参数个数边界。注意 `std::env::args()` 含 argv[0]（程序名），所以「传 5 个」= 总数 6
    // = 旧版父进程的调用方式，「传 6 个」= 总数 7 = 带 asr_threads 的新调用。二者都必须放行，
    // 只有传 4 个与 8 个才该被用法检查挡下。
    // 这几个用例只验证边界：路径填占位符（模型必然未就绪），因此不会真的跑识别。
    let base = [
        "1".to_string(),
        "占位".to_string(),
        "占位".to_string(),
        cache.to_string_lossy().into_owned(),
        out_json.to_string_lossy().into_owned(),
        "2".to_string(),
    ];

    for count in [4usize, 8] {
        let args = if count == 4 {
            base[..4].to_vec()
        } else {
            let mut v = base.to_vec();
            v.push("多余".to_string());
            v.push("多余".to_string());
            v
        };
        let out = std::process::Command::new(&child_exe)
            .args(&args)
            .output()
            .expect("无法启动子进程");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(2),
            "传 {count} 个参数应退出码 2，实际 {:?}，stderr={stderr}",
            out.status.code()
        );
        assert!(stderr.contains("用法"), "应提示用法，实际：{stderr}");
    }

    for count in [5usize, 6] {
        let args = base[..count].to_vec();
        let out = std::process::Command::new(&child_exe)
            .args(&args)
            .output()
            .expect("无法启动子进程");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(
            out.status.code(),
            Some(2),
            "传 {count} 个参数是合法调用，却被当成用法错误：{stderr}"
        );
        assert!(
            !stderr.contains("用法"),
            "传 {count} 个参数不该提示用法：{stderr}"
        );
    }

    let mroot = std::env::var("ST_MODELS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap().join("SoundTrace").join("models"));
    if !soundtrace_lib::models::ModelLayout::new(mroot.clone()).all_ready() {
        eprintln!("跳过：模型未就绪");
        return;
    }

    // 带 asr_threads 的真实调用：必须一路走到流水线并产出协议结果
    let _ = std::fs::remove_file(&out_json);
    let out = std::process::Command::new(&child_exe)
        .arg("1")
        .arg(&wav)
        .arg(&mroot)
        .arg(&cache)
        .arg(&out_json)
        .arg("2")
        .output()
        .expect("无法启动子进程");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        out.status.code(),
        Some(2),
        "带 asr_threads 的调用被当成用法错误了：{stderr}"
    );
    let raw = std::fs::read_to_string(&out_json).expect("结果文件应存在（失败也要落盘）");
    match serde_json::from_str::<PipelineOutput>(&raw) {
        Ok(PipelineOutput::Failure { error }) => {
            // 静音没有语音内容，预期就是这条失败
            println!("按预期返回失败：{error}");
            assert!(
                error.contains("语音") || error.contains("模型"),
                "静音应报「未识别到语音内容」，实际：{error}"
            );
        }
        Ok(PipelineOutput::Rows(rows)) => println!("静音竟识别出 {} 段（可接受）", rows.len()),
        Err(e) => panic!("结果文件不符合协议：{e}，原文 {raw}"),
    }
}

/// 生成指定秒数的 16kHz 单声道 PCM WAV（全零 = 数字静音）
fn write_silence_wav(path: &std::path::Path, seconds: usize) {
    let sample_rate: u32 = 16000;
    let data = vec![0u8; sample_rate as usize * seconds * 2];
    let data_len = data.len() as u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // 字节率
    wav.extend_from_slice(&2u16.to_le_bytes()); // 块对齐
    wav.extend_from_slice(&16u16.to_le_bytes()); // 位深
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(path, wav).expect("写入测试 WAV 失败");
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

fn diarize_job_count(db: &Db, rec: i64) -> i64 {
    let conn = db.conn.lock().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE recording_id=?1 AND kind='diarize'",
        [rec],
        |r| r.get(0),
    )
    .unwrap()
}

/// 「转写前预设人数、转写完自动分离」的收尾契约：
/// 预设了人数 → 转写成功后自动产生 kind='diarize' 的 pending 任务；
/// 没预设 → 不产生（保持「人工逐条分离」的现状，符合历史录音批量转写的用法）。
#[test]
fn finalize_success_enqueues_auto_diarize_only_when_preset() {
    let rows = vec![SegRow {
        start_ms: 0,
        end_ms: 500,
        text: "x".into(),
        chars: "[]".into(),
    }];
    let info = |rec: i64, job: i64| JobInfo {
        id: job,
        recording_id: rec,
        file_path: "C:/lib/a.m4a".into(),
    };

    // 无预设：不产生分离任务
    let root = temp_root("auto-diar-off");
    paths::ensure_dirs(&root).unwrap();
    let db = Arc::new(Db::open(&root).unwrap());
    let (rec, job) = fixture(&db);
    finalize_job(&db, &info(rec, job), Ok(rows.clone()), false);
    assert_eq!(diarize_job_count(&db, rec), 0, "未预设不应自动入队");

    // 预设 3 人：成功后产生一条 pending 的分离任务
    let root = temp_root("auto-diar-on");
    paths::ensure_dirs(&root).unwrap();
    let db = Arc::new(Db::open(&root).unwrap());
    let (rec, job) = fixture(&db);
    db.set_settings(&std::collections::HashMap::from([(
        soundtrace_lib::diar::clusters_setting_key(rec),
        "3".into(),
    )]))
    .unwrap();
    finalize_job(&db, &info(rec, job), Ok(rows), false);
    assert_eq!(diarize_job_count(&db, rec), 1, "预设人数后转写完成应自动入队");
    let conn = db.conn.lock().unwrap();
    let (kind, status): (String, String) = conn
        .query_row(
            "SELECT kind, status FROM jobs WHERE recording_id=?1 AND kind='diarize'",
            [rec],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), status.as_str()), ("diarize", "pending"));
}
