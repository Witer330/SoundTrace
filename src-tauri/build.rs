fn main() {
    ensure_sidecar_placeholder();
    tauri_build::build()
}

/// 保证 `binaries/` 下的子进程引擎占位文件存在，否则 Tauri 的构建脚本会因为
/// `bundle.resources` 指向不存在的文件而直接失败——那会让全新克隆连 `cargo test` 都跑不起来。
///
/// 这里只放占位文件：**开发时用不到它们**（主程序在 target/<profile>/ 下按同级查找到真正
/// 由 cargo 生成的 bin），只有打包才需要真货，而打包前会先由 `tools/build-sidecar.mjs`
/// 把它们覆盖成真正的引擎（见 tauri.conf.json 的 beforeBuildCommand）。
fn ensure_sidecar_placeholder() {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = manifest_dir.join("binaries");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        println!("cargo:warning=创建 binaries 目录失败: {e}");
        return;
    }
    for name in ["st-transcribe.exe", "st-diarize.exe"] {
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        if let Err(e) = std::fs::write(
            &path,
            b"placeholder: run `node tools/build-sidecar.mjs` before packaging\n",
        ) {
            println!("cargo:warning=写入占位文件失败: {e}");
            return;
        }
        println!("cargo:warning=已放置 {name} 的占位文件，打包前请运行 node tools/build-sidecar.mjs 生成真正的引擎");
    }
}
