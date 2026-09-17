use crate::error::{err, AppResult};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

/// 波形峰值分辨率：每 100ms 一个采样点（3 小时 ≈ 108,000 字节 BLOB）
pub const PEAK_RESOLUTION_MS: usize = 100;

/// 解码音频为 16kHz 单声道 PCM 流，按桶取峰值（0.0~1.0）。
/// 通过管道流式处理，不落盘、内存占用恒定。
pub fn compute_peaks(path: &Path) -> AppResult<Vec<f32>> {
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .arg("-i").arg(path)
        .args(["-f", "s16le", "-ac", "1", "-ar", "16000", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| err(format!("调用 ffmpeg 失败（请确认已安装并在 PATH）: {e}")))?;

    let samples_per_bucket: usize = 16000 * PEAK_RESOLUTION_MS / 1000; // 1600
    let mut stdout = child.stdout.take().expect("ffmpeg stdout");

    let mut peaks: Vec<f32> = Vec::new();
    let mut carry: Vec<i16> = Vec::with_capacity(samples_per_bucket);
    let mut read_buf = vec![0u8; samples_per_bucket * 2 * 16]; // ~51KB

    loop {
        let n = stdout.read(&mut read_buf)?;
        if n == 0 {
            break;
        }
        let usable = n & !1; // 丢弃孤字节
        for pair in read_buf[..usable].chunks_exact(2) {
            carry.push(i16::from_le_bytes([pair[0], pair[1]]));
        }
        while carry.len() >= samples_per_bucket {
            let bucket: Vec<i16> = carry.drain(..samples_per_bucket).collect();
            let max = bucket.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            peaks.push(max as f32 / 32768.0);
        }
    }
    if !carry.is_empty() {
        let max = carry.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        peaks.push(max as f32 / 32768.0);
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(err("ffmpeg 解码失败"));
    }
    Ok(peaks)
}

/// Vec<f32> ↔ LE f32 BLOB（SQLite 存储）
pub fn peaks_to_blob(peaks: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(peaks.len() * 4);
    for p in peaks {
        bytes.extend_from_slice(&p.to_le_bytes());
    }
    bytes
}

pub fn blob_to_peaks(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成指定秒数的 16kHz 单声道 s16 WAV（方波，固定幅度）
    fn write_test_wav(path: &Path, seconds: f64, amplitude: i16) {
        let sample_rate = 16000u32;
        let n = (sample_rate as f64 * seconds) as usize;
        let mut samples = Vec::with_capacity(n);
        for i in 0..n {
            let v = if (i / 80) % 2 == 0 { amplitude } else { -amplitude };
            samples.extend_from_slice(&v.to_le_bytes());
        }
        let data_len = samples.len() as u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // 单声道
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // 字节率
        wav.extend_from_slice(&2u16.to_le_bytes()); // 块对齐
        wav.extend_from_slice(&16u16.to_le_bytes()); // 位深
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(&samples);
        std::fs::write(path, wav).unwrap();
    }

    #[test]
    fn peaks_bucket_count_and_amplitude() {
        let dir = std::env::temp_dir().join("soundtrace-test");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("t.wav");
        write_test_wav(&wav, 1.0, 16000);

        let peaks = compute_peaks(&wav).unwrap();
        // 1 秒 ÷ 100ms = 10 桶
        assert_eq!(peaks.len(), 10, "桶数应为 10，实际 {}", peaks.len());
        let expect = 16000.0 / 32768.0;
        for p in &peaks {
            assert!((p - expect).abs() < 0.01, "峰值 {p} 偏离 {expect}");
        }

        // BLOB 往返
        let blob = peaks_to_blob(&peaks);
        assert_eq!(blob_to_peaks(&blob), peaks);
        std::fs::remove_file(&wav).ok();
    }
}
