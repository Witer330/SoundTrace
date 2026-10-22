//! 说话人分离的子进程协议与收尾入库测试。
//!
//! 守住的性质：
//! 1. 子进程参数校验（人数必填、范围 1~32）；
//! 2. 分离结果落库时**手工标注不被静默覆盖**（章程红线，见 finalize_diarize）；
//! 3. 双 TTS 音色的端到端：女声（Huihui）+ 男声（Kangkang）拼接后，
//!    锁人数 = 2 必须分出两个说话人，且前半/后半归属稳定。

use soundtrace_lib::db::Db;
use soundtrace_lib::diar::{self, DiarOut};
use soundtrace_lib::job_runner::child_exe_path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soundtrace-diar-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn child() -> Command {
    let exe = child_exe_path("st-diarize").expect("找不到 st-diarize（cargo build 会生成）");
    Command::new(exe)
}

/// 参数数量不对 → 退出码 2
#[test]
fn child_rejects_wrong_argc() {
    let out = child().args(["a", "b", "c"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "参数不足应退出码 2");
}

/// 人数越界（0 / 非整数）→ 退出码 2
#[test]
fn child_rejects_bad_num_clusters() {
    let base = ["1", "a.wav", "m", "c", "out.json"];
    for bad in ["0", "abc", "99"] {
        let out = child().args(base.iter().copied().chain([bad])).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "num_clusters={bad} 应退出码 2");
    }
}

fn fixture_db(root: &std::path::Path) -> Arc<Db> {
    let db = Arc::new(Db::open(root).unwrap());
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO recordings(file_hash, file_path, orig_name, title, duration_sec, size_bytes,
                                format, status, notes, participants, created_at, updated_at)
         VALUES('h1', 'C:/lib/a.m4a', 'a.m4a', '会议', 60.0, 1024, 'm4a', 'done', '', '',
                '2026-09-19T00:00:00Z', '2026-09-19T00:00:00Z')",
        [],
    )
    .unwrap();
    // 三段：1、3 段是自动结果（speaker_edited=0），第 2 段被手工标过（speaker_edited=1）
    for (start, end, spk, spk_edited) in
        [(0, 10_000, 1, 0), (10_000, 20_000, 2, 1), (20_000, 30_000, 0, 0)]
    {
        conn.execute(
            "INSERT INTO segments(recording_id, start_ms, end_ms, text, chars, speaker, edited, speaker_edited)
             VALUES(1, ?1, ?2, 'x', '[]', ?3, 0, ?4)",
            rusqlite::params![start, end, spk, spk_edited],
        )
        .unwrap();
    }
    drop(conn);
    db
}

