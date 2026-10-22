//! 子进程优先级：让媒体处理（转写、解码、波形）以**低于前台**的优先级运行。
//!
//! 为什么需要：转写是纯 CPU 计算，按核心数开线程后会把机器吃满。若与前台应用同优先级，
//! 一边转写一边做别的事就会明显卡顿。Windows 的调度是优先级抢占式的，把这类批处理子进程
//! 降到「低于正常」，机器空闲时它照常拿满 CPU（**速度零损失**），一旦前台程序需要 CPU 就
//! 立刻让路——所以这个策略固定常开，不需要开关。
//!
//! 另一层收益来自 CreateProcess 的继承规则：未指定优先级类时，子进程继承创建者的优先级类。
//! 所以给 `st-transcribe.exe` 打上标记之后，它在内部调用的 ffmpeg 解码进程会自动跟着降下来，
//! 一处生效即覆盖整条链路。

use std::process::Command;

/// 「低于正常」优先级类（任务管理器里显示为「低于正常」）。
/// 它既是 `SetPriorityClass` 的取值，也是 `CreateProcess` 合法的 `dwCreationFlags`，
/// 因此能在 spawn 时一次设定，不必再取进程句柄。
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// 让 `cmd` 启动的子进程以低于前台的优先级运行。
///
/// 非 Windows 平台不做处理：本项目只在 Windows 发布，其他平台仅用于开发与测试。
pub fn background(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // creation_flags 与标准库自己的 CREATE_UNICODE_ENVIRONMENT 相或，不会互相覆盖
        cmd.creation_flags(BELOW_NORMAL_PRIORITY_CLASS);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端验证标志真的生效：让子进程自报优先级类。
    /// 这条测试覆盖的是 Windows 是否会接受 `dwCreationFlags` 里的优先级位——若被忽略，
    /// 子进程会报 `Normal`，测试就红了。
    #[test]
    #[cfg(windows)]
    fn child_reports_below_normal() {
        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-Command",
            "(Get-Process -Id $PID).PriorityClass",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
        background(&mut cmd);

        let out = cmd.output().expect("无法启动 powershell");
        let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(got, "BelowNormal", "子进程优先级类应为 BelowNormal");
    }
}
