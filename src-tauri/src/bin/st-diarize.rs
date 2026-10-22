//! 说话人分离子进程：在**独立进程**里跑分离流水线（与转写子进程同一工程约束）。
//!
//! 存在的理由与 `st-transcribe` 相同：sherpa-onnx 原生库有破坏栈保护的前科，
//! 放在子进程里崩了只体现为非零退出码，父进程（Tauri 应用）不受影响。
//!
//! 用法：`st-diarize <recording_id> <audio_file> <models_root> <cache_dir> <out_json> <num_clusters>`
//! 人数必填（锁人数是产品路径，见 `diar` 模块顶部说明）。
//! stdout 每行一个 JSON 进度对象（见 `job_runner::ProgressLine`）；
//! 结束时把 `{segments, numSpeakers}`（成功）或 `{"error": "..."}`（失败）写入 `<out_json>`。
//!
//! 注意：不用 `#![windows_subsystem = "windows"]` —— 需要 stdout 才能回报进度。
use soundtrace_lib::diar::DiarResult;
use soundtrace_lib::error::AppResult;
use soundtrace_lib::job_runner::ProgressLine;
use std::io::Write;
use std::sync::atomic::AtomicBool;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 7 {
        eprintln!("用法: {} {}", args[0], soundtrace_lib::job_runner::DIAR_CHILD_ARG_USAGE);
        std::process::exit(2);
    }
    let recording_id: i64 = match args[1].parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("recording_id 不是合法整数: {}", args[1]);
            std::process::exit(2);
        }
    };
    let audio_file = std::path::PathBuf::from(&args[2]);
    let models_root = std::path::PathBuf::from(&args[3]);
    let cache_dir = std::path::PathBuf::from(&args[4]);
    let out_json = std::path::PathBuf::from(&args[5]);
    let num_clusters: i64 = match args[6].parse() {
        Ok(v) if (1..=32).contains(&v) => v,
        _ => {
            eprintln!("num_clusters 必须是 1~32 的整数: {}", args[6]);
            std::process::exit(2);
        }
    };

    let result = run(&audio_file, &models_root, &cache_dir, recording_id, num_clusters);
    let payload = match result {
        Ok(segs) => {
            let n = segs.iter().map(|s| s.speaker).collect::<std::collections::BTreeSet<_>>().len();
            serde_json::to_string(&DiarResult { segments: segs, num_speakers: n })
                .unwrap_or_else(|_| r#"{"error":"序列化分离结果失败"}"#.into())
        }
        Err(e) => serde_json::to_string(&soundtrace_lib::job_runner::PipelineOutput::Failure {
            error: e.msg.clone(),
        })
        .unwrap_or_else(|_| r#"{"error":"说话人分离失败"}"#.into()),
    };

    if let Err(e) = std::fs::write(&out_json, payload) {
        eprintln!("写入结果文件失败 {}: {e}", out_json.display());
        std::process::exit(3);
    }
}

/// 解析 PipelineOutput 复用 job_runner 的失败形态：成功时是 DiarResult，
/// 失败时 {"error": ...}。这里只需要成功路径，错误统一走退出码 + 结果文件。
fn run(
    audio_file: &std::path::Path,
    models_root: &std::path::Path,
    cache_dir: &std::path::Path,
    recording_id: i64,
    num_clusters: i64,
) -> AppResult<Vec<soundtrace_lib::diar::DiarSeg>> {
    let cancel = AtomicBool::new(false);
    let stdout = std::io::stdout();
    soundtrace_lib::diar::run_diarization(
        audio_file,
        models_root,
        cache_dir,
        recording_id,
        num_clusters,
        &cancel,
        |value| {
            let line = ProgressLine::Progress { value };
            if let Ok(s) = serde_json::to_string(&line) {
                let mut lock = stdout.lock();
                let _ = writeln!(lock, "{s}");
                let _ = lock.flush();
            }
        },
    )
}
