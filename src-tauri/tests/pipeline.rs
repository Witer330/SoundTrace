//! 全链路集成测试：导入（哈希去重/元数据/归档）→ 转写流水线 → 分段入库 → 全局搜索 → 导出。
//! 使用临时数据目录，不触碰用户真实库。

use soundtrace_lib::db::Db;
use soundtrace_lib::paths;
use std::path::{Path, PathBuf};

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soundtrace-it-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 生成 16kHz 单声道 WAV：n 秒静音 + 中文语音由调用方拼接（这里只做纯静音/方波，语音部分用外部 TTS 文件）
fn write_tone_wav(path: &Path, seconds: f64) {
    let sr = 16000u32;
    let n = (sr as f64 * seconds) as usize;
    let mut data = Vec::with_capacity(n * 2);
    for i in 0..n {
        let v: i16 = if (i / 100) % 2 == 0 { 8000 } else { -8000 };
        data.extend_from_slice(&v.to_le_bytes());
    }
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sr.to_le_bytes());
    wav.extend_from_slice(&(sr * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(path, wav).unwrap();
}

#[test]
fn import_dedup_transcribe_search_export() {
    let root = temp_root("main");
    paths::ensure_dirs(&root).unwrap();
    let db = Db::open(&root).unwrap();
    let library_dir = paths::library_dir(&root);

    // 找一个真实中文语音源：优先用 e2e 测试留下的 TTS WAV，否则用 testdata 目录
    let candidates = [
        std::env::temp_dir().join("soundtrace-e2e").join("speech.wav"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../testdata/会议录音_产品评审.wav"),
    ];
    let Some(source) = candidates.iter().find(|p| p.is_file()).cloned() else {
        eprintln!("跳过：未找到中文语音测试文件（先运行 transcribe 的 e2e 测试）");
        return;
    };

    // ---------- 1) 导入 ----------
    let importer = soundtrace_lib::library::Importer {
        db: &db,
        library_root: library_dir.clone(),
    };
    let result = importer
        .import(&[source.to_string_lossy().to_string()])
        .expect("导入失败");
    assert_eq!(result.imported.len(), 1, "应导入 1 条: {:?}", result.skipped);
    let rec = &result.imported[0];

    // 归档到 library/YYYY-MM/ 且文件存在
    let archived = PathBuf::from(&rec.file_path);
    assert!(archived.is_file(), "归档文件不存在: {}", rec.file_path);
    assert!(
        archived.starts_with(&library_dir),
        "未归档到 library 目录: {}",
        rec.file_path
    );
    // ffprobe 时长
    assert!(rec.duration_sec > 10.0, "时长异常: {}", rec.duration_sec);
    assert!(rec.size_bytes > 0);
    assert!(!rec.file_hash.is_empty());

    // ---------- 2) 去重 ----------
    let again = importer
        .import(&[source.to_string_lossy().to_string()])
        .expect("二次导入失败");
    assert_eq!(again.imported.len(), 0, "重复文件不应再导入");
    assert_eq!(again.skipped.len(), 1);
    assert!(
        again.skipped[0].reason.contains("已导入过"),
        "去重原因异常: {}",
        again.skipped[0].reason
    );
    assert_eq!(importer.list_recordings().unwrap().len(), 1);

    // ---------- 3) 元数据编辑 + 标签 ----------
    let updated = importer
        .update_recording(
            rec.id,
            soundtrace_lib::library::RecordingPatch {
                title: Some("产品评审会议".into()),
                notes: Some("含导出性能与审批流水号议题".into()),
                participants: Some("张伟 李明 王芳".into()),
                recorded_at: None,
                tags: Some(vec!["周会".into(), "产品".into()]),
            },
        )
        .unwrap()
        .expect("更新后应返回记录");
    assert_eq!(updated.title, "产品评审会议");
    assert_eq!(updated.tags.len(), 2);

    // ---------- 4) 转写流水线 ----------
    let models_root = dirs::home_dir().unwrap().join("SoundTrace").join("models");
    if !soundtrace_lib::models::ModelLayout::new(models_root.clone()).all_ready() {
        eprintln!("跳过转写部分：模型未安装");
        return;
    }
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let rows = soundtrace_lib::transcribe::run_pipeline(
        &archived,
        &models_root,
        &paths::cache_dir(&root),
        rec.id,
        &cancel,
        |_| {},
    )
    .expect("转写失败");
    assert!(!rows.is_empty(), "无转写分段");

    {
        let conn = db.conn.lock().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        for r in &rows {
            tx.execute(
                "INSERT INTO segments(recording_id, start_ms, end_ms, text, chars) VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params![rec.id, r.start_ms, r.end_ms, r.text, r.chars],
            )
            .unwrap();
        }
        tx.execute(
            "UPDATE recordings SET status='done' WHERE id=?1",
            [rec.id],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // ---------- 5) 全局搜索 ----------
    let hits = soundtrace_lib::search::search(&db, "导出").unwrap();
    assert!(
        !hits.segments.is_empty(),
        "搜索“导出”应有转写片段命中（转写文本：{}）",
        rows.iter().map(|r| r.text.clone()).collect::<String>()
    );
    let hits_title = soundtrace_lib::search::search(&db, "评审").unwrap();
    assert!(!hits_title.recordings.is_empty(), "标题搜索应命中");
    assert!(
        hits_title.recordings[0].tags.contains(&"周会".to_string()),
        "标签应随搜索结果返回"
    );

    // ---------- 6) 导出 ----------
    let out_dir = root.join("export");
    for kind in ["md", "srt", "txt"] {
        let dest = out_dir.join(format!("out.{kind}"));
        soundtrace_lib::export::export(&db, rec.id, kind, &dest).expect("导出失败");
        let content = std::fs::read_to_string(&dest).unwrap();
        assert!(!content.is_empty(), "{kind} 导出为空");
        assert!(content.contains("产品评审会议"), "{kind} 缺少标题");
    }
    let srt = std::fs::read_to_string(out_dir.join("out.srt")).unwrap();
    assert!(srt.contains("-->"), "SRT 缺少时间轴");
    let md = std::fs::read_to_string(out_dir.join("out.md")).unwrap();
    assert!(md.contains("转写全文"), "MD 缺少转写全文段");

    // ---------- 7) 删除 ----------
    importer.delete_recording(rec.id, true).unwrap();
    assert!(importer.list_recordings().unwrap().is_empty());
    assert!(!archived.exists(), "彻底删除应移除归档文件");
    // 段级联删除
    let seg_count: i64 = {
        let conn = db.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM segments", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(seg_count, 0, "删除录音应级联删除分段");

    let _ = std::fs::remove_dir_all(&root);
    eprintln!("全链路集成测试通过");
}

#[test]
fn tone_wav_helper_smoke() {
    // 保证辅助函数本身可用（导入路径的最小依赖）
    let root = temp_root("tone");
    let wav = root.join("tone.wav");
    write_tone_wav(&wav, 1.0);
    assert!(wav.is_file());
    let _ = std::fs::remove_dir_all(&root);
}
