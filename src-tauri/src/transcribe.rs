use crate::db::Db;
use crate::error::{err, AppResult};
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

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JobEvent {
    pub recording_id: i64,
    pub status: String, // queued|running|done|failed|canceled
    pub progress: f64,
    pub error: String,
}

/// 长驻转写工作线程：轮询 pending 任务，逐个执行。
pub struct TranscribeWorker {
    pub db: Arc<Db>,
    pub data_root: PathBuf,
    pub app: tauri::AppHandle,
    pub cancel: Arc<AtomicBool>,
}

struct JobInfo {
    id: i64,
    recording_id: i64,
    file_path: String,
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
        self.cancel.store(false, Ordering::Relaxed);
        let outcome = self.transcribe_one(&job);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let (job_status, rec_status, progress, error) = match outcome {
            Ok(rows) => {
                let mut conn = self.db.conn.lock().unwrap();
                let tx = conn.transaction().unwrap();
                {
                    tx.execute("DELETE FROM segments WHERE recording_id = ?1", [job.recording_id])
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
                drop(conn);
                ("done", "done", 1.0, "".to_string())
            }
            Err(e) => {
                let canceled = self.cancel.load(Ordering::Relaxed);
                let status = if canceled { "canceled" } else { "failed" };
                let rec_status = if canceled { "imported" } else { "failed" };
                let conn = self.db.conn.lock().unwrap();
                let _ = conn.execute(
                    "UPDATE jobs SET status=?1, error=?2, updated_at=?3 WHERE id=?4",
                    params![status, e.msg, now, job.id],
                );
                let _ = conn.execute(
                    "UPDATE recordings SET status=?1, updated_at=?2 WHERE id=?3",
                    params![rec_status, now, job.recording_id],
                );
                drop(conn);
                (status, rec_status, 0.0, e.msg)
            }
        };
        let _ = job_status;
        let _ = rec_status;
        self.emit(job.recording_id, job_status, progress, &error);
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

    /// 单条录音完整转写流水线，返回分段行
    fn transcribe_one(&self, job: &JobInfo) -> AppResult<Vec<SegRow>> {
        let cache = paths::cache_dir(&self.data_root);
        let cancel = self.cancel.clone();
        let app = self.app.clone();
        let recording_id = job.recording_id;
        run_pipeline(
            std::path::Path::new(&job.file_path),
            &paths::models_dir(&self.data_root),
            &cache,
            job.recording_id,
            &cancel,
            move |progress| {
                use tauri::Emitter;
                let _ = app.emit(
                    "job://progress",
                    JobEvent {
                        recording_id,
                        status: "running".into(),
                        progress,
                        error: String::new(),
                    },
                );
            },
        )
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

pub struct SegRow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// JSON: [[char, ms], ...] 供前端点字跳转
    pub chars: String,
}

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
