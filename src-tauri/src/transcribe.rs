use crate::db::Db;
use crate::error::{err, AppResult};
use crate::job_runner;
use crate::models::ModelLayout;
use crate::paths;
use rusqlite::params;
use serde::Serialize;
use sherpa_onnx::{
    OfflineParaformerModelConfig, OfflinePunctuation, OfflinePunctuationConfig,
    OfflineRecognizer, OfflineRecognizerConfig, VadModelConfig, VoiceActivityDetector, Wave,
};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Emitter;

const SAMPLE_RATE: i32 = 16000;
const VAD_WINDOW: usize = 512;

/// 已知缺陷（2026-09-18 定位）：sherpa-onnx 的 Paraformer 解码在**特定内容**上会破坏栈保护，
/// 表现为 `0xc0000409 / FAST_FAIL_STACK_COOKIE_CHECK_FAILURE` + `fatal runtime error: Rust
/// cannot catch foreign exceptions`，进程直接退出——用户侧看到的是转写中应用闪退。
///
/// 已确立的事实（可复现）：
///   1. 崩溃点在 `OfflineRecognizer::decode()` **内部**，属原生库缺陷，Rust 侧无法捕获或兜底；
///   2. debug 与 release 均复现，与构建优化无关；
///   3. 与音频时长**无单调关系**：同一条录音里 2 秒/6 秒片段必过，而 3.3 秒的一段必崩；
///   4. 与线程数无关（1/2/4/8/20 线程都会崩，线程数只改变复现概率）；
///   5. 是**内容相关**而非系统性缺陷：普通片段连续解码 40 轮零崩溃，问题片段约每 10 轮崩一次；
///   6. 重编码、重采样、限幅、滤波都不能规避，说明不是编码或电平问题。
///
/// 因此不要指望靠切片/限长/换线程来修（都试过，无效）。真正的修法是把转写放到**独立子进程**
/// 里跑，崩了由父进程重启并跳过问题片段——见 docs/开发记录.md 的「已知缺陷」一节。
const _KNOWN_NATIVE_CRASH_NOTE: () = ();

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JobEvent {
    pub recording_id: i64,
    pub status: String, // queued|running|done|failed|canceled
    pub progress: f64,
    pub error: String,
}

/// 长驻转写工作线程：轮询 pending 任务，逐个交给**子进程**执行。
pub struct TranscribeWorker {
    pub db: Arc<Db>,
    pub data_root: PathBuf,
    pub app: tauri::AppHandle,
    pub cancel: Arc<AtomicBool>,
    /// 正在跑的子进程（录音 id, pid）。取消时需要杀掉它才能立即停下。
    pub running: Arc<std::sync::Mutex<Option<(i64, u32)>>>,
}

pub struct JobInfo {
    pub id: i64,
    pub recording_id: i64,
    pub file_path: String,
}

impl TranscribeWorker {
    pub fn spawn(worker: TranscribeWorker) {
        std::thread::spawn(move || worker.run_loop());
    }

    fn run_loop(&self) {
        loop {
            match self.pick_job() {
                Some(job) => self.run_job(job),
                None => std::thread::sleep(Duration::from_millis(700)),
            }
        }
    }

