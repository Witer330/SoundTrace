//! 说话人分离实测探针（临时诊断工具，测完即删）。
//!
//! 回答三个问题：
//!   1. 过聚类（一个人被拆成多个）对聚类阈值有多敏感——**实测（2026-09-18）threshold
//!      语义是余弦距离，小于阈值才合并，因此「越大聚类越少」**：0.35→74 簇、0.45→58、
//!      0.65→33（300 秒截取）。要靠阈值自动收敛，得往高段（≥0.65）找点位；
//!   2. 「小簇吸收」后处理能消掉多少伪聚类（占比 < SPURIOUS_SHARE 的簇
//!      并入时间上接触最多的邻簇），每个配置给出吸收前后两行；
//!   3. 分离边界与已有转写分段错配到什么程度（决定映射规则能否用「取重叠多数」）。
//!
//! 用法：
//!   cargo run --example diar_spike -- <音频文件> [recording_id] [扫描秒数] [num_clusters] [threshold]
//!
//! 后两个参数控制「全片」那一遍的聚类配置（默认 -1 / 0.5，即 sherpa-onnx 默认）。
//! 指定 num_clusters 是为了在已知人数的条件下拿到不被过聚类污染的对齐指标。
//!
//! 只读音频与数据库，不写任何应用数据。跑在独立进程里，因为 sherpa-onnx
//! 原生库有破坏栈保护的前科（见 `transcribe.rs` 顶部注释），崩了不能连累 app。

use rusqlite::OpenFlags;
use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractorConfig, Wave,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 短片段判据：低于这个时长的分段视为碎片，是过聚类的嫌疑信号
const SHORT_SEG_SEC: f64 = 1.0;

/// 扫描用的聚类阈值序列。0.5 是 sherpa-onnx 默认值；threshold 是余弦距离，
/// 小于阈值才合并，越大聚类越少——第一轮实测（同一条录音）0.35→74 簇、
/// 0.45→58、0.65→33，说明默认值附近几乎不合并，有效区间在高段
const SWEEP_THRESHOLDS: [f32; 4] = [0.5, 0.65, 0.75, 0.85];

/// 判定「伪聚类」的占比门槛（吸收前后共用同一判据，方便对比）
const SPURIOUS_SHARE: f64 = 0.05;

/// 碎片吸收：两个分段的间隙小于该值视为「接触」，接触越多越可能同属一人
const ABSORB_GAP_SEC: f64 = 0.5;

