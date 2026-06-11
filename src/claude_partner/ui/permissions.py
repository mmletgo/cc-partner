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
