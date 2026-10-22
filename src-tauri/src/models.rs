use crate::error::{err, AppResult};
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::Emitter;

/// 模型仓库布局与下载管理。
/// 目录：<data_root>/models/{paraformer, vad, punct}
pub struct ModelLayout {
    pub root: PathBuf,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub id: String,
    pub name: String,
    pub ready: bool,
    /// 已存在文件的字节数（未就绪时用于显示下载量）
    pub present_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub model_id: String,
    pub bytes: u64,
    pub total: u64,
    /// 当前正在下载的文件名
    pub file: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DownloadDone {
    pub ok: bool,
    pub error: String,
}

const PARA_MODEL_URL: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-paraformer-zh-2023-09-14/resolve/main/model.int8.onnx";
const PARA_TOKENS_URL: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-paraformer-zh-2023-09-14/resolve/main/tokens.txt";
const VAD_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx";
const PUNCT_TGZ_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/punctuation-models/sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8.tar.bz2";
// 说话人分离（V2）：pyannote 分段 + CAM++ 声纹。下载 URL 已核实（2026-09-19）
const SEG_TGZ_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
const CAMPPLUS_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx";

impl ModelLayout {
    pub fn new(models_root: PathBuf) -> Self {
        ModelLayout { root: models_root }
    }

    pub fn asr_model(&self) -> PathBuf {
        self.root.join("paraformer").join("model.int8.onnx")
    }
    pub fn asr_tokens(&self) -> PathBuf {
        self.root.join("paraformer").join("tokens.txt")
    }
    pub fn vad_model(&self) -> PathBuf {
        self.root.join("vad").join("silero_vad.onnx")
    }
    pub fn punct_model(&self) -> PathBuf {
        self.root
            .join("punct")
            .join("sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8")
            .join("model.int8.onnx")
    }
    /// 说话人分离的分段模型（pyannote，fp32）
    pub fn seg_model(&self) -> PathBuf {
        self.root.join("seg").join("model.onnx")
    }
    /// 说话人声纹模型（3D-Speaker CAM++，中文 16k）
    pub fn speaker_model(&self) -> PathBuf {
        self.root.join("speaker").join("campplus_zh_16k.onnx")
    }

    pub fn status(&self) -> Vec<ModelStatus> {
        let para_ready = self.asr_model().is_file() && self.asr_tokens().is_file();
        let para_bytes = file_size(&self.asr_model()).unwrap_or(0);
        let vad_ready = self.vad_model().is_file();
        let punct_ready = self.punct_model().is_file();
        vec![
            ModelStatus {
                id: "paraformer".into(),
                name: "Paraformer-large（中文识别 · 字符级时间戳）".into(),
                ready: para_ready,
                present_bytes: para_bytes,
                total_bytes: 243_371_218,
            },
            ModelStatus {
                id: "vad".into(),
                name: "silero-vad（语音活动检测）".into(),
                ready: vad_ready,
                present_bytes: file_size(&self.vad_model()).unwrap_or(0),
                total_bytes: 2_281_296,
            },
            ModelStatus {
                id: "punct".into(),
                name: "CT-Transformer（自动标点）".into(),
                ready: punct_ready,
                present_bytes: if punct_ready {
                    file_size(&self.punct_model()).unwrap_or(0)
                } else {
                    0
                },
                total_bytes: 75_497_472,
            },
            ModelStatus {
                id: "seg".into(),
                name: "pyannote（说话人分段 · 分离用）".into(),
                ready: self.seg_model().is_file(),
                present_bytes: file_size(&self.seg_model()).unwrap_or(0),
                // tar.bz2 包整体大小（包内含 fp32/int8 两份，安装取 fp32 的 model.onnx）
                total_bytes: 6_958_444,
            },
            ModelStatus {
                id: "campplus".into(),
                name: "CAM++（声纹提取 · 分离用）".into(),
                ready: self.speaker_model().is_file(),
                present_bytes: file_size(&self.speaker_model()).unwrap_or(0),
                total_bytes: 28_281_138,
            },
        ]
    }

    /// 转写就绪（不含说话人模型——转写不依赖它们）
    pub fn all_ready(&self) -> bool {
        self.asr_model().is_file()
            && self.asr_tokens().is_file()
            && self.vad_model().is_file()
            && self.punct_model().is_file()
    }

    /// 说话人分离就绪
    pub fn speaker_ready(&self) -> bool {
        self.seg_model().is_file() && self.speaker_model().is_file()
    }
}

fn file_size(p: &Path) -> Option<u64> {
    fs::metadata(p).ok().map(|m| m.len())
}

/// 在目录的浅层（含一层子目录）里找指定文件名
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_file() && p.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(p);
        }
    }
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            for inner in fs::read_dir(&p).ok()?.flatten() {
                let ip = inner.path();
                if ip.is_file() && ip.file_name().map(|n| n == name).unwrap_or(false) {
                    return Some(ip);
                }
            }
        }
    }
    None
}

/// 删除目录下的一层子目录（解压 tar 后的清理；失败的静默忽略，不影响就绪判定）
fn cleanup_extracted_dirs(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let _ = fs::remove_dir_all(&p);
            }
        }
    }
}

