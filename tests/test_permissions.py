# -*- coding: utf-8 -*-
"""ui.permissions 模块单元测试。"""
from __future__ import annotations

import sys
import types

from claude_partner.ui import permissions


def test_check_non_darwin_returns_true(monkeypatch):
    """非 macOS 环境 check 函数视为已授权。"""
    monkeypatch.setattr(sys, "platform", "linux")
    assert permissions.check_screen_capture_access() is True
    assert permissions.check_input_monitoring_access() is True


def test_request_non_darwin_returns_false(monkeypatch):
    """非 macOS 环境 request 直接返回 False。"""
    monkeypatch.setattr(sys, "platform", "linux")
    assert permissions.request_screen_capture_access() is False


def test_request_darwin_calls_cgrequest(monkeypatch):
    """macOS 下调用 Quartz.CGRequestScreenCaptureAccess。"""
    monkeypatch.setattr(sys, "platform", "darwin")
    fake_quartz = types.SimpleNamespace(
        CGRequestScreenCaptureAccess=lambda: True,
    )
    monkeypatch.setitem(sys.modules, "Quartz", fake_quartz)
    assert permissions.request_screen_capture_access() is True


def test_request_darwin_without_quartz_returns_false(monkeypatch):
    """macOS 但 Quartz 不可用时安全返回 False。"""
    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setitem(sys.modules, "Quartz", None)
    assert permissions.request_screen_capture_access() is False


def test_open_settings_non_darwin_returns_false(monkeypatch):
    monkeypatch.setattr(sys, "platform", "linux")
    assert permissions.open_permission_settings("screenCapture") is False


def test_open_settings_unknown_type_returns_false(monkeypatch):
    monkeypatch.setattr(sys, "platform", "darwin")
    assert permissions.open_permission_settings("unknown") is False


def test_open_settings_darwin_calls_subprocess(monkeypatch):
    """macOS 下对已知类型调用 subprocess.Popen(['open', url])。"""
    monkeypatch.setattr(sys, "platform", "darwin")
    captured: dict = {}

    class FakePopen:
        def __init__(self, cmd):
            captured["cmd"] = cmd

    monkeypatch.setattr(permissions.subprocess, "Popen", FakePopen)
    assert permissions.open_permission_settings("screenCapture") is True
    assert captured["cmd"] == [
        "open",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
    ]

    assert permissions.open_permission_settings("inputMonitoring") is True
    assert "Privacy_ListenEvent" in captured["cmd"][1]
