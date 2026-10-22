//! 说话人自动分离（V2）：pyannote 分段 + CAM++ 声纹 + 锁人数聚类 + 小簇吸收。
//!
//! 为什么是「锁人数」而不是自动阈值（2026-09-18 实测，见 examples/diar_spike.rs）：
//! sherpa-onnx 的 `FastClusteringConfig::threshold` 是余弦距离（小于才合并），默认 0.5
//! 在真实会议音频上几乎不合并——同一条录音 0.5 → 60+ 簇，提到 0.85 仍把主讲人拆成两半。
//! 而 `num_clusters` 指定人数后聚类在结构上不可能过拆：16.2 分钟录音强制 3 人得到
//! 10.4% / 88.8% / 0.9%，吸收掉 0.9% 的碎片簇后是干净的两人分布。
//! 人数由用户指定（`participants` 预填），改个数重跑分离的代价只有几分钟。
//!
//! 与转写同一套工程约束：引擎跑在**独立子进程**（src/bin/st-diarize.rs），
//! 因为 sherpa-onnx 原生库有破坏栈保护的前科（见 `transcribe.rs` 顶部注释）。
//! 本模块同时被主进程（映射与入库）和子进程（推理）引用。

use crate::error::{err, AppResult};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::atomic::AtomicBool;

/// 分离分段的最短语音时长：低于它的窗口不产出，碎片是伪聚类的主要来源
/// （实测 min_duration_on 0.3 → 0.6，短片段占比 38% → 20%）
pub const MIN_DURATION_ON: f32 = 0.6;
pub const MIN_DURATION_OFF: f32 = 0.5;

/// 「伪聚类」判定：总语音占比低于该值的簇不视为真人（spike 实测同款判据）
pub const SPURIOUS_SHARE: f64 = 0.05;

/// 碎片吸收：两个分段间隙小于该值视为「接触」，接触最多的邻簇是并入对象
pub const ABSORB_GAP_SEC: f64 = 0.5;

/// 映射回转写分段时的最低主导占比：一段里最大说话人重叠不足 7 成就不标注，
/// 留给手工修正（实测约 8% 的段落在这一档）
pub const MAP_MIN_SHARE: f64 = 0.7;

/// 人数设置键：`diar.clusters.<recording_id>`（与 speakers.<id> 同一 KV 惯例）
pub fn clusters_setting_key(recording_id: i64) -> String {
    format!("diar.clusters.{recording_id}")
}

/// 分离结果中的一段（子进程 → 父进程的序列化形态；speaker 为 1 起的簇编号）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarSeg {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: i64,
}

/// 子进程结果文件的内容
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarResult {
    pub segments: Vec<DiarSeg>,
    /// 吸收后的互异说话人数
    pub num_speakers: usize,
}

/// 子进程结果文件的完整形态：成功是 [`DiarResult`]，失败是 {"error": "..."}
/// （与 `job_runner::PipelineOutput` 同一惯例，但分段类型不同）
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiarOut {
    Ok(DiarResult),
    Failure { error: String },
}

// ---------- 后处理 ----------

/// 小簇吸收：把总语音占比 < [`SPURIOUS_SHARE`] 的簇并入「与它接触最多」的邻簇。
/// 接触分 = 重叠时长 + 间隙衰减的邻接分；无任何接触的孤立碎片（咳嗽、摩擦声）
/// 并入占比最大的簇——不能让它自成一「人」，也不能因它终止循环。
/// 每轮处理占比最小的簇，直到没有疑似伪簇或只剩一簇。
/// 返回合并记录 [(被并入的簇, 并入的簇)]。
pub fn absorb_spurious(segs: &mut Vec<DiarSeg>) -> Vec<(i64, i64)> {
    let mut log = Vec::new();
    loop {
        let total: f64 = segs.iter().map(|s| (s.end_ms - s.start_ms) as f64).sum();
        if segs.is_empty() || total <= 0.0 {
            break;
        }
        let mut share: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
        for s in segs.iter() {
            *share.entry(s.speaker).or_insert(0.0) += (s.end_ms - s.start_ms) as f64 / total;
        }
        if share.len() <= 1 {
            break;
        }
        // 占比最小的簇若不低于门槛，就没有疑似伪簇了
        let Some((&victim, _)) = share.iter().min_by(|a, b| a.1.total_cmp(b.1)) else {
            break;
        };
        if share[&victim] >= SPURIOUS_SHARE {
            break;
        }
        let mut score: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
        for v in segs.iter().filter(|s| s.speaker == victim) {
            for t in segs.iter().filter(|s| s.speaker != victim) {
                let overlap = (v.end_ms.min(t.end_ms) - v.start_ms.max(t.start_ms)).max(0) as f64 / 1000.0;
                let gap = (v.start_ms.max(t.start_ms) - v.end_ms.min(t.end_ms)).max(0) as f64 / 1000.0;
                if overlap > 0.0 {
                    *score.entry(t.speaker).or_insert(0.0) += overlap;
                } else if gap <= ABSORB_GAP_SEC {
                    *score.entry(t.speaker).or_insert(0.0) += ABSORB_GAP_SEC - gap;
                }
            }
        }
        let target = if let Some((&t, _)) = score.iter().max_by(|a, b| a.1.total_cmp(b.1)) {
            t
        } else {
            let (&big, _) = share.iter().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
            big
        };
        for s in segs.iter_mut() {
            if s.speaker == victim {
                s.speaker = target;
            }
        }
        log.push((victim, target));
    }
    log
}

