# -*- coding: utf-8 -*-
"""截图管理器模块：协调截图流程，管理覆盖层生命周期。"""

from __future__ import annotations

import logging
import sys

from PyQt6.QtCore import QObject, pyqtSignal
from PyQt6.QtGui import QGuiApplication, QPixmap, QScreen

from claude_partner.screenshot.overlay import ScreenshotOverlay

logger = logging.getLogger(__name__)


class ScreenshotManager(QObject):
    """
    截图功能管理器，协调截图流程。

    Business Logic（为什么需要这个类）:
        截图功能涉及覆盖层的创建、截图完成后复制到剪贴板、
        以及覆盖层的清理，需要一个管理器统一协调这些步骤。

    Code Logic（这个类做什么）:
        创建和管理 ScreenshotOverlay 实例，连接其信号，
        截图完成后自动复制到系统剪贴板并发射 screenshot_ready 信号，
        取消或完成后清理覆盖层资源。
        macOS 多屏时为每个屏幕创建独立覆盖层（因为 macOS 不允许
        单个窗口跨屏显示），其他平台使用单一全屏覆盖层。
    """

    screenshot_ready = pyqtSignal(QPixmap)

    def __init__(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            管理器初始化时不创建覆盖层，等用户触发截图时再创建。

        Code Logic（这个函数做什么）:
            初始化覆盖层列表为空。
        """
        super().__init__()
        self._overlays: list[ScreenshotOverlay] = []

    def take_screenshot(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击截图按钮或使用快捷键时触发区域截图流程。

        Code Logic（这个函数做什么）:
            macOS 上为每个屏幕独立创建覆盖层（macOS 不允许单窗口跨屏）；
            其他平台创建单一覆盖层覆盖虚拟桌面全部区域。
        """
        logger.info("take_screenshot 被调用")
        if sys.platform == "darwin":
            self._take_screenshot_macos()
        else:
            self._take_screenshot_default()

    def _take_screenshot_default(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            Linux / Windows 上单个全屏窗口即可覆盖所有屏幕。

        Code Logic（这个函数做什么）:
            创建单个 ScreenshotOverlay（自动截取模式），
            连接信号后调用 start()。
        """
        overlay = ScreenshotOverlay()
        overlay.screenshot_taken.connect(self._on_screenshot_taken)
        overlay.screenshot_cancelled.connect(self._on_cancelled)
        self._overlays.append(overlay)
        overlay.start()

    def _take_screenshot_macos(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            macOS 不允许单个窗口跨屏显示，需要为每个屏幕创建独立的覆盖层，
            使用户可以在任意屏幕上进行区域截图。

        Code Logic（这个函数做什么）:
            1. 通过 NSApp 激活进程（从后台触发时必须）
            2. 遍历所有屏幕，各自截取屏幕内容
            3. 为每个屏幕创建 ScreenshotOverlay（预截取模式）
            4. 连接信号后依次启动所有覆盖层
        """
        try:
            from AppKit import NSApplication  # type: ignore[import-untyped]
            NSApplication.sharedApplication().activateIgnoringOtherApps_(True)
        except ImportError:
            logger.debug("AppKit 不可用，跳过进程激活")

        screens: list[QScreen] = QGuiApplication.screens()
        logger.info("macOS 多屏截图: 检测到 %d 个屏幕", len(screens))

        for screen in screens:
            geo = screen.geometry()
            screenshot = screen.grabWindow(0)
            logger.info(
                "屏幕 %s: geo=%s, 截图=%dx%d",
                screen.name(), geo, screenshot.width(), screenshot.height(),
            )
            overlay = ScreenshotOverlay(
                screenshot=screenshot, target_geometry=geo
            )
            overlay.screenshot_taken.connect(self._on_screenshot_taken)
            overlay.screenshot_cancelled.connect(self._on_cancelled)
            self._overlays.append(overlay)

        for overlay in self._overlays:
            overlay.start()

    def _on_screenshot_taken(self, pixmap: QPixmap) -> None:
        """
        Business Logic（为什么需要这个函数）:
            截图完成后需要复制到剪贴板以便用户直接粘贴使用，
            同时通知其他模块截图已就绪。

        Code Logic（这个函数做什么）:
            将截图 pixmap 复制到系统剪贴板，发射 screenshot_ready 信号，
            然后清理所有覆盖层。
        """
        clipboard = QGuiApplication.clipboard()
        if clipboard:
            clipboard.setPixmap(pixmap)
        self.screenshot_ready.emit(pixmap)
        self._cleanup()

    def _on_cancelled(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户取消截图时需要清理所有覆盖层资源。

        Code Logic（这个函数做什么）:
            调用 _cleanup 释放所有覆盖层。
        """
        self._cleanup()

    def _cleanup(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            截图流程结束后需要释放所有覆盖层占用的内存和系统资源。

        Code Logic（这个函数做什么）:
            关闭并销毁所有覆盖层对象，清空列表。
        """
        for overlay in self._overlays:
            overlay.close()
            overlay.deleteLater()
        self._overlays.clear()
