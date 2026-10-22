//! 转写子进程入口：在**独立进程**里跑完整转写流水线。
//!
//! 存在的唯一理由：原生识别引擎在特定音频内容上会破坏栈保护并让进程直接退出。
//! 把它放到子进程后，崩溃只体现为一个非零退出码，父进程（Tauri 应用）不受影响。
//!
//! 用法：`st-transcribe <recording_id> <audio_file> <models_root> <cache_dir> <out_json> [asr_threads]`
//! 末位 `asr_threads` 可省略（等价于 `auto`）：见 `transcribe::parse_threads`。
//! stdout 每行一个 JSON 进度对象（见 `job_runner::ProgressLine`）；
//! 结束时把分段数组（成功）或 `{"error": "..."}`（失败）写入 `<out_json>`。
//!
//! 注意：不用 `#![windows_subsystem = "windows"]` —— 需要 stdout 才能回报进度。
use soundtrace_lib::job_runner::{PipelineOutput, ProgressLine, SegRow};
use std::io::Write;
use std::sync::atomic::AtomicBool;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // 第 6 个参数（asr_threads）可选：省略与旧版父进程兼容
    if args.len() != 6 && args.len() != 7 {
        eprintln!("用法: {} {}", args[0], soundtrace_lib::job_runner::CHILD_ARG_USAGE);
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
    let asr_threads = args.get(6).map(String::as_str).unwrap_or("auto");

    let cancel = AtomicBool::new(false);
    let stdout = std::io::stdout();
    let result = soundtrace_lib::transcribe::run_pipeline(
        &audio_file,
        &models_root,
        &cache_dir,
        recording_id,
        asr_threads,
        &cancel,
        |value| {
            // 进度按行输出，父进程逐行解析；不缓存，保证崩溃前已发出的进度都送达
            let line = ProgressLine::Progress { value };
            if let Ok(s) = serde_json::to_string(&line) {
                let mut lock = stdout.lock();
                let _ = writeln!(lock, "{s}");
                let _ = lock.flush();
            }
        },
    );

    let payload = match result {
        Ok(rows) => serde_json::to_string(
            &rows
                .iter()
                .map(|r| SegRow {
                    start_ms: r.start_ms,
                    end_ms: r.end_ms,
                    text: r.text.clone(),
                    chars: r.chars.clone(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".into()),
        Err(e) => {
            // 失败也要留下结果文件，父进程据此给出可读原因
            serde_json::to_string(&PipelineOutput::Failure { error: e.msg.clone() })
                .unwrap_or_else(|_| r#"{"error":"转写失败"}"#.into())
        }
    };

    if let Err(e) = std::fs::write(&out_json, payload) {
        eprintln!("写入结果文件失败 {}: {e}", out_json.display());
        std::process::exit(3);
    }
}