    /// 取最早的 pending 转写任务并置为 running
    fn pick_job(&self) -> Option<JobInfo> {
        let conn = self.db.conn.lock().unwrap();
        let job: Option<(i64, i64)> = conn
            .query_row(
                "SELECT id, recording_id FROM jobs
                 WHERE kind='transcribe' AND status='pending'
                 ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })
            .ok()
            .flatten();
        let (job_id, rec_id) = job?;
        let file_path: String = conn
            .query_row(
                "SELECT file_path FROM recordings WHERE id = ?1",
                [rec_id],
                |r| r.get(0),
            )
            .ok()?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let _ = conn.execute(
            "UPDATE jobs SET status='running', updated_at=?1 WHERE id=?2",
            params![now, job_id],
        );
        let _ = conn.execute(
            "UPDATE recordings SET status='transcribing', updated_at=?1 WHERE id=?2",
            params![now, rec_id],
        );
        drop(conn);
        self.emit(rec_id, "running", 0.0, "");
        Some(JobInfo { id: job_id, recording_id: rec_id, file_path })
    }

    fn run_job(&self, job: JobInfo) {
        let outcome = self.transcribe_one(&job);
        let canceled = self.cancel.load(Ordering::Relaxed);
        let (job_status, rec_status, progress, error) =
            finalize_job(&self.db, &job, outcome, canceled);
        let _ = job_status;
        let _ = rec_status;
        self.emit(job.recording_id, &job_status, progress, &error);
    }

    fn emit(&self, recording_id: i64, status: &str, progress: f64, error: &str) {
        let _ = self.app.emit(
            "job://progress",
            JobEvent {
                recording_id,
                status: status.into(),
                progress,
                error: error.into(),
            },
        );
    }

    /// 在**子进程**里跑完整转写流水线，返回分段行。
    ///
    /// 为什么是子进程：原生识别引擎在特定内容上会破坏栈保护并让进程直接退出，
    /// 放在子进程里跑，崩溃只表现为非零退出码，应用本身不受影响。
    ///
    /// 进度：子进程按行输出 JSON（`job_runner::ProgressLine`），这里逐行解析后转发为 Tauri 事件。
    /// 取消：把 300ms 内的进度静默视作活跃检查点；同时父进程杀掉子进程即可立即停止。
    fn transcribe_one(&self, job: &JobInfo) -> AppResult<Vec<job_runner::SegRow>> {
        let cache = paths::cache_dir(&self.data_root);
        std::fs::create_dir_all(&cache)?;
        let out_json = cache.join(format!("job-{}.json", job.id));
        let _ = std::fs::remove_file(&out_json);

        let exe = child_exe_path()?;
        let mut child = Command::new(&exe)
            .arg(job.recording_id.to_string())
            .arg(&job.file_path)
            .arg(paths::models_dir(&self.data_root))
            .arg(&cache)
            .arg(&out_json)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                err(format!(
                    "启动转写子进程失败：{e}（预期位置 {}）",
                    exe.display()
                ))
            })?;

        let pid = child.id();
        *self.running.lock().unwrap() = Some((job.recording_id, pid));
        // 本任务开始，清掉上一轮可能残留的取消标记
        self.cancel.store(false, Ordering::Relaxed);

        // 进度行读取独立成线程：主线程要同时轮询退出状态与取消请求
        let stdout = child.stdout.take();
        let recording_id = job.recording_id;
        let app = self.app.clone();
        let reader = std::thread::spawn(move || {
            let Some(out) = stdout else { return };
            use std::io::BufRead;
            for line in std::io::BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<job_runner::ProgressLine>(line.trim()) {
                    Ok(job_runner::ProgressLine::Progress { value }) => {
                        use tauri::Emitter;
                        let _ = app.emit(
                            "job://progress",
                            JobEvent {
                                recording_id,
                                status: "running".into(),
                                progress: value,
                                error: String::new(),
                            },
                        );
                    }
                    Ok(job_runner::ProgressLine::Error { msg }) => eprintln!("[转写] {msg}"),
                    // 非协议行（原生库偶发打印）忽略即可
                    Err(_) => {}
                }
            }
        });

        // 每 300ms 检查一次：子进程是否结束 / 用户是否取消
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => {}
                Err(e) => {
                    *self.running.lock().unwrap() = None;
                    return Err(err(format!("等待转写子进程失败：{e}")));
                }
            }
            if self.cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                *self.running.lock().unwrap() = None;
                let _ = std::fs::remove_file(&out_json);
                return Err(err("已取消"));
            }
            std::thread::sleep(Duration::from_millis(300));
        };
        *self.running.lock().unwrap() = None;
        let _ = reader.join();
        // 收集子进程 stderr，崩溃时用于说明原因（截断避免过长）
        let stderr_tail = child
            .stderr
            .take()
            .map(|mut s| {
                use std::io::Read;
                let mut buf = String::new();
                let _ = s.read_to_string(&mut buf);
                buf.chars().take(400).collect::<String>()
            })
            .unwrap_or_default();

        // 先读结果文件：子进程可能以非零码退出但仍写下了可读错误
        let payload = std::fs::read_to_string(&out_json).ok();
        let _ = std::fs::remove_file(&out_json);

        if let Some(raw) = payload {
            match serde_json::from_str::<job_runner::PipelineOutput>(&raw) {
                Ok(job_runner::PipelineOutput::Rows(rows)) => {
                    if !status.success() {
                        // 有结果但退出码非零：仍按成功处理，避免白跑一趟
                        eprintln!("[转写] 子进程退出码异常但已产出结果，按成功处理");
                    }
                    if rows.is_empty() {
                        return Err(err("未识别到任何语音内容"));
                    }
                    return Ok(rows);
                }
                Ok(job_runner::PipelineOutput::Failure { error }) => return Err(err(error)),
                Err(_) => {
                    // 结果文件损坏（例如子进程写到一半被打断）
                }
            }
        }

        Err(err(job_runner::explain_child_exit(status.code(), &stderr_tail)))
    }
}