/// 簇编号归一化：按首次出现顺序重编为 1..k（前端配色与命名按 1 起编号）。
/// 返回互异簇数。
pub fn normalize_ids(segs: &mut [DiarSeg]) -> usize {
    let mut mapping: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
    for s in segs.iter_mut() {
        let next = mapping.len() as i64 + 1;
        let slot = mapping.entry(s.speaker).or_insert(next);
        s.speaker = *slot;
    }
    mapping.len()
}

/// 把分离结果映射到转写分段：按重叠时长取主导说话人，
/// 主导占比 >= `min_share` 才指派，不足返回 None（不标注，留给手工）。
/// `spans` 是 (start_ms, end_ms)，与 `segs` 都按时间轴对齐的毫秒值。
pub fn map_speakers(segs: &[DiarSeg], spans: &[(i64, i64)], min_share: f64) -> Vec<Option<i64>> {
    spans
        .iter()
        .map(|&(start_ms, end_ms)| {
            let dur = (end_ms - start_ms) as f64;
            if dur <= 0.0 {
                return None;
            }
            let mut by_spk: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
            for d in segs {
                let overlap = (end_ms.min(d.end_ms) - start_ms.max(d.start_ms)).max(0) as f64;
                if overlap > 0.0 {
                    *by_spk.entry(d.speaker).or_insert(0.0) += overlap;
                }
            }
            let best = by_spk.values().cloned().fold(0.0f64, f64::max);
            if best / dur >= min_share {
                by_spk
                    .into_iter()
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(spk, _)| spk)
            } else {
                None
            }
        })
        .collect()
}

// ---------- 子进程流水线 ----------