/// 与 `sherpa_onnx::OfflineSpeakerDiarizationSegment` 对应的自有分段。
/// 后处理（吸收）与统计都在它上面做，不依赖 sherpa 的类型。
#[derive(Clone, Copy, Debug)]
struct DiarSeg {
    start: f64,
    end: f64,
    speaker: i32,
    confidence: f32,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "用法: {} <音频文件> [recording_id] [扫描秒数] [num_clusters] [threshold]",
            args[0]
        );
        std::process::exit(2);
    }
    let audio = PathBuf::from(&args[1]);
    let recording_id: Option<i64> = args.get(2).and_then(|s| s.parse().ok());
    let sweep_secs: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(300.0);
    let full_clusters: i32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(-1);
    let full_threshold: f32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let full_min_on: f32 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(0.3);

    if !audio.is_file() {
        eprintln!("音频文件不存在: {}", audio.display());
        std::process::exit(2);
    }

    let data_root = resolve_data_root();
    let seg_model = data_root.join("models/seg/model.onnx");
    let emb_model = data_root.join("models/speaker/campplus_zh_16k.onnx");
    for p in [&seg_model, &emb_model] {
        if !p.is_file() {
            eprintln!("缺少模型: {}", p.display());
            std::process::exit(2);
        }
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4);

    println!("=== 探针配置 ===");
    println!("音频      : {}", audio.display());
    println!("分段模型  : {}", seg_model.display());
    println!("声纹模型  : {}", emb_model.display());
    println!("线程      : {threads}");
    println!("数据根目录: {}", data_root.display());

    // 1) 解码为 16k 单声道 WAV，参数与 transcribe.rs 完全一致
    let wav = std::env::temp_dir().join(format!("st_diar_spike_{}.wav", std::process::id()));
    if let Err(e) = decode_16k_mono(&audio, &wav) {
        eprintln!("解码失败: {e}");
        std::process::exit(1);
    }
    let wave = match Wave::read(wav.to_string_lossy().as_ref()) {
        Some(w) => w,
        None => {
            eprintln!("读取解码 WAV 失败");
            std::process::exit(1);
        }
    };
    // 拷成自有 Vec 并及时释放 Wave：Windows 上 Wave 持有的句柄会让删除 WAV 静默失败
    let samples: Vec<f32> = wave.samples().to_vec();
    drop(wave);
    let _ = std::fs::remove_file(&wav);
    let samples = samples.as_slice();
    let dur = samples.len() as f64 / 16000.0;
    println!("解码结果  : {} 样本 / {:.1} 秒 / 16000 Hz", samples.len(), dur);

    // 2) 全片按指定配置跑一遍，拿头条数据
    println!(
        "\n=== 全片分离（num_clusters = {full_clusters}, threshold = {full_threshold}, min_duration_on = {full_min_on}）==="
    );
    let full_segs = match run_diar(
        &seg_model,
        &emb_model,
        threads,
        full_threshold,
        full_clusters,
        full_min_on,
        true,
        samples,
    ) {
        Some(s) => s,
        None => {
            eprintln!("分离失败（模型加载或推理失败）");
            let _ = std::fs::remove_file(&wav);
            std::process::exit(1);
        }
    };
    println!("—— 吸收前 ——");
    print_detail(&full_segs, &summarize(&full_segs), dur);

    let (absorbed, absorb_log) = absorb_spurious(&full_segs);
    if absorb_log.is_empty() {
        println!("\n小簇吸收：无疑似伪簇（占比 < {SPURIOUS_SHARE:.0}% 的簇），未做任何合并");
    } else {
        println!("\n—— 吸收后（{} 次合并）——", absorb_log.len());
        for (victim, target) in &absorb_log {
            println!("  spk_{victim} → spk_{target}");
        }
        print_detail(&absorbed, &summarize(&absorbed), dur);
    }

    // 3) 阈值扫描：截取前 N 秒做相对比较，省时间。每个配置输出吸收前后两行
    let n = ((sweep_secs * 16000.0) as usize).min(samples.len());
    let excerpt = &samples[..n];
    let ex_dur = n as f64 / 16000.0;
    println!("\n=== 阈值扫描（前 {ex_dur:.0} 秒截取，各配置对比）===");
    println!(
        "{:<24} {:>6} {:>10} {:>10}  按占比排序的说话人分布",
        "配置", "人数", "短片段占比", "疑伪聚类"
    );

    for t in SWEEP_THRESHOLDS {
        if let Some(segs) = run_diar(&seg_model, &emb_model, threads, t, -1, 0.3, true, excerpt) {
            print_sweep_row(&format!("threshold = {t}"), &summarize(&segs));
            let (absorbed, _) = absorb_spurious(&segs);
            print_sweep_row("  ↳ 吸收后", &summarize(&absorbed));
        }
    }
    // 已知人数参照：用 num_clusters 指定，过聚类在结构上不可能发生
    for k in [2, 3] {
        if let Some(segs) = run_diar(&seg_model, &emb_model, threads, 0.5, k, 0.3, true, excerpt) {
            print_sweep_row(&format!("num_clusters = {k}"), &summarize(&segs));
            let (absorbed, _) = absorb_spurious(&segs);
            print_sweep_row("  ↳ 吸收后", &summarize(&absorbed));
        }
    }

    // 4) 与已有转写分段对齐（用吸收后的标签，才是产品实际会呈现的结果）
    if let Some(rid) = recording_id {
        println!("\n=== 与已有转写分段对齐（recording_id = {rid}，吸收后标签）===");
        match load_segments(&data_root, rid) {
            Ok(segs) => align_report(&absorbed, &summarize(&absorbed), &segs),
            Err(e) => eprintln!("读取分段失败: {e}"),
        }
    } else {
        println!("\n（未传 recording_id，跳过分段对齐分析）");
    }

    let _ = std::fs::remove_file(&wav);
    println!("\n完成。");
}