/// 定位转写子进程可执行文件。
///
/// 查找顺序（命中即返回）：
/// 1. 与主程序同目录 —— 开发时 `cargo build` 把 bin 与 lib 放在同一 profile 目录；
/// 2. 上一级目录 —— 从 `target/<profile>/deps/` 运行测试时，bin 在上一级；
/// 3. `<主程序目录>/Resources/` —— Tauri 打包（bundle.resources）把外部二进制放这里，
///    Windows 上形如 `C:\Program Files\SoundTrace\Resources\st-transcribe.exe`。
fn child_exe_path() -> AppResult<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| err(format!("无法定位当前可执行文件：{e}")))?;
    let dir = exe
        .parent()
        .ok_or_else(|| err("无法定位当前可执行文件所在目录"))?;
    let name = if cfg!(windows) {
        "st-transcribe.exe"
    } else {
        "st-transcribe"
    };

    let mut candidates: Vec<PathBuf> = vec![dir.join(name)];
    if let Some(parent) = dir.parent() {
        candidates.push(parent.join(name));
    }
    candidates.push(dir.join("Resources").join(name));

    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(err(format!(
        "找不到转写引擎。开发时请执行 `cargo build`；打包前请运行 `node tools/build-sidecar.mjs`。\
         已查找：{}",
        candidates
            .iter()
            .map(|c| c.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    )))
}

/// 把一次转写的结果落库，返回 (job 状态, 录音状态, 进度, 错误文本) 供事件推送。
///
/// 成功：整批替换该录音的分段并把两边都置为 done。
/// 失败：区分「用户取消」（录音回到 imported，可重新排队）与「引擎崩溃/报错」
/// （录音置为 failed 并保留原因，用户能看到为什么没成）。崩溃由子进程隔离后走到这里，
/// 因此应用不会闪退，任务状态也能正确收尾。
pub fn finalize_job(
    db: &Arc<Db>,
    job: &JobInfo,
    outcome: AppResult<Vec<job_runner::SegRow>>,
    canceled: bool,
) -> (String, String, f64, String) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    match outcome {
        Ok(rows) => {
            let mut conn = db.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            {
                tx.execute(
                    "DELETE FROM segments WHERE recording_id = ?1",
                    [job.recording_id],
                )
                .unwrap();
                let mut stmt = tx
                    .prepare(
                        "INSERT INTO segments(recording_id, start_ms, end_ms, text, chars)
                         VALUES(?1,?2,?3,?4,?5)",
                    )
                    .unwrap();
                for r in &rows {
                    stmt.execute(params![
                        job.recording_id,
                        r.start_ms,
                        r.end_ms,
                        r.text,
                        r.chars
                    ])
                    .unwrap();
                }
            }
            tx.execute(
                "UPDATE recordings SET status='done', updated_at=?1 WHERE id=?2",
                params![now, job.recording_id],
            )
            .unwrap();
            tx.execute(
                "UPDATE jobs SET status='done', progress=1.0, updated_at=?1 WHERE id=?2",
                params![now, job.id],
            )
            .unwrap();
            tx.commit().unwrap();
            ("done".into(), "done".into(), 1.0, String::new())
        }
        Err(e) => {
            let status = if canceled { "canceled" } else { "failed" };
            let rec_status = if canceled { "imported" } else { "failed" };
            let conn = db.conn.lock().unwrap();
            let _ = conn.execute(
                "UPDATE jobs SET status=?1, error=?2, updated_at=?3 WHERE id=?4",
                params![status, e.msg, now, job.id],
            );
            let _ = conn.execute(
                "UPDATE recordings SET status=?1, updated_at=?2 WHERE id=?3",
                params![rec_status, now, job.recording_id],
            );
            (status.into(), rec_status.into(), 0.0, e.msg)
        }
    }
}

