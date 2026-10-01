//! 健康遮罩焦点锚点:遮罩打开前记录用户正在使用的 app,全部遮罩最终关闭后把前台焦点还给该 app。
//!
//! Business Logic: 全屏健康遮罩弹出时用户往往正在其它 app（如 IDE）工作；在遮罩上点击
//!     按钮（确认/推迟/跳过）会激活 cc-partner 进程，macOS 会把该进程所有可见窗口
//!     （含主窗口）带到前台；遮罩关闭后主窗口持有焦点，用户被切到 cc-partner 界面，
//!     回不到原来的 app。焦点锚点在遮罩打开前捕获 frontmost app（排除本进程），在
//!     全部遮罩最终关闭时若本进程仍在前台则重新激活锚点 app；用户已在遮罩展示期间
//!     手动切走（cmd-tab）则丢弃锚点不动作，绝不抢夺用户自发的焦点选择。
//!
//! Code Logic: 纯逻辑（`merge_anchor`/`should_restore`）跨平台可单测；macOS FFI 走
//!     objc2-app-kit 的 `NSWorkspace.frontmostApplication` 与
//!     `NSRunningApplication.activateWithOptions`；非 macOS 提供 no-op 版本。
//!     两个 FFI 函数都要求调用方已在主线程（AppKit 主线程约束由调用方保证，函数内
//!     不再派发），mod.rs 侧只在 `run_on_main_thread` 闭包内调用它们。

/// 焦点锚点:遮罩打开前的前台 app 快照。
///
/// `pid` 是恢复焦点的唯一依据（`runningApplicationWithProcessIdentifier`）；
/// `bundle_id`/`name` 仅用于日志定位,不参与恢复逻辑。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OverlayFocusAnchor {
    /// 锚点 app 的进程标识(macOS pid);capture 阶段已排除本进程。
    pub pid: i32,
    /// 锚点 app 的 bundle 标识(可能为 nil,仅日志用)。
    pub bundle_id: Option<String>,
    /// 锚点 app 的本地化名称(可能为 nil,仅日志用)。
    pub name: Option<String>,
}

#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};

/// 合并捕获到的焦点锚点到已有锚点(仅 existing 为 None 时写入)。
///
/// Business Logic: 遮罩队列推进会连续打开多个遮罩窗口,必须复用第一次记录的锚点;
///     用户点击遮罩后 frontmost 已变成本进程,此时再 capture 会得到 None(或不属于
///     用户原 app 的结果),绝不能把已有锚点清掉或覆盖,否则遮罩关闭后无从恢复。
/// Code Logic: `existing` 为 None 才写入 `captured`;已有值时保留 existing 并 drop
///     captured(包括 captured 为 None 的情况,即"捕获不到"不等于"清空")。
pub(crate) fn merge_anchor(
    existing: &mut Option<OverlayFocusAnchor>,
    captured: Option<OverlayFocusAnchor>,
) {
    if existing.is_none() {
        *existing = captured;
    }
}

/// 判断遮罩关闭后是否应恢复焦点锚点 app。
///
/// Business Logic: 仅当本进程仍在前台(用户没有在遮罩展示期间手动 cmd-tab 切走)时
///     才把焦点还给锚点 app;用户已自发切走时绝不能把前台抢回旧 app。
/// Code Logic: `current_frontmost_pid == Some(our_pid)` 才返回 true;frontmost 查询
///     失败(None)或已是其它 app 都返回 false。`anchor_pid` 仅用于 debug 自检
///     (锚点在 capture 阶段已排除本进程,正常不可能等于 our_pid)。
pub(crate) fn should_restore(
    anchor_pid: i32,
    current_frontmost_pid: Option<i32>,
    our_pid: i32,
) -> bool {
    debug_assert_ne!(
        anchor_pid, our_pid,
        "焦点锚点不应指向本进程(capture 阶段已排除)"
    );
    current_frontmost_pid == Some(our_pid)
}

