//! macos_dock — 独立后端进程脱离 macOS Dock。
//!
//! Business Logic（为什么需要这个模块）:
//!     `cc-partner-backend` 与 GUI 打在同一个 `.app/Contents/MacOS/` 下。
//!     macOS 按 bundle 判断「应用是否在运行」：GUI 退出后只要 sidecar 还活着，
//!     程序坞就会继续显示 cc-partner 在后台运行。
//!
//! Code Logic（这个模块做什么）:
//!     按可执行文件名判断是否为独立后端；是则把当前进程变成 Background helper，
//!     不点亮宿主 .app 的 Dock。非 macOS 为空操作。

use std::path::Path;

/// macOS `TransformProcessType` 的目标角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockProcessRole {
    /// 独立后端 CLI：后台 helper，不得出现在 Dock。
    BackgroundHelper,
    /// 桌面 GUI 退出前：先变成 UIElement，再强退进程。
    HideBeforeQuit,
}

/// Business Logic（为什么需要这个函数）:
///     只有 `cc-partner-backend` 需要脱离 Dock；GUI `cc-partner` 必须保留程序坞图标，
///     cargo test 二进制也绝不能把测试进程变成后台应用。
///
/// Code Logic（这个函数做什么）:
///     比较可执行文件 stem 是否恰好为 `cc-partner-backend`（Windows 去 `.exe`）。
pub fn should_detach_current_process_from_dock(exe: &Path) -> bool {
    exe.file_stem().and_then(|name| name.to_str()) == Some("cc-partner-backend")
}

/// Business Logic（为什么需要这个函数）:
///     sidecar 与 GUI 退出前要用不同的进程变换：helper 走 Background，GUI 走 UIElement。
///
/// Code Logic（这个函数做什么）:
///     返回 Apple `TransformProcessType` 常量：Background=2，UIElement=4。
pub fn process_transform_code(role: DockProcessRole) -> i32 {
    match role {
        // kProcessTransformToBackgroundApplication
        DockProcessRole::BackgroundHelper => 2,
        // kProcessTransformToUIElementApplication
        DockProcessRole::HideBeforeQuit => 4,
    }
}

/// Business Logic（为什么需要这个函数）:
///     backend CLI 入口必须在 dispatch 前调用，避免 start/serve/stop 短进程把宿主 .app 点亮。
///
/// Code Logic（这个函数做什么）:
///     读 current_exe；不该脱离则返回；macOS 上调用 TransformProcessType(Background)。
///     非 macOS 仍走同一判定，真正的进程变换为空操作，避免 Linux clippy 把尾部 `return` 当成 needless_return。
pub fn detach_current_process_from_dock() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if should_detach_current_process_from_dock(&exe) {
        apply_backend_dock_detach();
    }
}

/// Business Logic（为什么需要这个函数）:
///     Dock 变换只在 macOS 有系统 API；其它平台必须保持空操作。
///
/// Code Logic（这个函数做什么）:
///     macOS 调 TransformProcessType(Background)。
#[cfg(target_os = "macos")]
fn apply_backend_dock_detach() {
    let status =
        transform_current_process(process_transform_code(DockProcessRole::BackgroundHelper));
    if status != 0 {
        tracing::warn!("将 backend 进程转为后台 helper 失败: status={status}");
    }
}

/// Business Logic（为什么需要这个函数）:
///     Linux/Windows CI 仍编译本模块；真正的 Dock 变换只存在于 macOS。
///
/// Code Logic（这个函数做什么）:
///     no-op，保证非 macOS 上 `detach_current_process_from_dock` 没有尾部 needless_return。
#[cfg(not(target_os = "macos"))]
fn apply_backend_dock_detach() {
    // TransformProcessType 只在 macOS 存在。
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct ProcessSerialNumber {
    high_long_of_psn: u32,
    low_long_of_psn: u32,
}

#[cfg(target_os = "macos")]
const K_CURRENT_PROCESS: u32 = 2;

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn TransformProcessType(psn: *const ProcessSerialNumber, transform_state: i32) -> i32;
}

