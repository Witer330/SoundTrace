use crate::error::{err, AppResult};
use std::fs;
use std::path::{Path, PathBuf};

/// 数据根目录解析。
///
/// 数据库本身存放在数据根目录下，而"数据根目录"这个设置又只能来自数据库之外，
/// 因此用一个极小的 bootstrap.json（固定在系统配置目录）来记住用户选择的数据根目录。
/// 缺省为 ~/SoundTrace。
#[derive(serde::Deserialize, Default)]
struct Bootstrap {
    #[serde(default)]
    data_root: Option<String>,
}

pub fn bootstrap_path() -> AppResult<PathBuf> {
    let config = dirs::config_dir().ok_or_else(|| err("无法定位系统配置目录"))?;
    Ok(config.join("com.soundtrace.desktop").join("bootstrap.json"))
}

pub fn resolve_data_root() -> AppResult<PathBuf> {
    let path = bootstrap_path()?;
    let root = if path.exists() {
        let raw = fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str::<Bootstrap>(&raw)
            .ok()
            .and_then(|b| b.data_root)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    } else {
        None
    }
    .unwrap_or_else(default_root);

    if !root.exists() {
        fs::create_dir_all(&root).map_err(|e| err(format!("创建数据目录失败 {}: {e}", root.display())))?;
    }
    Ok(root)
}

pub fn set_data_root(new_root: &Path) -> AppResult<()> {
    let path = bootstrap_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::json!({ "data_root": new_root.to_string_lossy() });
    fs::write(&path, serde_json::to_string_pretty(&raw)?)?;
    Ok(())
}

pub fn default_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SoundTrace")
}

/// 在数据根目录下建立子目录结构：library / models / cache
pub fn ensure_dirs(root: &Path) -> AppResult<()> {
    for sub in ["library", "models", "cache"] {
        let dir = root.join(sub);
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
        }
    }
    Ok(())
}

pub fn library_dir(root: &Path) -> PathBuf {
    root.join("library")
}

pub fn models_dir(root: &Path) -> PathBuf {
    root.join("models")
}

pub fn cache_dir(root: &Path) -> PathBuf {
    root.join("cache")
}