/// 捕获当前前台 app(排除本进程)为焦点锚点。
///
/// Business Logic: 遮罩打开前记录用户正在使用的 app,供遮罩最终关闭后恢复焦点。
/// Code Logic: `NSWorkspace.sharedWorkspace.frontmostApplication` 取前台 app;
///     pid 等于本进程时返回 None(遮罩队列推进时 frontmost 可能已是本进程,此时无锚点
///     可记,merge_anchor 会保留已有锚点);其余取 pid / bundleIdentifier / localizedName。
///     线程约束:**调用方必须已在主线程**(AppKit 约束),函数内不再派发。
#[cfg(target_os = "macos")]
pub(crate) fn capture_frontmost_excluding_current_process() -> Option<OverlayFocusAnchor> {
    let workspace = NSWorkspace::sharedWorkspace();
    let frontmost = workspace.frontmostApplication()?;
    let pid = frontmost.processIdentifier();
    // SAFETY: getpid 是无副作用的系统调用封装,任意线程可安全调用。
    if pid == unsafe { libc::getpid() } {
        return None;
    }
    let bundle_id = frontmost.bundleIdentifier().map(|s| s.to_string());
    let name = frontmost.localizedName().map(|s| s.to_string());
    Some(OverlayFocusAnchor {
        pid,
        bundle_id,
        name,
    })
}

/// 把前台焦点恢复给锚点 app(仅当本进程仍在前台)。
///
/// Business Logic: 全部遮罩关闭后,若用户仍停在 cc-partner(因点击遮罩按钮被带前台),
///     需要把前台还给锚点 app;用户已手动切走则不动作。
/// Code Logic: 按 pid 查 `NSRunningApplication`(查不到说明锚点 app 已退出,直接跳过);
///     重查 frontmost 并经 `should_restore` 判断(传 our_pid),通过才
///     `activateWithOptions(ActivateIgnoringOtherApps)`。线程约束:**调用方必须已在
///     主线程**。锚点由调用方 take 后传入,无论是否恢复成功均不再保留(一次性消费)。
#[cfg(target_os = "macos")]
pub(crate) fn restore_focus_to_anchor(anchor: &OverlayFocusAnchor) {
    let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(anchor.pid)
    else {
        tracing::debug!(
            pid = anchor.pid,
            name = ?anchor.name,
            "焦点锚点 app 已退出,跳过恢复"
        );
        return;
    };
    // SAFETY: getpid 是无副作用的系统调用封装,任意线程可安全调用。
    let our_pid = unsafe { libc::getpid() };
    let frontmost_pid = NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|a| a.processIdentifier());
    if !should_restore(anchor.pid, frontmost_pid, our_pid) {
        tracing::info!(
            pid = anchor.pid,
            name = ?anchor.name,
            frontmost = ?frontmost_pid,
            "用户已自行切换前台 app,丢弃焦点锚点不恢复"
        );
        return;
    }
    // macOS 14 起 `NSApplicationActivateIgnoringOtherApps` 标记 deprecated 且无实际效果
    // (系统改为协作激活),但 macOS <14 上它是本进程持有焦点时把激活权交给目标 app 的
    // 必要选项;objc2-app-kit 0.3.2 未提供 macOS 14+ 的无参 `activate()` selector,
    // `activateWithOptions:` 全版本可用,故统一走它并压制该常量的 deprecation 警告。
    #[allow(deprecated)]
    let activated =
        app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
    tracing::info!(
        pid = anchor.pid,
        name = ?anchor.name,
        activated,
        "焦点锚点恢复请求已发送"
    );
}

/// 非 macOS 平台:恒返回 None(capture 不可用,merge_anchor 保留 None 即可)。
#[cfg(not(target_os = "macos"))]
pub(crate) fn capture_frontmost_excluding_current_process() -> Option<OverlayFocusAnchor> {
    None
}

