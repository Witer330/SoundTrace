//! 转写作业的**进程隔离**协议。
//!
//! 为什么要有这个模块：`sherpa-onnx` 的 Paraformer 解码在特定音频内容上会破坏栈保护
//! （0xc0000409 / FAST_FAIL_STACK_COOKIE_CHECK_FAILURE），Rust 侧无法用 Result 或
//! catch_unwind 兜住，进程直接退出。既然修不了原生库，就让它在**独立子进程**里崩：
//! 父进程只收到一个非零退出码，应用本身不闪退，任务可标记失败或跳过继续。
//!
//! 协议：
//! - 子进程（`src/bin/st-transcribe.rs`）按行往 stdout 打 JSON 进度对象；
//! - 结束后把结果写成 JSON 文件（成功=分段数组，失败=`{"error": "..."}`）；
//! - 父进程读 stdout 转发进度事件，等子进程退出后读结果文件。
use serde::{Deserialize, Serialize};

/// 子进程每行 stdout 的进度对象
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ProgressLine {
    /// 进度百分比 0..1
    Progress { value: f64 },
    /// 子进程内部的可读错误（此时子进程会以非零码退出）
    Error { msg: String },
}

/// 转写结果分段（父子进程共用的序列化形态）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegRow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// JSON: [[char, ms], ...] 供前端点字跳转
    pub chars: String,
}

/// 子进程结果文件的内容：成功时是分段数组，失败时是 {"error": "..."}
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PipelineOutput {
    Rows(Vec<SegRow>),
    Failure { error: String },
}

/// 子进程命令行参数（顺序固定，便于父进程拼装与子进程解析）。
/// `asr_threads` 可省略，省略等价于 `auto`（见 `transcribe::SETTING_ASR_THREADS`）。
pub const CHILD_ARG_USAGE: &str =
    "<recording_id> <audio_file> <models_root> <cache_dir> <out_json> [asr_threads]";

/// 分离子进程（`src/bin/st-diarize.rs`）的参数：人数必填（锁人数是产品路径，
/// 见 `diar` 模块顶部说明），无「自动」档。
pub const DIAR_CHILD_ARG_USAGE: &str =
    "<recording_id> <audio_file> <models_root> <cache_dir> <out_json> <num_clusters>";

/// 定位子进程可执行文件。
///
/// 查找顺序（命中即返回）：
/// 1. 与主程序同目录 —— 开发时 `cargo build` 把 bin 与 lib 放在同一 profile 目录；
/// 2. 上一级目录 —— 从 `target/<profile>/deps/` 运行测试时，bin 在上一级；
/// 3. `<主程序目录>/Resources/` —— Tauri 打包（bundle.resources）把外部二进制放这里，
///    Windows 上形如 `C:\Program Files\SoundTrace\Resources\st-transcribe.exe`。
pub fn child_exe_path(name: &str) -> crate::error::AppResult<std::path::PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| crate::error::err(format!("无法定位当前可执行文件：{e}")))?;
    let dir = exe
        .parent()
        .ok_or_else(|| crate::error::err("无法定位当前可执行文件所在目录"))?;
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };

    let mut candidates: Vec<std::path::PathBuf> = vec![dir.join(&file)];
    if let Some(parent) = dir.parent() {
        candidates.push(parent.join(&file));
    }
    candidates.push(dir.join("Resources").join(&file));

    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(crate::error::err(format!(
        "找不到子进程引擎 {file}。开发时请执行 `cargo build`；打包前请运行 `node tools/build-sidecar.mjs`。\
         已查找：{}",
        candidates
            .iter()
            .map(|c| c.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    )))
}

/// 把子进程的退出态翻译成用户可读的失败原因。
/// `stderr_tail` 是子进程最后输出的错误片段（原生层的报错在这里，对排查很有用）。
/// 崩溃码的出现说明命中了已知的原生层缺陷，提示要给用户**可执行的下一步**。
pub fn explain_child_exit(code: Option<i32>, stderr_tail: &str) -> String {
    let mut msg = match code {
        Some(0) => "转写引擎未返回结果".to_string(),
        // 0xC0000409 = STATUS_STACK_BUFFER_OVERRUN（fast-fail，栈保护检查失败）
        Some(c) if c as u32 == 0xc000_0409 => {
            "转写引擎在处理这段音频的某些内容时崩溃（0xC0000409 栈保护检查失败）。\
             这是已知的识别引擎缺陷；本次崩溃已被隔离，应用与其他录音不受影响，可稍后重试这条。"
                .to_string()
        }
        Some(c) => format!(
            "转写引擎异常退出（退出码 {:#x}）。本次崩溃已被隔离，应用与其他录音不受影响。",
            c as u32
        ),
        None => "转写引擎被系统强制结束（可能内存不足或被杀毒软件拦截）".to_string(),
    };
    let tail = stderr_tail.trim();
    if !tail.is_empty() {
        let short: String = tail.lines().take(3).collect::<Vec<_>>().join(" / ");
        msg.push_str(&format!(" 引擎输出：{short}"));
    }
    msg
}