/// 后台线程下载缺失模型，通过事件汇报进度：models://progress / models://done
pub fn download_models(app: tauri::AppHandle, layout: ModelLayout, ids: Vec<String>) {
    std::thread::spawn(move || {
        let app2 = app.clone();
        let result = install_models(
            &layout.root,
            &ids,
            &move |p: DownloadProgress| {
                let _ = app2.emit("models://progress", p);
            },
        );
        let _ = app.emit(
            "models://done",
            DownloadDone {
                ok: result.is_ok(),
                error: result.err().map(|e| e.msg).unwrap_or_default(),
            },
        );
    });
}

/// 下载并安装指定模型（与 UI 解耦，可测试）。emit 回调用于进度上报。
pub fn install_models(
    models_root: &Path,
    ids: &[String],
    emit: &dyn Fn(DownloadProgress),
) -> AppResult<()> {
    let layout = ModelLayout::new(models_root.to_path_buf());
    for id in ids {
        match id.as_str() {
            "paraformer" => {
                fs::create_dir_all(layout.root.join("paraformer"))?;
                download_file(emit, "paraformer", PARA_MODEL_URL, &layout.asr_model())?;
                download_file(emit, "paraformer", PARA_TOKENS_URL, &layout.asr_tokens())?;
            }
            "vad" => {
                fs::create_dir_all(layout.root.join("vad"))?;
                download_file(emit, "vad", VAD_URL, &layout.vad_model())?;
            }
            "punct" => {
                let punct_dir = layout.root.join("punct");
                fs::create_dir_all(&punct_dir)?;
                let tbz = punct_dir.join("punct.tar.bz2");
                download_file(emit, "punct", PUNCT_TGZ_URL, &tbz)?;
                // Windows 10+ 自带 bsdtar（-j 为 bzip2）
                let out = Command::new("tar")
                    .args(["-xjf"])
                    .arg(&tbz)
                    .arg("-C")
                    .arg(&punct_dir)
                    .output()
                    .map_err(|e| err(format!("调用 tar 失败: {e}")))?;
                if !out.status.success() {
                    return Err(err(format!(
                        "解压失败: {}",
                        String::from_utf8_lossy(&out.stderr)
                    )));
                }
                let _ = fs::remove_file(&tbz);
                if !layout.punct_model().is_file() {
                    return Err(err("解压后未找到 model.int8.onnx"));
                }
            }
            "seg" => {
                let seg_dir = layout.root.join("seg");
                fs::create_dir_all(&seg_dir)?;
                let tbz = seg_dir.join("seg.tar.bz2");
                download_file(emit, "seg", SEG_TGZ_URL, &tbz)?;
                let out = Command::new("tar")
                    .args(["-xjf"])
                    .arg(&tbz)
                    .arg("-C")
                    .arg(&seg_dir)
                    .output()
                    .map_err(|e| err(format!("调用 tar 失败: {e}")))?;
                if !out.status.success() {
                    return Err(err(format!(
                        "解压失败: {}",
                        String::from_utf8_lossy(&out.stderr)
                    )));
                }
                let _ = fs::remove_file(&tbz);
                // 包内是 <子目录>/model.onnx：把它挪到 seg/model.onnx，再清掉解压残留
                if !layout.seg_model().is_file() {
                    let found = find_file(&seg_dir, "model.onnx")
                        .ok_or_else(|| err("解压后未找到 model.onnx"))?;
                    fs::rename(&found, layout.seg_model())?;
                }
                cleanup_extracted_dirs(&seg_dir);
            }
            "campplus" => {
                let spk_dir = layout.root.join("speaker");
                fs::create_dir_all(&spk_dir)?;
                download_file(emit, "campplus", CAMPPLUS_URL, &layout.speaker_model())?;
            }
            other => return Err(err(format!("未知模型: {other}"))),
        }
    }
    Ok(())
}

fn download_file(
    emit: &dyn Fn(DownloadProgress),
    model_id: &str,
    url: &str,
    dest: &Path,
) -> AppResult<()> {
    if dest.is_file() {
        return Ok(()); // 已存在，跳过
    }
    let file_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(86400))
        .build()
        .map_err(|e| err(format!("创建 HTTP 客户端失败: {e}")))?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| err(format!("下载失败 {url}: {e}")))?
        .error_for_status()
        .map_err(|e| err(format!("下载失败 {url}: {e}")))?;

    let total = resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let part = dest.with_extension("part");
    let mut file = fs::File::create(&part)?;
    let mut bytes: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    let mut buf = vec![0u8; 256 * 1024];
    use std::io::Read;
    loop {
        let n = resp
            .read(&mut buf)
            .map_err(|e| err(format!("下载中断 {url}: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        bytes += n as u64;
        if last_emit.elapsed().as_millis() > 300 {
            last_emit = std::time::Instant::now();
            emit(DownloadProgress {
                model_id: model_id.into(),
                bytes,
                total,
                file: file_name.clone(),
            });
        }
    }
    drop(file);
    fs::rename(&part, dest)?;
    Ok(())
}