/// 完整分离流水线：解码 → pyannote 分段 + CAM++ 声纹 → 锁人数聚类 → 吸收 → 归一化。
/// 与 UI 解耦、在子进程里运行；`on_progress` 只有粗粒度的阶段点
/// （分离本身是单次推理调用，没有内部进度回调）。
pub fn run_diarization(
    audio_file: &std::path::Path,
    models_root: &std::path::Path,
    cache_dir: &std::path::Path,
    recording_id: i64,
    num_clusters: i64,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f64),
) -> AppResult<Vec<DiarSeg>> {
    if !(1..=32).contains(&num_clusters) {
        return Err(err(format!("说话人数必须是 1~32，收到 {num_clusters}")));
    }
    let layout = crate::models::ModelLayout::new(models_root.to_path_buf());
    if !layout.speaker_ready() {
        return Err(err("说话人模型未就绪，请先在设置中下载模型"));
    }

    // 1) 解码为 16k 单声道 WAV（与转写共用同一预处理约定，缓存在 cache 目录）
    std::fs::create_dir_all(cache_dir)?;
    let wav = cache_dir.join(format!("{recording_id}.diar.wav"));
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(audio_file)
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
    on_progress(0.15);

    use sherpa_onnx::Wave;
    let wave = Wave::read(wav.to_string_lossy().as_ref())
        .ok_or_else(|| err("读取解码 WAV 失败"))?;
    on_progress(0.25);
    // 拷成自有 Vec 并及时释放 Wave：Windows 上 Wave 持有的句柄会让删除 WAV 静默失败
    let samples: Vec<f32> = wave.samples().to_vec();
    drop(wave);
    let _ = std::fs::remove_file(&wav);
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(err("已取消"));
    }

    // 2) 分离（单次推理调用，耗时主体，无内部进度）
    use sherpa_onnx::{
        FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
        OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
        SpeakerEmbeddingExtractorConfig,
    };
    let threads = crate::cpu::auto_threads() as i32;
    let mut config = OfflineSpeakerDiarizationConfig::default();
    config.segmentation = OfflineSpeakerSegmentationModelConfig {
        pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
            model: Some(layout.seg_model().to_string_lossy().into_owned()),
            window_shift_ratio: 0.1,
        },
        num_threads: threads,
        debug: false,
        provider: Some("cpu".into()),
    };
    config.embedding = SpeakerEmbeddingExtractorConfig {
        model: Some(layout.speaker_model().to_string_lossy().into_owned()),
        num_threads: threads,
        debug: false,
        provider: Some("cpu".into()),
    };
    config.clustering = FastClusteringConfig {
        num_clusters: num_clusters as i32,
        threshold: 0.5, // num_clusters > 0 时被忽略
        compute_confidence: false,
    };
    config.min_duration_on = MIN_DURATION_ON;
    config.min_duration_off = MIN_DURATION_OFF;

    let diar = OfflineSpeakerDiarization::create(&config)
        .ok_or_else(|| err("加载说话人分离模型失败"))?;
    let expected = diar.sample_rate();
    if expected != 16000 {
        return Err(err(format!("分离模型期望 {expected} Hz，与 16k 解码不匹配")));
    }
    let raw = diar
        .process(&samples)
        .ok_or_else(|| err("说话人分离推理失败"))?;
    on_progress(0.9);

    // 3) 吸收 + 归一化（与 spike 相同的后处理，参数经过实测标定）
    let mut segs: Vec<DiarSeg> = raw
        .sort_by_start_time()
        .into_iter()
        .map(|s| DiarSeg {
            start_ms: (s.start as f64 * 1000.0).round() as i64,
            end_ms: (s.end as f64 * 1000.0).round() as i64,
            speaker: s.speaker as i64,
        })
        .collect();
    absorb_spurious(&mut segs);
    let n = normalize_ids(&mut segs);
    if n == 0 {
        return Err(err("未分离出任何说话人（音频可能没有语音）"));
    }
    on_progress(1.0);
    Ok(segs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: i64, end_ms: i64, speaker: i64) -> DiarSeg {
        DiarSeg { start_ms, end_ms, speaker }
    }

    /// 吸收：占比 <5% 的簇并入接触最多的邻簇；>=5% 的真簇不能被动
    #[test]
    fn absorb_merges_spurious_into_touching_neighbor() {
        // spk_1 是主讲（约 95%），spk_2 只在语句间冒出 3 次（共 1.2 秒 ≈ 3.8%），spk_3 一段
        let mut segs = vec![
            seg(0, 10_000, 1),
            seg(10_200, 10_600, 2), // 紧邻 spk_1
            seg(20_000, 30_000, 1),
            seg(30_100, 30_500, 2),
            seg(40_000, 50_000, 1),
            seg(50_200, 50_600, 2),
            seg(60_000, 60_400, 3), // 与 spk_1 接触
        ];
        let log = absorb_spurious(&mut segs);
        assert_eq!(log.len(), 2, "两个伪簇都该被吸收: {log:?}");
        assert!(segs.iter().all(|s| s.speaker == 1), "应全部并入 spk_1: {segs:?}");
    }

    /// 吸收：与谁都没有接触的孤立碎片并入最大簇，且循环不提前终止
    #[test]
    fn absorb_handles_isolated_fragment_without_stopping() {
        let mut segs = vec![
            seg(0, 90_000, 1),      // 90%
            seg(100_000, 104_000, 2), // 4%，与 3 有接触
            seg(200_000, 200_300, 3), // 0.3%，远离一切（咳嗽）
        ];
        let log = absorb_spurious(&mut segs);
        assert_eq!(log.len(), 2, "孤立碎片也必须被吸收: {log:?}");
        assert!(segs.iter().all(|s| s.speaker == 1));
    }

    /// 吸收：没有伪簇时原样返回；只剩一簇时终止
    #[test]
    fn absorb_noop_when_all_significant() {
        let mut segs = vec![seg(0, 60_000, 1), seg(60_500, 120_000, 2)];
        let log = absorb_spurious(&mut segs);
        assert!(log.is_empty());
        assert_eq!(segs.len(), 2);
    }

    /// 归一化：稀疏标签重编为 1..k（前端按 1 起编号配色/命名）
    #[test]
    fn normalize_relabels_by_first_appearance() {
        let mut segs = vec![seg(0, 1000, 7), seg(2000, 3000, 7), seg(4000, 5000, 2)];
        let n = normalize_ids(&mut segs);
        assert_eq!(n, 2);
        assert_eq!(segs[0].speaker, 1);
        assert_eq!(segs[1].speaker, 1);
        assert_eq!(segs[2].speaker, 2);
    }

    /// 映射：主导占比足够才指派；跨人段落不标注；无覆盖返回 None
    #[test]
    fn map_assigns_dominant_or_none() {
        let diar = vec![
            seg(0, 10_000, 1),
            seg(10_000, 20_000, 2),
            seg(30_000, 40_000, 1),
        ];
        let spans = vec![
            (0, 9_000),      // 90% spk_1 → 1
            (10_000, 20_000), // 100% spk_2 → 2
            (5_000, 15_000),  // 各一半 → None
            (50_000, 60_000), // 无覆盖 → None
        ];
        let got = map_speakers(&diar, &spans, MAP_MIN_SHARE);
        assert_eq!(got, vec![Some(1), Some(2), None, None]);
    }

    /// 人数设置的键
    #[test]
    fn clusters_key_format() {
        assert_eq!(clusters_setting_key(23), "diar.clusters.23");
    }
}
