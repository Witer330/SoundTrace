//! 验证音频播放所依赖的 asset 协议路径放行规则。
//!
//! 应用在 setup 中调用 `asset_protocol_scope().allow_directory(<data_root>/library, true)`，
//! Tauri 内部（tauri::scope::fs）为每个允许目录生成若干 glob 模式，鉴权时先对请求路径
//! 做 canonicalize 再匹配。Windows 上 canonicalize 会带 `\\?\` 前缀，而 allow_directory
//! 传入的是普通路径 —— 本测试忠实复现 Tauri 两端的规范化过程（escape 只作用于路径部分，
//! 通配符另行拼接），确认归档音频落在放行范围内。若此测试失败，应用内播放会静默失效。

use glob::{MatchOptions, Pattern};
use std::path::{Path, PathBuf};

const MAIN_SEPARATOR: char = std::path::MAIN_SEPARATOR;

/// 复现 tauri::scope::fs::escaped_pattern_with
fn escaped_pattern_with(p: &str, append: &str) -> Pattern {
    let s = if p.ends_with(MAIN_SEPARATOR) {
        format!("{}{append}", glob::Pattern::escape(p))
    } else {
        format!(
            "{}{}{append}",
            glob::Pattern::escape(p),
            MAIN_SEPARATOR
        )
    };
    Pattern::new(&s).unwrap()
}

/// 复现 tauri::scope::fs::canonicalize_parent
fn canonicalize_parent(mut path: PathBuf) -> Option<PathBuf> {
    let mut failed_components: Option<PathBuf> = None;
    loop {
        if let Ok(p) = path.canonicalize() {
            break Some(match failed_components {
                Some(fc) => p.join(fc),
                None => p,
            });
        }
        if let Some(mut last) = path.iter().next_back().map(PathBuf::from) {
            if !path.pop() {
                break None;
            }
            if let Some(fc) = &failed_components {
                last.push(fc);
            }
            failed_components.replace(last);
        } else {
            break None;
        }
    }
}

/// 复现 tauri::scope::fs::push_pattern
fn push_pattern(list: &mut Vec<Pattern>, raw: &str, f: impl Fn(&str) -> Pattern) {
    // Tauri: let path: PathBuf = pattern.as_ref().components().collect();
    let path: PathBuf = Path::new(raw).components().collect();
    let path_str = path.to_string_lossy().to_string();

    list.push(f(&path_str));

    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        let is_verbatim_disk = matches!(
            components.next(),
            Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::VerbatimDisk(..))
        );
        if is_verbatim_disk {
            if let Some(simplified) = path_str.get(4..) {
                if simplified != path_str {
                    list.push(f(simplified));
                }
            }
        }
    }

    if let Some(p) = canonicalize_parent(path) {
        list.push(f(&p.to_string_lossy()));
    }
}

/// 复现 allow_directory(path, recursive=true) 生成的模式集合
fn patterns_for_directory(dir: &Path) -> Vec<Pattern> {
    let mut list = Vec::new();
    let raw = dir.to_string_lossy().to_string();
    push_pattern(&mut list, &raw, |p| {
        Pattern::new(&glob::Pattern::escape(p)).unwrap()
    });
    push_pattern(&mut list, &raw, |p| escaped_pattern_with(p, "**"));
    list
}

/// 复现 Scope::is_allowed
fn is_allowed(patterns: &[Pattern], path: &Path) -> bool {
    let options = MatchOptions {
        require_literal_separator: true,
        require_literal_leading_dot: false,
        case_sensitive: false,
    };
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    patterns
        .iter()
        .any(|p| p.matches_path_with(&resolved, options))
}

fn scenario(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("soundtrace-asset-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    let library = root.join("library");
    std::fs::create_dir_all(&library).unwrap();
    (root, library)
}

#[test]
fn archived_audio_is_allowed_by_asset_scope() {
    let (root, library) = scenario("scope");
    let month = library.join("2026-09");
    std::fs::create_dir_all(&month).unwrap();
    let audio = month.join("会议录音_产品评审_abc123def456.wav");
    std::fs::write(&audio, b"RIFF....WAVE").unwrap();

    let patterns = patterns_for_directory(&library);
    assert!(
        is_allowed(&patterns, &audio),
        "归档音频未被 asset 协议放行（应用内播放会失败）。\n路径: {}\n模式: {:?}",
        audio.display(),
        patterns.iter().map(|p| p.as_str()).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn nested_subdirectories_are_allowed() {
    let (root, library) = scenario("scope-nested");
    let deep = library.join("2026-09").join("sub");
    std::fs::create_dir_all(&deep).unwrap();
    let audio = deep.join("a.m4a");
    std::fs::write(&audio, b"x").unwrap();

    let patterns = patterns_for_directory(&library);
    assert!(is_allowed(&patterns, &audio), "递归放行应覆盖子目录");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn files_outside_library_are_rejected() {
    let (root, library) = scenario("scope-outside");
    let outside = root.join("secret.txt");
    std::fs::write(&outside, b"nope").unwrap();
    // 兄弟目录
    let sibling = root.join("other").join("a.wav");
    std::fs::create_dir_all(sibling.parent().unwrap()).unwrap();
    std::fs::write(&sibling, b"x").unwrap();

    let patterns = patterns_for_directory(&library);
    assert!(!is_allowed(&patterns, &outside), "库目录外的文件不应放行");
    assert!(!is_allowed(&patterns, &sibling), "兄弟目录不应放行");

    let _ = std::fs::remove_dir_all(&root);
}

/// 与 push_pattern 中的 Windows 前缀剥离逻辑一致：带 `\\?\` 前缀的模式也要能匹配
#[test]
fn verbatim_prefixed_patterns_also_match() {
    let (root, library) = scenario("scope-verbatim");
    let audio = library.join("a.wav");
    std::fs::write(&audio, b"x").unwrap();

    let canon = std::fs::canonicalize(&library).unwrap();
    let canon_str = canon.to_string_lossy().to_string();
    let mut patterns = Vec::new();
    push_pattern(&mut patterns, &canon_str, |p| escaped_pattern_with(p, "**"));

    assert!(
        is_allowed(&patterns, &audio),
        "带 verbatim 前缀的模式应能匹配（模式: {:?}）",
        patterns.iter().map(|p| p.as_str()).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&root);
}