/// Business Logic（为什么需要这个函数）:
///     需要把当前进程从宿主 .app 的前台身份里摘出来。
///
/// Code Logic（这个函数做什么）:
///     对 kCurrentProcess 调用 TransformProcessType；0 为成功。
#[cfg(target_os = "macos")]
fn transform_current_process(transform_state: i32) -> i32 {
    let psn = ProcessSerialNumber {
        high_long_of_psn: 0,
        low_long_of_psn: K_CURRENT_PROCESS,
    };
    unsafe { TransformProcessType(&psn, transform_state) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Business Logic（为什么需要这个测试）:
    ///     打进 `.app/Contents/MacOS/` 的 sidecar 正是会点亮程序坞的进程。
    ///
    /// Code Logic（这个测试做什么）:
    ///     正式包与 Dev.app 的 sidecar 路径都必须判定为需要脱离 Dock。
    #[test]
    fn packaged_sidecar_inside_app_bundle_must_detach_from_dock() {
        assert!(should_detach_current_process_from_dock(Path::new(
            "/Applications/cc-partner.app/Contents/MacOS/cc-partner-backend"
        )));
        assert!(should_detach_current_process_from_dock(Path::new(
            "/Users/hans/Applications/cc-partner (Dev).app/Contents/MacOS/cc-partner-backend"
        )));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     开发态 durable 副本不在 .app 内，但链接了 AppKit 的 Mach-O 仍可能单独出现在 Dock。
    ///
    /// Code Logic（这个测试做什么）:
    ///     `~/.cc-partner/runtime/cc-partner-backend` 必须脱离 Dock。
    #[test]
    fn durable_runtime_backend_must_detach_from_dock() {
        assert!(should_detach_current_process_from_dock(Path::new(
            "/Users/hans/.cc-partner/runtime/cc-partner-backend"
        )));
        let mut win_path = std::path::PathBuf::from(r"C:\Users\hans\.cc-partner\runtime");
        win_path.push("cc-partner-backend.exe");
        assert!(should_detach_current_process_from_dock(&win_path));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     GUI 进程必须继续作为普通前台应用出现在程序坞。
    ///
    /// Code Logic（这个测试做什么）:
    ///     `cc-partner` 主二进制不得被判定为 helper。
    #[test]
    fn gui_binary_must_not_detach_from_dock() {
        assert!(!should_detach_current_process_from_dock(Path::new(
            "/Users/hans/Applications/cc-partner (Dev).app/Contents/MacOS/cc-partner"
        )));
        assert!(!should_detach_current_process_from_dock(Path::new(
            "/Applications/cc-partner.app/Contents/MacOS/cc-partner"
        )));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     单测进程若被误判成 helper，TransformProcessType 会把 cargo test 变成后台应用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     当前测试 exe 与典型 deps 二进制都不得脱离 Dock。
    #[test]
    fn cargo_test_binary_must_not_detach_from_dock() {
        assert!(!should_detach_current_process_from_dock(Path::new(
            "/Users/hans/web_project/cc-partner/src-tauri/target/debug/deps/app_lib-abc123"
        )));
        let exe = std::env::current_exe().expect("current_exe");
        assert!(
            !should_detach_current_process_from_dock(&exe),
            "cargo test 当前 exe 不得脱离 Dock: {}",
            exe.display()
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     sidecar 必须用 Background，GUI 退出隐藏必须用 UIElement，不能用错常量。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 Background=2、UIElement=4（Apple TransformProcessType 常量）。
    #[test]
    fn process_transform_codes_match_apple_constants() {
        assert_eq!(process_transform_code(DockProcessRole::BackgroundHelper), 2);
        assert_eq!(process_transform_code(DockProcessRole::HideBeforeQuit), 4);
    }
}