/// 收尾入库：自动段被新结果覆盖，手工段（speaker_edited=1）保持原样
#[test]
fn finalize_respects_manual_speaker_marks() {
    let root = temp_root("finalize");
    let db = fixture_db(&root);
    // 分离结果：0-30 秒都属于簇 1
    let segs = vec![diar::DiarSeg { start_ms: 0, end_ms: 30_000, speaker: 1 }];
    let applied = soundtrace_lib::finalize_diarize(&db, 1, &segs).unwrap();
    let conn = db.conn.lock().unwrap();
    let spk = |id: i64| -> Option<i64> {
        conn.query_row("SELECT speaker FROM segments WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    };
    // 第 1 段（自动）被覆盖为簇 1；第 2 段（手工标的 2）保持 2；第 3 段（自动、无语音归属清晰）→ 1
    assert_eq!(spk(1), Some(1), "自动段应被新结果覆盖");
    assert_eq!(spk(2), Some(2), "手工标注的段绝不能被自动分离覆盖");
    assert_eq!(spk(3), Some(1));
    assert_eq!(applied, 2, "只统计真正回填的自动段");
}

/// 端到端：双 TTS 音色 → 子进程 → 锁 2 人 → 结果文件 → 两个人、前后半归属稳定
#[test]
fn e2e_two_voices_two_speakers() {
    // 1) 模型准备（与转写 e2e 同一惯例：装到 ~/SoundTrace/models，已有则跳过）
    let home = dirs::home_dir().unwrap();
    let models_root = home.join("SoundTrace").join("models");
    soundtrace_lib::models::install_models(
        &models_root,
        &["seg".to_string(), "campplus".to_string()],
        &|_| {},
    )
    .expect("说话人模型安装失败");

    // 2) 两个不同音色的 TTS，各说约 15 秒
    let dir = std::env::temp_dir().join("soundtrace-diar-e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let v1 = dir.join("voice1.wav");
    let v2 = dir.join("voice2.wav");
    let mk_tts = |file: &std::path::Path, voice: &str, text: &str| {
        let ps = format!(
            r#"
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
try {{ $s.SelectVoice('{}') }} catch {{ }}
$s.SetOutputToWaveFile('{}')
$s.Rate = 0
$s.Speak('{}')
$s.Dispose()
"#,
            voice,
            file.to_string_lossy().replace('\\', "\\\\"),
            text,
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .output()
            .expect("运行 PowerShell 失败");
        assert!(
            out.status.success() && file.is_file(),
            "TTS 生成失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    mk_tts(
        &v1,
        "Microsoft Huihui",
        "大家好，现在开始本周的项目例会。首先由我汇报整体进度，接口联调已经基本完成，\
         性能优化方案也在评审中，测试环境今天下午可以就绪，请大家关注邮件通知。",
    );
    mk_tts(
        &v2,
        "Microsoft Kangkang",
        "好的，我补充一下测试这边的安排。回归测试计划已经排好，覆盖所有核心流程，\
         压测报告会在周五之前提交，另外下周的客户演示需要产品经理确认演示脚本。",
    );

    // 3) 拼接成一条 30 秒音频：前半女声、后半男声
    let mixed = dir.join("mixed.wav");
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i").arg(&v1)
        .arg("-i").arg(&v2)
        .args(["-filter_complex", "[0:a][1:a]concat=n=2:v=0:a=1"])
        .arg(&mixed)
        .output()
        .expect("ffmpeg 拼接失败");
    assert!(out.status.success(), "拼接失败: {}", String::from_utf8_lossy(&out.stderr));

    // 4) 子进程分离，锁 2 人
    let out_json = dir.join("out.json");
    let out = child()
        .args([
            "999".to_string(),
            mixed.to_string_lossy().into_owned(),
            models_root.to_string_lossy().into_owned(),
            dir.to_string_lossy().into_owned(),
            out_json.to_string_lossy().into_owned(),
            "2".to_string(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "子进程失败: {}", String::from_utf8_lossy(&out.stderr));

    // 5) 结果文件 → 两个人，且前半/后半归属稳定
    let raw = std::fs::read_to_string(&out_json).unwrap();
    let result: DiarOut = serde_json::from_str(&raw).unwrap();
    let diar::DiarOut::Ok(result) = result else {
        panic!("成功路径不应返回失败形态: {raw}");
    };
    assert_eq!(result.num_speakers, 2, "锁 2 人必须得到 2 个说话人");

    let speakers: Vec<i64> = result.segments.iter().map(|s| s.speaker).collect();
    assert!(speakers.contains(&1) && speakers.contains(&2), "两个人都要出现: {speakers:?}");

    let mid = {
        let dur_sec = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(&mixed)
            .output()
            .unwrap();
        let d: f64 = String::from_utf8_lossy(&dur_sec.stdout).trim().parse().unwrap();
        (d * 1000.0 / 2.0) as i64
    };

    // 每个半场的主导说话人按「时长占比」计：拼接缝附近跨界的窗口允许存在，
    // 但主导说话人必须占绝对多数（≥80%），且两个半场的主导者不同
    let dominant = |half: &[&diar::DiarSeg]| -> (i64, f64) {
        let mut by: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
        let mut total = 0.0;
        for s in half {
            let d = (s.end_ms - s.start_ms) as f64;
            *by.entry(s.speaker).or_insert(0.0) += d;
            total += d;
        }
        let (spk, d) = by.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        (spk, d / total)
    };
    let first: Vec<&diar::DiarSeg> = result
        .segments
        .iter()
        .filter(|s| (s.start_ms + s.end_ms) / 2 < mid)
        .collect();
    let second: Vec<&diar::DiarSeg> = result
        .segments
        .iter()
        .filter(|s| (s.start_ms + s.end_ms) / 2 >= mid)
        .collect();
    assert!(!first.is_empty() && !second.is_empty());
    let (spk1, share1) = dominant(&first);
    let (spk2, share2) = dominant(&second);
    assert!(share1 >= 0.8, "前半段主导说话人应 ≥80%: {share1} ({first:?})");
    assert!(share2 >= 0.8, "后半段主导说话人应 ≥80%: {share2} ({second:?})");
    assert_ne!(spk1, spk2, "两个音色必须被分开: spk_{spk1} vs spk_{spk2}");

    let _ = std::fs::remove_dir_all(&dir);
}