/// 非 macOS 平台:no-op(焦点恢复仅 macOS 实现)。
#[cfg(not(target_os = "macos"))]
pub(crate) fn restore_focus_to_anchor(_anchor: &OverlayFocusAnchor) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锚点样例构造(纯数据,便于断言相等)。
    fn anchor(pid: i32) -> OverlayFocusAnchor {
        OverlayFocusAnchor {
            pid,
            bundle_id: Some(format!("com.example.app{pid}")),
            name: Some(format!("App {pid}")),
        }
    }

    /// merge_anchor:existing 为 None 时写入 captured。
    #[test]
    fn merge_writes_when_existing_none() {
        let mut existing = None;
        merge_anchor(&mut existing, Some(anchor(100)));
        assert_eq!(existing, Some(anchor(100)));
    }

    /// merge_anchor:已有锚点时保留 existing 并 drop captured(不得覆盖)。
    #[test]
    fn merge_keeps_existing_and_drops_captured() {
        let mut existing = Some(anchor(100));
        merge_anchor(&mut existing, Some(anchor(200)));
        assert_eq!(existing, Some(anchor(100)), "已有锚点不得被新捕获覆盖");
    }

    /// merge_anchor:captured 为 None(捕获不到)时不清空已有锚点。
    #[test]
    fn merge_keeps_existing_when_captured_none() {
        let mut existing = Some(anchor(100));
        merge_anchor(&mut existing, None);
        assert_eq!(existing, Some(anchor(100)), "捕获失败不得清空已有锚点");
    }

    /// should_restore:前台是其它 app(用户已切走)→ false。
    #[test]
    fn should_not_restore_when_other_app_frontmost() {
        assert!(!should_restore(100, Some(999), 42));
    }

    /// should_restore:前台是本进程 → true。
    #[test]
    fn should_restore_when_our_process_frontmost() {
        assert!(should_restore(100, Some(42), 42));
    }

    /// should_restore:frontmost 查询失败(None)→ false(fail-closed 不动作)。
    #[test]
    fn should_not_restore_when_frontmost_unknown() {
        assert!(!should_restore(100, None, 42));
    }
}

/// macOS 真环境 smoke(默认 ignore:CI/无 GUI 环境不跑;本地
/// `cargo test health::focus_anchor -- --ignored --nocapture` 验证)。
#[cfg(all(test, target_os = "macos"))]
mod macos_smoke_tests {
    use super::*;

    /// 真环境 capture + restore 不 panic 且 pid 语义自洽。
    ///
    /// Business Logic: FFI 路径(真实 NSWorkspace/NSRunningApplication)只有真环境能验证;
    ///     无 GUI 的 CI 上这些调用可能返回 None,不应失败。
    /// Code Logic: capture 只断言「不 panic 且 Some 时 pid > 0 且非本进程」;restore 用
    ///     必然不存在的 pid(i32::MAX)调用,断言查不到 app 时 no-op 不 panic。
    #[test]
    #[ignore = "需要 macOS 真实 GUI 环境(NSWorkspace);CI/无 GUI 跳过"]
    fn capture_and_restore_smoke() {
        let captured = capture_frontmost_excluding_current_process();
        if let Some(anchor) = &captured {
            assert!(anchor.pid > 0, "前台 app pid 应为正: {anchor:?}");
            assert_ne!(
                anchor.pid,
                // SAFETY: getpid 是无副作用的系统调用封装,任意线程可安全调用。
                unsafe { libc::getpid() },
                "capture 必须排除本进程"
            );
        }
        // i32::MAX 几乎不可能是真实 pid → runningApplicationWithProcessIdentifier 返回
        // nil → no-op;整条 restore 路径(含 should_restore 短路)不应 panic。
        let ghost = OverlayFocusAnchor {
            pid: i32::MAX,
            bundle_id: None,
            name: None,
        };
        restore_focus_to_anchor(&ghost);
        let _ = captured; // Some/None 均合法(取决于测试时机前台是哪个 app)
    }
}
