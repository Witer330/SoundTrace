// 将 brand/icon.svg 渲染为 1024x1024 PNG 母图，供 `pnpm tauri icon` 生成全套图标。
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../brand");
    let svg = std::fs::read_to_string(root.join("icon.svg"))?;

    let opts = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(&svg, &opts)?;

    let size = 1024u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)
        .ok_or("failed to create pixmap")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(root.join("icon-1024.png"))?;
    println!("written brand/icon-1024.png ({}x{})", size, size);
    Ok(())
}
