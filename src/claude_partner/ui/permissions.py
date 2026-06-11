# -*- coding: utf-8 -*-
"""macOS 权限状态检查：供后端 API 端点查询当前权限授权情况。

Business Logic（为什么需要这个模块）:
    前端 React 设置页面需要展示 macOS 权限状态（屏幕录制、输入监控），
    让用户了解截图、全局快捷键等功能是否可用。后端通过 /api/permissions
    端点返回检查结果，本模块提供底层的权限探测实现。

Code Logic（这个模块做什么）:
    提供 check_screen_capture_access / check_input_monitoring_access 两个函数，
    分别检测屏幕录制权限和输入监控权限。仅在 macOS 打包环境中真实检测，
    其他环境直接视为已授权。
"""

from __future__ import annotations

import subprocess
import sys


def check_screen_capture_access() -> bool:
    """
    Business Logic:
        截图功能需要屏幕录制权限，需检查当前是否已授权。

    Code Logic:
        使用 Quartz.CGPreflightScreenCaptureAccess() 检查。
        非 macOS 或非 frozen 环境直接返回 True。
    """
    if sys.platform != "darwin" or not getattr(sys, "frozen", False):
        return True
    try:
        import Quartz  # type: ignore[import-untyped]
        if hasattr(Quartz, "CGPreflightScreenCaptureAccess"):
            return bool(Quartz.CGPreflightScreenCaptureAccess())  # type: ignore[attr-defined]
    except ImportError:
        pass
    return True


def check_input_monitoring_access() -> bool:
    """
    Business Logic:
        全局快捷键功能需要输入监控权限，需检查当前是否已授权。

    Code Logic:
        尝试创建 CGEventTap，返回 None 表示缺少权限。
        非 macOS 或非 frozen 环境直接返回 True。
    """
    if sys.platform != "darwin" or not getattr(sys, "frozen", False):
        return True
    try:
        import Quartz  # type: ignore[import-untyped]

        def _dummy(_proxy: object, _etype: int, event: object, _ref: object) -> object:
            return event

        tap = Quartz.CGEventTapCreate(  # type: ignore[attr-defined]
            Quartz.kCGHIDEventTap,  # type: ignore[attr-defined]
            Quartz.kCGHeadInsertEventTap,  # type: ignore[attr-defined]
            Quartz.kCGEventTapOptionListenOnly,  # type: ignore[attr-defined]
            Quartz.CGEventMaskBit(Quartz.kCGEventKeyDown),  # type: ignore[attr-defined]
            _dummy,
            None,
        )
        if tap is None:
            return False
        Quartz.CFMachPortInvalidate(tap)  # type: ignore[attr-defined]
        return True
    except ImportError:
        pass
    return True


# 权限类型 → macOS「系统设置 → 隐私与安全」对应面板的 URL scheme
_PERMISSION_SETTINGS_URLS: dict[str, str] = {
    "screenCapture": (
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
    ),
    "inputMonitoring": (
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
    ),
}


def request_screen_capture_access() -> bool:
    """
    Business Logic（为什么需要这个函数）:
        屏幕录制权限需要主动请求以触发系统授权弹窗（首次使用时），
        供前端「去设置/请求授权」流程调用，避免用户不知道要去哪里开启。

    Code Logic（这个函数做什么）:
        调用 Quartz.CGRequestScreenCaptureAccess()（macOS 10.15+）。
        非 macOS 返回 False；Quartz 不可用或无该 API 时返回 False。
        注意：该 API 仅在「未决定」状态下弹系统对话框，已被用户拒绝时
        直接返回 False 且不再弹窗，此时需配合 open_permission_settings
        引导用户到设置面板手动开启。
    """
    if sys.platform != "darwin":
        return False
    try:
        import Quartz  # type: ignore[import-untyped]
        if hasattr(Quartz, "CGRequestScreenCaptureAccess"):
            return bool(Quartz.CGRequestScreenCaptureAccess())  # type: ignore[attr-defined]
    except ImportError:
        pass
    return False


def open_permission_settings(perm_type: str) -> bool:
    """
    Business Logic（为什么需要这个函数）:
        用户需要手动在「系统设置 → 隐私与安全」中开启对应权限，
        本函数直接打开对应面板，免去用户手动查找，提升授权转化。

    Code Logic（这个函数做什么）:
        通过 subprocess.Popen 非阻塞调用 `open <url-scheme>` 打开面板。
        仅 macOS 生效；未知 perm_type 或非 macOS 返回 False。
    """
    if sys.platform != "darwin":
        return False
    url: str | None = _PERMISSION_SETTINGS_URLS.get(perm_type)
    if not url:
        return False
    try:
        subprocess.Popen(["open", url])
        return True
    except Exception:
        return False