/// 纯核心流水线：解码 → VAD 分段 → Paraformer 识别（字符时间戳）→ 标点。
/// 与 UI 解耦，可独立测试。
pub fn run_pipeline(
    audio_file: &std::path::Path,
    models_root: &std::path::Path,
    cache_dir: &std::path::Path,
    recording_id: i64,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f64),
) -> AppResult<Vec<SegRow>> {
    let layout = ModelLayout::new(models_root.to_path_buf());
    if !layout.all_ready() {
        return Err(err("转写模型未就绪，请先在设置中下载模型"));
    }

    // 1) 解码为 16k 单声道 WAV（缓存在 cache 目录）
    std::fs::create_dir_all(cache_dir)?;
    let wav = cache_dir.join(format!("{recording_id}.wav"));
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i").arg(audio_file)
        .args(["-ac", "1", "-ar", "16000", "-acodec", "pcm_s16le"])
        .arg(&wav)
        .output()
        .map_err(|e| err(format!("调用 ffmpeg 失败: {e}")))?;
    if !out.status.success() {
        return Err(err(format!(
            "ffmpeg 解码失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    let wave = Wave::read(wav.to_string_lossy().as_ref())
        .ok_or_else(|| err("读取解码 WAV 失败"))?;
    let samples = wave.samples();
    let total_samples = samples.len().max(1) as f64;

    // 2) 构建识别器 / VAD / 标点
    let num_threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4);
    let mut asr_config = OfflineRecognizerConfig::default();
    asr_config.model_config.paraformer = OfflineParaformerModelConfig {
        model: Some(layout.asr_model().to_string_lossy().into_owned()),
    };
    asr_config.model_config.tokens =
        Some(layout.asr_tokens().to_string_lossy().into_owned());
    asr_config.model_config.num_threads = num_threads;
    let recognizer = OfflineRecognizer::create(&asr_config)
        .ok_or_else(|| err("加载 Paraformer 模型失败"))?;

    let mut vad_config = VadModelConfig::default();
    vad_config.silero_vad.model = Some(layout.vad_model().to_string_lossy().into_owned());
    vad_config.silero_vad.threshold = 0.5;
    vad_config.silero_vad.min_silence_duration = 0.5;
    vad_config.silero_vad.min_speech_duration = 0.25;
    vad_config.silero_vad.max_speech_duration = 15.0;
    vad_config.silero_vad.window_size = VAD_WINDOW as i32;
    vad_config.sample_rate = SAMPLE_RATE;
    vad_config.num_threads = 1;
    let vad = VoiceActivityDetector::create(&vad_config, 60.0)
        .ok_or_else(|| err("加载 silero-vad 模型失败"))?;

    let mut punct_config = OfflinePunctuationConfig::default();
    punct_config.model.ct_transformer =
        Some(layout.punct_model().to_string_lossy().into_owned());
    let punct = OfflinePunctuation::create(&punct_config)
        .ok_or_else(|| err("加载标点模型失败"))?;

    // 3) VAD 分段 → 逐段识别 → 标点
    let mut rows: Vec<SegRow> = Vec::new();
    let mut processed: usize = 0;
    let mut last_emit = Instant::now();

    let drain = |rows: &mut Vec<SegRow>,
                 vad: &VoiceActivityDetector,
                 recognizer: &OfflineRecognizer,
                 punct: &OfflinePunctuation,
                 processed: &mut usize| {
        while !vad.is_empty() {
            if let Some(seg) = vad.front() {
                let seg_start_sample = seg.start() as usize;
                *processed = (*processed).max(seg_start_sample + seg.samples().len());
                if let Some(row) = recognize_segment(recognizer, punct, &seg) {
                    rows.push(row);
                }
                vad.pop();
            }
        }
    };

    for chunk in samples.chunks(VAD_WINDOW) {
        if cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&wav);
            return Err(err("已取消"));
        }
        vad.accept_waveform(chunk);
        drain(&mut rows, &vad, &recognizer, &punct, &mut processed);
        if last_emit.elapsed() > Duration::from_millis(500) {
            last_emit = Instant::now();
            let pos = ((processed as f64) / total_samples).min(0.99);
            on_progress(pos);
        }
    }
    vad.flush();
    drain(&mut rows, &vad, &recognizer, &punct, &mut processed);

    let _ = std::fs::remove_file(&wav);
    if rows.is_empty() {
        return Err(err("未识别到任何语音内容"));
    }
    Ok(rows)
}

/// 转写结果分段。与 `job_runner::SegRow` 字段一致，后者用于父子进程间序列化。
#[derive(Clone)]
pub struct SegRow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// JSON: [[char, ms], ...] 供前端点字跳转
    pub chars: String,
}

