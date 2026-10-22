//! 回归测试：导入必须在「小栈线程」上也能跑通。
//!
//! 背景（2026-09-18 定位）：`hash_file` 曾在**栈上**放 1 MiB 缓冲，而 `import_files`
//! 当时是同步 `#[tauri::command]`（Tauri 把同步命令放在**主线程**执行），Windows
//! 主线程栈正好 1 MiB（PE SizeOfStackReserve）→ 一导入就
//! `thread 'main' has overflowed its stack`，应用卡死/退出，且一个文件都写不进去。
//!
//! 这里把导入放进 1 MiB 栈的线程里跑，等价于主线程的栈预算：
//! 谁再往栈上放大缓冲，这个测试就会直接把测试进程带走。

use soundtrace_lib::db::Db;
use soundtrace_lib::library::Importer;
use std::path::Path;
use std::time::Instant;

/// 生成一个 2 秒的 16kHz 单声道 PCM WAV
fn write_wav(path: &Path, seconds: usize) {
    let sample_rate: u32 = 16000;
    let n = sample_rate as usize * seconds;
    let mut data = Vec::with_capacity(n * 2);
    for i in 0..n {
        // 方波，保证不是纯静音
        let v: i16 = if (i / 80) % 2 == 0 { 8000 } else { -8000 };
        data.extend_from_slice(&v.to_le_bytes());
    }
    let data_len = data.len() as u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(path, wav).unwrap();
}

#[test]
fn import_runs_on_one_mib_stack() {
    let root = std::env::temp_dir().join("soundtrace-small-stack");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let db = Db::open(&root).unwrap();

    // 大文件也要覆盖：哈希是流式的，缓冲多大都不该动栈
    let big = root.join("big.wav");
    write_wav(&big, 60); // 约 1.9 MB
    let paths = vec![
        root.join("small.wav").to_string_lossy().to_string(),
        big.to_string_lossy().to_string(),
    ];
    write_wav(Path::new(&paths[0]), 2);
    let library_root = root.join("library");

    let handle = std::thread::Builder::new()
        .name("fake-main-1mib".into())
        .stack_size(1024 * 1024) // 与 Windows 主线程默认栈一致
        .spawn(move || {
            let importer = Importer {
                db: &db,
                library_root,
            };
            let t0 = Instant::now();
            let first = importer.import(&paths);
            let first_took = t0.elapsed();
            let t1 = Instant::now();
            let again = importer.import(&paths);
            (first, first_took, again, t1.elapsed())
        })
        .expect("创建小栈线程失败");

    let (first, first_took, again, again_took) =
        handle.join().expect("导入线程崩溃（栈溢出？）");
    let first = first.expect("导入失败");
    assert_eq!(
        first.imported.len(),
        2,
        "应导入 2 条，跳过项：{:?}",
        first.skipped
    );
    for r in &first.imported {
        assert!(r.size_bytes > 0, "归档文件大小为 0：{}", r.title);
        assert!(Path::new(&r.file_path).is_file(), "归档文件不存在");
    }
    println!("1 MiB 栈线程导入成功：{} 条，耗时 {first_took:?}", first.imported.len());

    let again = again.expect("二次导入失败");
    assert_eq!(again.imported.len(), 0, "二次导入不应新增");
    assert_eq!(again.skipped.len(), 2, "二次导入应全部按哈希去重跳过");
    println!("二次导入去重 OK，耗时 {again_took:?}");
}
