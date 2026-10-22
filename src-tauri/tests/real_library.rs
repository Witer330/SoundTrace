//! 真实录音全流程验证：对着用户自己的录音目录跑「导入 → 去重 → 元数据 → 波形峰值 →
//! 转写 → 分段 → 搜索 → 导出 → 删除」，用真实的 aac/m4a 手机录音验证整条链路。
//!
//! 目录不存在时自动跳过（不在别人机器上失败）。
//! 环境变量：
//!   ST_REAL_DIR   录音目录，默认 `C:\Users\witer\Documents\录音`
//!   ST_VERIFY_N   转写前 N 个最短文件（默认 1；设 0 只验证导入与峰值）

use soundtrace_lib::db::Db;
use soundtrace_lib::library::Importer;
use soundtrace_lib::paths;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soundtrace-real-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn real_dir() -> PathBuf {
    std::env::var("ST_REAL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Users\witer\Documents\录音"))
}

/// ST_ONLY_FILE 指定单个音频文件（用于对某个具体文件做二分/片段验证，跳过目录扫描）
fn only_file() -> Option<PathBuf> {
    std::env::var("ST_ONLY_FILE").ok().map(PathBuf::from)
}

fn models_root() -> PathBuf {
    std::env::var("ST_MODELS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap().join("SoundTrace").join("models"))
}

/// 隔离性质测试：已知会让原生识别引擎崩溃的音频，交给**子进程**跑时，
/// 子进程会异常退出，而父进程（跑这个测试的进程）必须完好无损。
///
/// 这是整个子进程架构存在的理由——如果哪天有人把转写改回进程内执行，
/// 这个测试会以「测试进程自己被 0xc0000409 带走」的方式失败。
#[test]
fn child_process_isolates_native_crash() {
    let Some(clip) = std::env::var("ST_CRASH_CLIP").ok().map(PathBuf::from) else {
        eprintln!("跳过：未设置 ST_CRASH_CLIP（指向已知会崩溃的音频）");
        return;
    };
    if !clip.is_file() {
        eprintln!("跳过：{} 不存在", clip.display());
        return;
    }
    let mroot = models_root();
    if !soundtrace_lib::models::ModelLayout::new(mroot.clone()).all_ready() {
        eprintln!("跳过：模型未就绪");
        return;
    }

    // 子进程可执行文件就在测试 exe 的同级目录（target/debug/deps/ 的上一级）
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

    let cache = std::env::temp_dir().join("st-crash-isolation");
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();
    let out_json = cache.join("out.json");

    println!("拉起子进程处理已知崩溃片段…（父进程存活即为通过）");
    let output = std::process::Command::new(&child_exe)
        .arg("1")
        .arg(&clip)
        .arg(&mroot)
        .arg(&cache)
        .arg(&out_json)
        .output()
        .expect("无法启动子进程");

    println!("子进程退出码：{:?}", output.status.code());
    println!(
        "子进程 stderr：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );

    assert!(
        !output.status.success(),
        "该片段本应让原生层崩溃，子进程却正常退出——样本失效或缺陷已修，请更新样本"
    );
    // 崩溃前已发出的进度应当留在 stdout 里（说明进度是流式的）
    let stdout = String::from_utf8_lossy(&output.stdout);
    let has_progress = stdout.lines().any(|l| l.contains("\"type\":\"progress\""));
    println!("崩溃前是否已输出进度：{has_progress}");
    assert!(has_progress, "崩溃前的进度应已通过 stdout 送出");

    // 关键断言：走到这里说明父进程没被带走
    println!("✅ 父进程存活：原生崩溃已被隔离在子进程内");

    // 退出码可翻译成用户可读原因
    let msg = soundtrace_lib::job_runner::explain_child_exit(output.status.code(), &String::from_utf8_lossy(&output.stderr));
    println!("给用户看的原因：{msg}");
    assert!(!msg.is_empty());
}

/// 隔离探针：把 VAD 与识别器分开跑，定位原生崩溃到底发生在哪一层。
/// 用法：ST_PROBE_WAV=xxx.wav cargo test --test real_library probe_vad_or_asr -- --nocapture
#[test]
fn probe_vad_or_asr() {
    use sherpa_onnx::{
        OfflineParaformerModelConfig, OfflineRecognizer, OfflineRecognizerConfig, VadModelConfig,
        VoiceActivityDetector, Wave,
    };
    let Ok(wav) = std::env::var("ST_PROBE_WAV") else {
        eprintln!("跳过：未设置 ST_PROBE_WAV");
        return;
    };
    let mroot = models_root();
    let layout = soundtrace_lib::models::ModelLayout::new(mroot.clone());
    if !layout.all_ready() {
        eprintln!("跳过：模型未就绪");
        return;
    }
    let wave = Wave::read(&wav).expect("读取 WAV 失败");
    let samples = wave.samples().to_vec();
    println!(
        "样本 {} 个（{:.1} 秒）",
        samples.len(),
        samples.len() as f64 / 16000.0
    );

    let mode = std::env::var("ST_PROBE_MODE").unwrap_or_else(|_| "vad".into());
    println!("模式：{mode}");

    if mode == "vad" || mode == "both" {
        println!("--- 只听 VAD ---");
        let mut cfg = VadModelConfig::default();
        cfg.silero_vad.model = Some(layout.vad_model().to_string_lossy().into_owned());
        cfg.silero_vad.threshold = 0.5;
        cfg.silero_vad.min_silence_duration = 0.5;
        cfg.silero_vad.min_speech_duration = 0.25;
        cfg.silero_vad.max_speech_duration = 15.0;
        cfg.silero_vad.window_size = 512;
        cfg.sample_rate = 16000;
        cfg.num_threads = 1;
        let vad = VoiceActivityDetector::create(&cfg, 60.0).expect("VAD 创建失败");
        let mut segs = 0usize;
        let mut longest = 0usize;
        for chunk in samples.chunks(512) {
            vad.accept_waveform(chunk);
            while !vad.is_empty() {
                if let Some(seg) = vad.front() {
                    segs += 1;
                    longest = longest.max(seg.samples().len());
                }
                vad.pop();
            }
        }
        vad.flush();
        while !vad.is_empty() {
            if let Some(seg) = vad.front() {
                segs += 1;
                longest = longest.max(seg.samples().len());
            }
            vad.pop();
        }
        println!(
            "VAD 完成：{segs} 段，最长 {:.2} 秒",
            longest as f64 / 16000.0
        );
    }

    if mode == "asr" || mode == "both" {
        println!("--- 只跑识别器（6 秒/块）---");
        let mut cfg = OfflineRecognizerConfig::default();
        cfg.model_config.paraformer = OfflineParaformerModelConfig {
            model: Some(layout.asr_model().to_string_lossy().into_owned()),
        };
        cfg.model_config.tokens = Some(layout.asr_tokens().to_string_lossy().into_owned());
        cfg.model_config.num_threads = 4;
        let rec = OfflineRecognizer::create(&cfg).expect("识别器创建失败");
        let mut n = 0usize;
        for (i, chunk) in samples.chunks(6 * 16000).enumerate() {
            let stream = rec.create_stream();
            stream.accept_waveform(16000, chunk);
            rec.decode(&stream);
            if let Some(r) = stream.get_result() {
                if !r.text.trim().is_empty() {
                    n += 1;
                }
            }
            println!("  块 {i} 完成");
        }
        println!("识别器完成：{n} 个非空块");
    }

    // 直接看识别器在这段音频上的原始输出（崩溃前能打印到哪一步）
    if mode == "raw" {
        let nthr: i32 = std::env::var("ST_PROBE_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        let mut acfg = OfflineRecognizerConfig::default();
        acfg.model_config.paraformer = OfflineParaformerModelConfig {
            model: Some(layout.asr_model().to_string_lossy().into_owned()),
        };
        acfg.model_config.tokens = Some(layout.asr_tokens().to_string_lossy().into_owned());
        acfg.model_config.num_threads = nthr;
        println!("--- 创建识别器（num_threads={nthr}）---");
        let rec = OfflineRecognizer::create(&acfg).expect("识别器创建失败");
        let repeat: usize = std::env::var("ST_PROBE_REPEAT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        for i in 0..repeat {
            println!("--- 第 {i} 轮 ---");
            let stream = rec.create_stream();
            stream.accept_waveform(16000, &samples);
            println!("--- 开始解码（这里就是崩溃点）---");
            rec.decode(&stream);
            let r = stream.get_result();
            println!(
                "--- 解码完成：tokens={} ---",
                r.as_ref().map(|x| x.tokens.len()).unwrap_or(0)
            );
            if i == 0 {
                if let Some(r) = r {
                    println!(
                        "文本（前 100 字）：{}",
                        r.text.chars().take(100).collect::<String>()
                    );
                    println!(
                        "时间戳个数：{}",
                        r.timestamps.as_ref().map(|t| t.len()).unwrap_or(0)
                    );
                }
            }
        }
        println!("raw 探针结束（{repeat} 轮无崩溃）");
    }

    // 按 VAD 切段后整段送识别器（复现流水线路径，定位崩在哪个段）
    if mode == "segments" {
        println!("--- VAD 切段后整段送识别器 ---");
        let mut vcfg = VadModelConfig::default();
        vcfg.silero_vad.model = Some(layout.vad_model().to_string_lossy().into_owned());
        vcfg.silero_vad.threshold = 0.5;
        vcfg.silero_vad.min_silence_duration = 0.5;
        vcfg.silero_vad.min_speech_duration = 0.25;
        vcfg.silero_vad.max_speech_duration = 15.0;
        vcfg.silero_vad.window_size = 512;
        vcfg.sample_rate = 16000;
        vcfg.num_threads = 1;
        let vad = VoiceActivityDetector::create(&vcfg, 60.0).expect("VAD 创建失败");
        let mut segs: Vec<(usize, Vec<f32>)> = Vec::new();
        for chunk in samples.chunks(512) {
            vad.accept_waveform(chunk);
            while !vad.is_empty() {
                if let Some(seg) = vad.front() {
                    segs.push((seg.start() as usize, seg.samples().to_vec()));
                }
                vad.pop();
            }
        }
        vad.flush();
        while !vad.is_empty() {
            if let Some(seg) = vad.front() {
                segs.push((seg.start() as usize, seg.samples().to_vec()));
            }
            vad.pop();
        }

        let mut acfg = OfflineRecognizerConfig::default();
        acfg.model_config.paraformer = OfflineParaformerModelConfig {
            model: Some(layout.asr_model().to_string_lossy().into_owned()),
        };
        acfg.model_config.tokens = Some(layout.asr_tokens().to_string_lossy().into_owned());
        acfg.model_config.num_threads = 4;
        let rec = OfflineRecognizer::create(&acfg).expect("识别器创建失败");

        for (i, (start, s)) in segs.iter().enumerate() {
            println!(
                "  段 {i}: 起点 {:.1}s 长度 {:.2}s —— 送入识别器…",
                *start as f64 / 16000.0,
                s.len() as f64 / 16000.0
            );
            let stream = rec.create_stream();
            stream.accept_waveform(16000, s);
            rec.decode(&stream);
            let text = stream
                .get_result()
                .map(|r| r.text.trim().to_string())
                .unwrap_or_default();
            println!("    完成：{}", text.chars().take(30).collect::<String>());
        }
        println!("整段识别完成（没有崩溃）");
    }
    println!("探针结束（没有崩溃）");
}

#[test]
fn real_recordings_full_pipeline() {
    let single = only_file();
    let src_dir = real_dir();
    if single.is_none() && !src_dir.is_dir() {
        eprintln!("跳过：录音目录不存在 {}", src_dir.display());
        return;
    }

    let root = temp_root("main");
    paths::ensure_dirs(&root).unwrap();
    let db = Db::open(&root).unwrap();
    let library_dir = paths::library_dir(&root);
    let importer = Importer {
        db: &db,
        library_root: library_dir.clone(),
    };

    // ---------- 1) 递归展开目录（或直接用指定文件） ----------
    let t0 = Instant::now();
    let files = match &single {
        Some(f) => vec![f.to_string_lossy().to_string()],
        None => soundtrace_lib::library::expand_audio_paths(&[src_dir.to_string_lossy().to_string()])
            .expect("展开目录失败"),
    };
    println!("展开耗时 {:?}，发现 {} 个音频文件", t0.elapsed(), files.len());
    assert!(!files.is_empty(), "没有可识别的音频");

    // ---------- 2) 批量导入（哈希去重 + ffprobe 元数据 + 归档） ----------
    let t1 = Instant::now();
    let result = importer.import(&files).expect("导入失败");
    let import_took = t1.elapsed();
    println!(
        "导入完成：新增 {} 条，跳过 {} 条，耗时 {:?}",
        result.imported.len(),
        result.skipped.len(),
        import_took
    );
    for s in result.skipped.iter().take(5) {
        println!("  跳过 {}：{}", s.path, s.reason);
    }
    assert_eq!(
        result.imported.len() + result.skipped.len(),
        files.len(),
        "每条文件都应被处理（导入或跳过）"
    );

    // 元数据完整性：时长、大小、哈希、归档路径
    let mut total_duration = 0.0f64;
    for r in &result.imported {
        assert!(r.duration_sec > 0.0, "{} 时长为 0（ffprobe 未取到）", r.title);
        assert!(r.size_bytes > 0, "{} 大小异常", r.title);
        assert!(!r.file_hash.is_empty(), "{} 哈希为空", r.title);
        let archived = Path::new(&r.file_path);
        assert!(archived.is_file(), "归档文件不存在：{}", r.file_path);
        assert!(
            archived.starts_with(&library_dir),
            "未归档到库目录：{}",
            r.file_path
        );
        total_duration += r.duration_sec;
    }
    println!(
        "累计音频时长 {:.1} 小时（平均 {:.1} 分钟/条）",
        total_duration / 3600.0,
        total_duration / 60.0 / result.imported.len().max(1) as f64
    );

    // aac 是否被识别（用户目录里有 4 个 .aac）
    let aac_count = result
        .imported
        .iter()
        .filter(|r| r.orig_name.to_ascii_lowercase().ends_with(".aac"))
        .count();
    println!("其中 .aac 文件 {} 条", aac_count);

    // ---------- 3) 重复导入应全部被去重 ----------
    let again = importer.import(&files).expect("二次导入失败");
    assert_eq!(again.imported.len(), 0, "重复导入不应新增");
    assert_eq!(again.skipped.len(), files.len(), "重复导入应全部跳过");
    println!("去重 OK：二次导入全部跳过");
    assert_eq!(importer.list_recordings().unwrap().len(), result.imported.len());

    // ---------- 4) 波形峰值（取最短的一条，验证量级与缓存复用） ----------
    let shortest = result
        .imported
        .iter()
        .min_by(|a, b| a.duration_sec.partial_cmp(&b.duration_sec).unwrap())
        .unwrap();
    println!(
        "波形测试样本：{}（{:.1} 分钟）",
        shortest.title,
        shortest.duration_sec / 60.0
    );
    let t2 = Instant::now();
    let peaks =
        soundtrace_lib::audio::compute_peaks(Path::new(&shortest.file_path)).expect("峰值计算失败");
    let peak_took = t2.elapsed();
    let expected = (shortest.duration_sec * 10.0) as usize; // 100ms 一点
    println!(
        "峰值 {} 点（预期约 {}），耗时 {:?}",
        peaks.len(),
        expected,
        peak_took
    );
    assert!(!peaks.is_empty(), "峰值不能为空");
    // 100ms 一点；允许与 ffprobe 时长有 25% 偏差（容器时长与解码时长可能有差）
    let ratio = peaks.len() as f64 / expected.max(1) as f64;
    assert!(
        (0.75..=1.25).contains(&ratio),
        "峰值点数与时长量级不符：{} 点 vs 预期 {}（比值 {:.2}）",
        peaks.len(),
        expected,
        ratio
    );
    assert!(
        peaks.iter().all(|p| (0.0..=1.0).contains(p)),
        "峰值应在 0..1 之间"
    );
    // 写回 BLOB 再读回，验证复用路径
    let blob = soundtrace_lib::audio::peaks_to_blob(&peaks);
    let back = soundtrace_lib::audio::blob_to_peaks(&blob);
    assert_eq!(back.len(), peaks.len(), "BLOB 往返点数不一致");
    println!("峰值 BLOB 往返 OK（{} 字节）", blob.len());

    // ---------- 5) 转写流水线（真实文件 + 真实模型） ----------
    let n: usize = std::env::var("ST_VERIFY_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    if n == 0 {
        println!("ST_VERIFY_N=0，跳过转写");
        return;
    }
    let mroot = models_root();
    if !soundtrace_lib::models::ModelLayout::new(mroot.clone()).all_ready() {
        eprintln!("跳过转写：模型未安装（{}）", mroot.display());
        return;
    }

    let mut by_duration: Vec<_> = result.imported.iter().collect();
    // 默认从最短开始（快速冒烟）；ST_VERIFY_LONGEST=1 从最长开始（测真实吞吐，短文件 RTF 偏高不代表长会议）
    if std::env::var("ST_VERIFY_LONGEST").is_ok() {
        by_duration.sort_by(|a, b| b.duration_sec.partial_cmp(&a.duration_sec).unwrap());
    } else {
        by_duration.sort_by(|a, b| a.duration_sec.partial_cmp(&b.duration_sec).unwrap());
    }

    let mut rtf_samples: Vec<(String, f64, f64)> = Vec::new();
    for rec in by_duration.into_iter().take(n) {
        println!(
            "开始转写：{}（{:.1} 分钟）",
            rec.title,
            rec.duration_sec / 60.0
        );
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let t3 = Instant::now();
        println!(
            "  参数：线程数 {}（ST_ASR_THREADS 可覆盖，用于扫 RTF 曲线）",
            soundtrace_lib::transcribe::resolve_threads("auto")
        );
        let rows = soundtrace_lib::transcribe::run_pipeline(
            Path::new(&rec.file_path),
            &mroot,
            &paths::cache_dir(&root),
            rec.id,
            "auto",
            &cancel,
            |p| {
                if (p * 100.0) as i32 % 25 == 0 {
                    print!("\r  进度 {:.0}%", p * 100.0);
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
            },
        )
        .expect("转写失败");
        let took = t3.elapsed();
        println!(
            "\r  完成：{} 段，耗时 {:.1} 秒（音频 {:.1} 分钟，RTF {:.3}）        ",
            rows.len(),
            took.as_secs_f64(),
            rec.duration_sec / 60.0,
            took.as_secs_f64() / rec.duration_sec.max(1.0)
        );
        assert!(!rows.is_empty(), "「{}」没有转写出任何分段", rec.title);

        // 时间戳单调、文本非空、字符时间轴可解析
        let mut last_start = -1i64;
        let mut nonempty = 0;
        for r in &rows {
            assert!(r.start_ms >= last_start, "分段起点非单调：{last_start} → {}", r.start_ms);
            last_start = r.start_ms;
            assert!(r.end_ms > r.start_ms, "分段区间非法：{}..{}", r.start_ms, r.end_ms);
            if !r.text.trim().is_empty() {
                nonempty += 1;
            }
            if r.chars != "[]" {
                let parsed: Vec<(String, i64)> =
                    serde_json::from_str(&r.chars).expect("字符时间轴不是合法 JSON");
                assert!(!parsed.is_empty());
                assert!(
                    parsed.windows(2).all(|w| w[0].1 <= w[1].1),
                    "字符时间轴非单调（点字跳转会倒退）"
                );
            }
        }
        println!("  时间戳单调 OK，非空分段 {}/{}", nonempty, rows.len());
        assert!(nonempty > 0, "全部段落都是空文本");

        // 入库
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

        // 从转写文本里取一段真实词做检索验证（用第一段的前若干字符）
        let probe: String = rows
            .iter()
            .find(|r| r.text.chars().count() >= 2)
            .map(|r| r.text.chars().take(2).collect())
            .unwrap_or_default();
        if probe.chars().count() >= 2 {
            let hits = soundtrace_lib::search::search(&db, &probe).expect("搜索失败");
            println!(
                "  搜索「{}」：命中录音 {} 条 / 片段 {} 条",
                probe,
                hits.recordings.len(),
                hits.segments.len()
            );
            assert!(
                !hits.segments.is_empty(),
                "用转写稿里的原文「{probe}」搜索应至少命中一条片段"
            );
        }

        rtf_samples.push((
            rec.title.clone(),
            rec.duration_sec,
            took.as_secs_f64(),
        ));
    }

    // ---------- 6) 导出三种格式 ----------
    let done = importer
        .list_recordings()
        .unwrap()
        .into_iter()
        .find(|r| r.status == "done")
        .expect("应有已转写的录音");
    let out_dir = std::env::temp_dir().join("st-real-export");
    std::fs::create_dir_all(&out_dir).unwrap();
    // 给一条录音加个标题，验证 Markdown 头部
    let mut patch = soundtrace_lib::library::RecordingPatch::default();
    patch.title = Some("真实录音流程验证".into());
    importer.update_recording(done.id, patch).unwrap();

    for kind in ["txt", "srt", "md"] {
        let dest = out_dir.join(format!("out.{kind}"));
        soundtrace_lib::export::export(&db, done.id, kind, &dest).expect("导出失败");
        let content = std::fs::read_to_string(&dest).expect("导出文件不可读");
        assert!(!content.trim().is_empty(), "{kind} 导出为空");
        println!(
            "导出 {kind}：{} 字节，前 60 字：{}",
            content.len(),
            content.chars().take(60).collect::<String>().replace('\n', " ")
        );
        if kind == "srt" {
            assert!(content.contains("-->"), "SRT 缺少时间轴标记");
            assert!(content.contains("00:00"), "SRT 缺少起始时间");
        }
    }

    // ---------- 7) 删除（级联清掉分段） ----------
    let victim = done.id;
    importer.delete_recording(victim, false).expect("删除失败");
    {
        let conn = db.conn.lock().unwrap();
        let left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM segments WHERE recording_id=?1",
                [victim],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "删除录音后分段未被级联清理");
    }
    println!("删除与级联清理 OK");

    // ---------- 8) RTF 汇总 ----------
    if !rtf_samples.is_empty() {
        println!("\n转写性能：");
        let mut total_audio = 0.0;
        let mut total_took = 0.0;
        for (title, dur, took) in &rtf_samples {
            println!(
                "  {title}: 音频 {:.1} 分钟，耗时 {:.1} 秒，RTF {:.3}",
                dur / 60.0,
                took,
                took / dur.max(1.0)
            );
            total_audio += dur;
            total_took += took;
        }
        let rtf = total_took / total_audio.max(1.0);
        println!(
            "  合计音频 {:.1} 小时 → 平均 RTF {:.3}，全量 {:.1} 小时音频预计 {:.1} 小时",
            total_audio / 3600.0,
            rtf,
            total_audio / 3600.0,
            total_audio / 3600.0 * rtf
        );
    }
    println!("\n全流程通过（真实录音 {} 个文件）", files.len());
}