/// 识别一个 VAD 片段：建流、解码、标点，并把字符时间戳映射回全局时间轴。
/// 注意：`decode()` 内部存在已知原生层崩溃（见文件顶部 `_KNOWN_NATIVE_CRASH_NOTE`），
/// 崩溃时整个进程退出，此处无法用 Result 兜住。
fn recognize_segment(
    recognizer: &OfflineRecognizer,
    punct: &OfflinePunctuation,
    seg: &sherpa_onnx::SpeechSegment,
) -> Option<SegRow> {
    let stream = recognizer.create_stream();
    stream.accept_waveform(SAMPLE_RATE, seg.samples());
    recognizer.decode(&stream);
    let result = stream.get_result()?;
    let raw_text = result.text.trim().to_string();
    if raw_text.is_empty() {
        return None;
    }

    let seg_start_sec = seg.start() as f64 / SAMPLE_RATE as f64;
    let timestamps = result.timestamps.clone().unwrap_or_default();

    // 字符级时间映射（tokens 对中文基本是单字）
    let mut chars: Vec<(String, i64)> = Vec::with_capacity(result.tokens.len());
    for (i, tok) in result.tokens.iter().enumerate() {
        let ts = timestamps.get(i).copied().unwrap_or(0.0);
        chars.push((tok.clone(), ((seg_start_sec + ts as f64) * 1000.0).round() as i64));
    }

    let text = punct
        .add_punctuation(&raw_text)
        .unwrap_or(raw_text);

    let start_ms = chars.first().map(|c| c.1).unwrap_or_else(|| (seg_start_sec * 1000.0) as i64);
    let last_ts = timestamps.last().copied().unwrap_or(0.0);
    let end_ms = ((seg_start_sec + last_ts as f64 + 0.5) * 1000.0).round() as i64;

    let chars_json = serde_json::to_string(
        &chars.iter().map(|(t, ms)| (t, ms)).collect::<Vec<_>>(),
    )
    .unwrap_or_default();

    Some(SegRow {
        start_ms: start_ms.max(0),
        end_ms: end_ms.max(start_ms + 200),
        text,
        chars: chars_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端：TTS 生成中文语音 → 下载模型（缺失时）→ 完整转写流水线。
    /// 模型安装到 ~/SoundTrace/models（与正式应用共用，一次下载处处可用）。
    #[test]
    fn e2e_chinese_transcription() {
        // 1) 模型准备
        let home = dirs::home_dir().unwrap();
        let models_root = home.join("SoundTrace").join("models");
        let ids: Vec<String> = vec!["paraformer", "vad", "punct"]
            .into_iter()
            .map(String::from)
            .collect();
        crate::models::install_models(&models_root, &ids, &|_| {}).expect("模型安装失败");

        // 2) Windows TTS 生成中文语音
        let dir = std::env::temp_dir().join("soundtrace-e2e");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("speech.wav");
        let ps = format!(
            r#"
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
try {{ $s.SelectVoice('Microsoft Huihui') }} catch {{ }}
$s.SetOutputToWaveFile('{}')
$s.Rate = 0
$s.Speak('今天我们召开项目复盘会议，首先回顾上周的进展。接口联调已经完成了百分之八十，剩下的工作预计三天内收尾。下一阶段的重点是性能优化，请测试组在周五之前提交压测报告。另外下周一上午十点有客户来访，大家提前准备材料。')
$s.Dispose()
"#,
            wav.to_string_lossy().replace('\\', "\\\\")
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .output()
            .expect("运行 PowerShell 失败");
        assert!(
            out.status.success() && wav.is_file(),
            "TTS 生成失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // 3) 转写流水线
        let cancel = AtomicBool::new(false);
        let rows = run_pipeline(&wav, &models_root, &dir, -1, &cancel, |_| {})
            .expect("转写流水线失败");

        let full: String = rows.iter().map(|r| r.text.clone()).collect();
        eprintln!("--- 分段数: {}", rows.len());
        eprintln!("--- 识别结果: {full}");
        for r in rows.iter().take(3) {
            eprintln!("--- [{}ms~{}ms] {}", r.start_ms, r.end_ms, r.text);
        }

        // 中文内容识别成功（TTS 文本含大量中文）
        let cjk_count = full.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
        assert!(cjk_count >= 20, "中文字符过少（{cjk_count}）: {full}");
        // 出现原句关键词之一
        let keywords = ["会议", "进展", "优化", "报告", "客户", "材料"];
        assert!(
            keywords.iter().any(|k| full.contains(k)),
            "未命中任何关键词: {full}"
        );
        // 时间戳单调不减
        for w in rows.windows(2) {
            assert!(w[0].start_ms <= w[1].start_ms, "时间戳非单调");
        }
        // 标点已恢复
        assert!(
            full.contains('。') || full.contains('，'),
            "未恢复标点: {full}"
        );
        // chars JSON 可解析且带时间
        let chars: Vec<(String, i64)> = serde_json::from_str(&rows[0].chars).unwrap();
        assert!(!chars.is_empty());
        assert!(chars[0].1 >= 0);

        let _ = std::fs::remove_file(&wav);
    }
}
