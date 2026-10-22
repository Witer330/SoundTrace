//! CPU 拓扑与转写线程数的取值策略。
//!
//! 为什么需要单独一个模块：`sherpa-onnx` 曾经按「全部逻辑核心」开线程，实测在带 HT / 大小核的
//! CPU 上**反而更慢**——多出来的线程落在线程超售的超线程兄弟与 E 核上，互相抢执行单元。
//! 实测曲线（i5-14600KF，6P+8E，20 逻辑核心，10 分钟真实会议音频，int8 Paraformer）：
//!
//! | 线程数 | 6 | 10 | 12 | 14 | 16 | 18 | 20 |
//! |---|---|---|---|---|---|---|---|
//! | RTF | 0.026 | 0.026 | 0.026 | 0.028 | 0.032 | 0.046 | 0.056 |
//!
//! 平坦区正好停在物理核心数（14），越过它就开始变慢：20 线程比 14 线程慢一倍以上。
//! 所以策略是「按物理核心数开线程」——既最快，又天然留下逻辑核心给前台程序。

/// 自动模式至少留给系统的逻辑核心数。即使物理核心数很少，也不能让转写独占整机。
const AUTO_RESERVED_CORES: usize = 2;

/// 逻辑核心数（含超线程）。拿不到时按 4 估。
pub fn logical_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// 物理核心数（不重复计超线程兄弟）。拿不到时返回 `None`，由调用方回退。
#[cfg(windows)]
pub fn physical_cores() -> Option<usize> {
    // 手写声明而不是引入 windows-sys：只需要这一个导出，且它的 ABI 自 Vista 起未变。
    // 用的是 `SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX` 的**文档化布局**：
    //   offset 0: Relationship (u32)
    //   offset 4: Size (u32) —— 每条记录的字节长度，遍历就靠它步进
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLogicalProcessorInformationEx(
            relationship: u32,
            buffer: *mut u8,
            returned_length: *mut u32,
        ) -> i32;
    }
    /// `LOGICAL_PROCESSOR_RELATIONSHIP::RelationProcessorCore`
    const RELATION_PROCESSOR_CORE: u32 = 0;

    unsafe {
        // 第一次调用只为问出所需缓冲区大小（buffer 传空必然失败，但会填长度）
        let mut len: u32 = 0;
        GetLogicalProcessorInformationEx(RELATION_PROCESSOR_CORE, std::ptr::null_mut(), &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if GetLogicalProcessorInformationEx(RELATION_PROCESSOR_CORE, buf.as_mut_ptr(), &mut len) == 0 {
            return None;
        }

        // 每条记录对应一个物理核心，按各自 Size 步进
        let mut cores = 0usize;
        let mut off = 0usize;
        while off + 8 <= len as usize {
            let size = u32::from_le_bytes(buf[off + 4..off + 8].try_into().ok()?) as usize;
            if size < 8 || off + size > len as usize {
                break; // 布局与预期不符：宁可放弃，也不要数出错误的核数
            }
            cores += 1;
            off += size;
        }
        (cores > 0).then_some(cores)
    }
}

#[cfg(not(windows))]
pub fn physical_cores() -> Option<usize> {
    None
}

/// 自动模式的线程数：物理核心数，但至少留 `AUTO_RESERVED_CORES` 个逻辑核心给系统。
///
/// 上界用「逻辑核心数 - 保留量」，是为了在没有超线程或拿不到物理核心数时也不独占整机；
/// 下界保证至少 1 个线程，避免把 0 传给识别器。
pub fn auto_threads_with(logical: usize, physical: Option<usize>) -> usize {
    let logical = logical.max(1);
    let ceiling = logical.saturating_sub(AUTO_RESERVED_CORES).max(1);
    physical.unwrap_or(logical).min(ceiling).max(1)
}

/// 自动模式的线程数（按本机实际拓扑）
pub fn auto_threads() -> usize {
    auto_threads_with(logical_cores(), physical_cores())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自动取值：物理核心数封顶，且至少留 2 个逻辑核心。
    #[test]
    fn auto_threads_respects_topology_and_reserve() {
        // 有超线程：取物理核心数（本例即平滑区），而不是逻辑核心数
        assert_eq!(auto_threads_with(20, Some(14)), 14);
        assert_eq!(auto_threads_with(16, Some(8)), 8);
        // 物理核心数 > 逻辑核心数 - 2 时，保留量优先（不独占整机）
        assert_eq!(auto_threads_with(8, Some(8)), 6);
        assert_eq!(auto_threads_with(6, Some(6)), 4);
        // 拿不到物理核心数：退回「逻辑核心 - 2」
        assert_eq!(auto_threads_with(20, None), 18);
        // 极端小机器：无论如何 ≥ 1
        assert_eq!(auto_threads_with(1, Some(1)), 1);
        assert_eq!(auto_threads_with(2, Some(2)), 1);
    }

    /// 探测结果必须自洽：1 ≤ 物理核心数 ≤ 逻辑核心数，且自动取值落在合法区间。
    #[test]
    fn detected_topology_is_sane() {
        let logical = logical_cores();
        let physical = physical_cores();
        if let Some(p) = physical {
            assert!(p >= 1 && p <= logical, "物理 {p} 应落在 [1, 逻辑 {logical}]");
        }
        let auto = auto_threads();
        assert!(auto >= 1 && auto <= logical, "自动取到 {auto}，逻辑 {logical}");
        println!("本机拓扑：逻辑 {logical}，物理 {physical:?}，自动线程数 {auto}");
    }
}