// ---------- 分离 ----------

/// 跑一次分离。返回按时间排序的分段；失败返回 None。
/// 每次都新建 diarizer：宁可多花一点模型加载时间，也不依赖 set_config 的状态语义。
fn run_diar(
    seg_model: &Path,
    emb_model: &Path,
    threads: i32,
    threshold: f32,
    num_clusters: i32,
    min_duration_on: f32,
    confidence: bool,
    samples: &[f32],
) -> Option<Vec<DiarSeg>> {
    let mut config = OfflineSpeakerDiarizationConfig::default();
    config.segmentation = OfflineSpeakerSegmentationModelConfig {
        pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
            model: Some(seg_model.to_string_lossy().into_owned()),
            window_shift_ratio: 0.1,
        },
        num_threads: threads,
        debug: false,
        provider: Some("cpu".into()),
    };
    config.embedding = SpeakerEmbeddingExtractorConfig {
        model: Some(emb_model.to_string_lossy().into_owned()),
        num_threads: threads,
        debug: false,
        provider: Some("cpu".into()),
    };
    config.clustering = FastClusteringConfig {
        num_clusters,
        threshold,
        compute_confidence: confidence,
    };
    config.min_duration_on = min_duration_on;
    config.min_duration_off = 0.5;

    let diar = OfflineSpeakerDiarization::create(&config)?;
    let expected = diar.sample_rate();
    if expected != 16000 {
        eprintln!("警告：模型期望 {expected} Hz，喂入的是 16000 Hz");
    }
    let result = diar.process(samples)?;
    Some(
        result
            .sort_by_start_time()
            .into_iter()
            .map(|s| DiarSeg {
                start: s.start as f64,
                end: s.end as f64,
                speaker: s.speaker,
                confidence: s.confidence,
            })
            .collect(),
    )
}

// ---------- 小簇吸收 ----------

