//! 子进程创建辅助。
//!
//! Windows 上从「没有控制台的父进程」spawn 控制台程序（node / ffmpeg / aria2c / yt-dlp）
//! 时，系统会为每个子进程新建一个控制台窗口，表现就是屏幕上不停闪黑框。本项目会频繁
//! spawn 这类工具（漫画加密页甚至是「一页一次 sidecar」），所以统一用这里的工厂函数创建
//! Command，把 CREATE_NO_WINDOW 打开。

use std::ffi::OsStr;

/// CREATE_NO_WINDOW：不为子进程创建控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

/// `tokio::process::Command::new` 的替代品：Windows 下顺带隐藏子进程控制台窗口。
pub fn tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// `std::process::Command::new` 的替代品，行为同上。
pub fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