/// 把总语音占比 < SPURIOUS_SHARE 的簇并入「与它接触最多」的邻簇。
/// 接触分：重叠时长 + 间隙衰减的邻接分（间隙 <= ABSORB_GAP_SEC 才算邻接）。
/// 每轮处理占比最小的簇，直到没有疑似伪簇或只剩一簇。
/// 返回 (吸收后的分段, 合并记录 [(被并入的簇, 并入的簇)])。
fn absorb_spurious(segs: &[DiarSeg]) -> (Vec<DiarSeg>, Vec<(i32, i32)>) {
    let mut segs = segs.to_vec();
    let mut log = Vec::new();
    loop {
        let total: f64 = segs.iter().map(|s| s.end - s.start).sum();
        if segs.is_empty() || total <= 0.0 {
            break;
        }
        let mut share: BTreeMap<i32, f64> = BTreeMap::new();
        for s in &segs {
            *share.entry(s.speaker).or_insert(0.0) += (s.end - s.start) / total;
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
        // 各候选簇与 victim 的接触分：重叠直接按时长计，紧邻按间隙衰减计
        let mut score: BTreeMap<i32, f64> = BTreeMap::new();
        for v in segs.iter().filter(|s| s.speaker == victim) {
            for t in segs.iter().filter(|s| s.speaker != victim) {
                let overlap = (v.end.min(t.end) - v.start.max(t.start)).max(0.0);
                let gap = (v.start.max(t.start) - v.end.min(t.end)).max(0.0);
                if overlap > 0.0 {
                    *score.entry(t.speaker).or_insert(0.0) += overlap;
                } else if gap <= ABSORB_GAP_SEC {
                    *score.entry(t.speaker).or_insert(0.0) += ABSORB_GAP_SEC - gap;
                }
            }
        }
        // 没有任何接触的孤立碎片（咳嗽、摩擦声）：并入占比最大的簇。
        // 不能让它自成一「人」，也不能因它终止整个吸收循环。
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
    (segs, log)
}

// ---------- 指标 ----------

struct Summary {
    num_speakers: usize,
    total_speech: f64,
    short_total: usize,
    seg_total: usize,
    /// (说话人, 占比, 分段数, 短片段数)
    per_speaker: Vec<(i32, f64, usize, usize)>,
    conf_min: f32,
    conf_max: f32,
    median_conf: f64,
}

fn summarize(segs: &[DiarSeg]) -> Summary {
    let mut by: BTreeMap<i32, (f64, usize, usize)> = BTreeMap::new();
    let mut total = 0.0;
    let mut short_total = 0usize;
    let mut confs: Vec<f64> = Vec::new();
    for s in segs {
        let d = s.end - s.start;
        total += d;
        let is_short = d < SHORT_SEG_SEC;
        if is_short {
            short_total += 1;
        }
        let e = by.entry(s.speaker).or_insert((0.0, 0, 0));
        e.0 += d;
        e.1 += 1;
        if is_short {
            e.2 += 1;
        }
        confs.push(s.confidence as f64);
    }
    // 人数 = 互异标签数。聚类的标签是稀疏的（max id 可能远大于簇数），
    // 不能用 max(id)+1
    let num_speakers = by.len();
    let per_speaker = by
        .into_iter()
        .map(|(spk, (d, n, sh))| (spk, if total > 0.0 { d / total } else { 0.0 }, n, sh))
        .collect();
    Summary {
        num_speakers,
        total_speech: total,
        short_total,
        seg_total: segs.len(),
        per_speaker,
        conf_min: segs.iter().map(|x| x.confidence).fold(f32::MAX, f32::min),
        conf_max: segs.iter().map(|x| x.confidence).fold(f32::MIN, f32::max),
        median_conf: median(confs),
    }
}

fn print_detail(segs: &[DiarSeg], s: &Summary, audio_dur: f64) {
    println!(
        "语音总时长: {:.1} 秒 / 音频 {:.1} 秒（覆盖率 {:.0}%）",
        s.total_speech,
        audio_dur,
        if audio_dur > 0.0 {
            s.total_speech / audio_dur * 100.0
        } else {
            0.0
        }
    );
    println!("说话人数  : {}（原始分段 {} 个）", s.num_speakers, s.seg_total);
    println!(
        "短片段    : {} / {} = {:.0}%  （<{SHORT_SEG_SEC:.1} 秒）",
        s.short_total,
        s.seg_total,
        if s.seg_total > 0 {
            s.short_total as f64 / s.seg_total as f64 * 100.0
        } else {
            0.0
        }
    );
    println!(
        "\n{:<8} {:>9} {:>8} {:>7} {:>9} {:>9}",
        "说话人", "时长(秒)", "占比", "段数", "短片段数", "中位时长"
    );
    for (spk, share, n, sh) in &s.per_speaker {
        let durs: Vec<f64> = segs
            .iter()
            .filter(|x| x.speaker == *spk)
            .map(|x| x.end - x.start)
            .collect();
        println!(
            "{:<8} {:>9.1} {:>7.1}% {:>7} {:>9} {:>9.2}",
            format!("spk_{spk}"),
            s.total_speech * share,
            share * 100.0,
            n,
            sh,
            median(durs)
        );
    }
    println!(
        "置信度    : 中位 {:.4}，范围 {:.4} ~ {:.4}",
        s.median_conf, s.conf_min, s.conf_max
    );
}

fn print_sweep_row(label: &str, s: &Summary) {
    let spurious = s
        .per_speaker
        .iter()
        .filter(|(_, share, _, _)| *share < SPURIOUS_SHARE)
        .count();
    let dist = s
        .per_speaker
        .iter()
        .map(|(spk, share, _, _)| format!("spk_{spk}:{:.0}%", share * 100.0))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "{:<24} {:>6} {:>9.0}% {:>10}  {}",
        label,
        s.num_speakers,
        if s.seg_total > 0 {
            s.short_total as f64 / s.seg_total as f64 * 100.0
        } else {
            0.0
        },
        spurious,
        dist
    );
}

// ---------- 分段对齐 ----------

/// 从数据库读一条录音的转写分段（只读打开）
fn load_segments(data_root: &Path, recording_id: i64) -> Result<Vec<(i64, i64)>, String> {
    let db = data_root.join("soundtrace.db");
    let conn = rusqlite::Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("只读打开 {} 失败: {e}", db.display()))?;
    let mut stmt = conn
        .prepare("SELECT start_ms, end_ms FROM segments WHERE recording_id = ?1 ORDER BY start_ms")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([recording_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 量化「按重叠多数给分段指派说话人」这条规则的损失
fn align_report(diar: &[DiarSeg], sum: &Summary, segments: &[(i64, i64)]) {
    if segments.is_empty() {
        println!("该录音没有转写分段，无法对齐");
        return;
    }
    println!("转写分段数: {}", segments.len());
    println!(
        "分离说话人: {} 人（{}）",
        sum.num_speakers,
        sum.per_speaker
            .iter()
            .map(|(s, sh, _, _)| format!("spk_{s}:{:.0}%", sh * 100.0))
            .collect::<Vec<_>>()
            .join(" ")
    );

    let mut shares: Vec<f64> = Vec::new();
    let mut no_cover = 0usize;
    for (start_ms, end_ms) in segments {
        let dur = (*end_ms - *start_ms) as f64 / 1000.0;
        if dur <= 0.0 {
            continue;
        }
        let mut by_spk: BTreeMap<i32, f64> = BTreeMap::new();
        for d in diar {
            let a = (*start_ms as f64 / 1000.0).max(d.start);
            let b = (*end_ms as f64 / 1000.0).min(d.end);
            if b > a {
                *by_spk.entry(d.speaker).or_insert(0.0) += b - a;
            }
        }
        let best = by_spk.values().cloned().fold(0.0f64, f64::max);
        if best <= 0.0 {
            no_cover += 1;
            shares.push(0.0);
        } else {
            shares.push(best / dur);
        }
    }

    let n = shares.len().max(1);
    let below = |t: f64| shares.iter().filter(|x| **x < t).count();
    let pure = shares.iter().filter(|x| **x >= 0.999).count();
    println!("\n每条转写分段里「主导说话人」占比（1.0 = 该段完全属于一个人）：");
    println!("  平均          {:.3}", shares.iter().sum::<f64>() / n as f64);
    println!("  中位数        {:.3}", median(shares.clone()));
    println!("  完全纯净      {pure} / {n} = {:.0}%", pure as f64 / n as f64 * 100.0);
    println!(
        "  占比 < 0.9    {} / {n} = {:.0}%   ← 取多数会丢信息",
        below(0.9),
        below(0.9) as f64 / n as f64 * 100.0
    );
    println!(
        "  占比 < 0.7    {} / {n} = {:.0}%   ← 明显跨两个人",
        below(0.7),
        below(0.7) as f64 / n as f64 * 100.0
    );
    println!(
        "  占比 < 0.5    {} / {n} = {:.0}%   ← 多数规则已失效",
        below(0.5),
        below(0.5) as f64 / n as f64 * 100.0
    );
    println!(
        "  无分离覆盖    {no_cover} / {n} = {:.0}%   ← 分离未标出语音（静音/音乐/漏检）",
        no_cover as f64 / n as f64 * 100.0
    );
}

// ---------- 工具 ----------

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        v[n / 2 - 1] / 2.0 + v[n / 2] / 2.0
    }
}

fn decode_16k_mono(src: &Path, dst: &Path) -> Result<(), String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(src)
        .args(["-ac", "1", "-ar", "16000", "-acodec", "pcm_s16le"])
        .arg(dst)
        .output()
        .map_err(|e| format!("调用 ffmpeg 失败: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

/// 与 paths.rs 一致：优先读 bootstrap.json 里记住的数据根目录
fn resolve_data_root() -> PathBuf {
    if let Some(cfg) = dirs::config_dir() {
        let b = cfg.join("com.soundtrace.desktop").join("bootstrap.json");
        if let Ok(raw) = std::fs::read_to_string(&b) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(s) = v.get("data_root").and_then(|x| x.as_str()) {
                    if !s.is_empty() {
                        return PathBuf::from(s);
                    }
                }
            }
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SoundTrace")
}
