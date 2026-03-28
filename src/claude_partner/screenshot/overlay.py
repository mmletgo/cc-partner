# -*- coding: utf-8 -*-
"""截图覆盖层模块：全屏半透明覆盖层，实现区域截图选择。"""

from __future__ import annotations

import logging
import sys

from PyQt6.QtCore import QPoint, QRect, Qt, pyqtSignal
from PyQt6.QtGui import (
    QColor,
    QGuiApplication,
    QKeyEvent,
    QMouseEvent,
    QPainter,
    QPaintEvent,
    QPen,
    QPixmap,
)
from PyQt6.QtWidgets import QWidget

logger = logging.getLogger(__name__)


class ScreenshotOverlay(QWidget):
    """
    全屏半透明覆盖层，实现区域截图选择。

    Business Logic（为什么需要这个类）:
        用户需要在屏幕上框选区域来截图，截图后可粘贴到 Claude Code 中。
        覆盖层提供可视化的选区交互，选区内无遮罩、周围半透明，
        让用户清晰地看到选择的区域。

    Code Logic（这个类做什么）:
        显示时先截取全屏作为背景，覆盖半透明遮罩。
        用户鼠标拖动时实时绘制选区（选区内显示原图，周围保持遮罩）。
        鼠标释放后裁剪选区图像并通过信号发射。
        支持 ESC 键取消和多显示器。
    """

    screenshot_taken = pyqtSignal(QPixmap)
    screenshot_cancelled = pyqtSignal()

    def __init__(
        self,
        screenshot: QPixmap | None = None,
        target_geometry: QRect | None = None,
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化覆盖层窗口，设置为无边框、置顶、工具窗口样式。

        Code Logic（这个函数做什么）:
            配置窗口标志（无边框、置顶、工具窗口），设置十字光标，
            初始化截图和选区坐标变量。
            可选接收预截取的截图和目标几何（macOS 多屏模式下由
            ScreenshotManager 为每个屏幕独立创建覆盖层时传入）。
        """
        super().__init__()
        self.setWindowFlags(
            Qt.WindowType.FramelessWindowHint
            | Qt.WindowType.WindowStaysOnTopHint
            | Qt.WindowType.Tool
        )
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground, False)
        self.setCursor(Qt.CursorShape.CrossCursor)

        self._screenshot: QPixmap | None = screenshot
        self._target_geometry: QRect | None = target_geometry
        self._origin: QPoint | None = None
        self._current: QPoint | None = None

    def start(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户触发截图时需要截取当前全屏内容作为背景，
            然后全屏显示覆盖层让用户选择区域。

        Code Logic（这个函数做什么）:
            两种模式:
            - 预截取模式（macOS 多屏）: 截图和几何由外部传入，直接显示
            - 自动截取模式（默认）: 自行截取虚拟桌面并全屏显示
        """
        if self._screenshot is not None and self._target_geometry is not None:
            # 预截取模式：截图和几何由 ScreenshotManager 传入
            self.setGeometry(self._target_geometry)
            self.show()
            self.raise_()
            self.activateWindow()
            logger.info("覆盖层已显示（预截取模式）: %s", self._target_geometry)
            return

        # 自动截取模式
        primary_screen = QGuiApplication.primaryScreen()
        if primary_screen is None:
            logger.error("无法获取主屏幕")
            self.screenshot_cancelled.emit()
            return

        virtual_geo: QRect = primary_screen.virtualGeometry()
        self._screenshot = primary_screen.grabWindow(
            0,
            virtual_geo.x(),
            virtual_geo.y(),
            virtual_geo.width(),
            virtual_geo.height(),
        )
        logger.info(
            "截图已捕获: %dx%d, overlay 区域: %s",
            self._screenshot.width(), self._screenshot.height(), virtual_geo,
        )
        self.setGeometry(virtual_geo)
        self.showFullScreen()

    def paintEvent(self, event: QPaintEvent) -> None:
        """
        Business Logic（为什么需要这个函数）:
            覆盖层需要实时绘制背景截图、半透明遮罩和选区效果，
            让用户直观地看到自己选择了哪个区域。

        Code Logic（这个函数做什么）:
            1. 绘制全屏截图作为背景
            2. 覆盖半透明黑色遮罩（alpha=100）
            3. 如果用户正在拖动选区，在选区内重新绘制原图（去除遮罩）
            4. 绘制选区边框（#0078D4 蓝色虚线 2px）
        """
        painter = QPainter(self)
        if self._screenshot:
            # 绘制截图作为背景
            painter.drawPixmap(0, 0, self._screenshot)
            # 半透明遮罩
            painter.fillRect(self.rect(), QColor(0, 0, 0, 100))

            # 绘制选区
            if self._origin and self._current:
                selection: QRect = QRect(self._origin, self._current).normalized()
                if selection.width() > 2 and selection.height() > 2:
                    # 在选区内重新绘制原图（去除遮罩效果）
                    painter.drawPixmap(selection, self._screenshot, selection)
                    # 选区边框
                    pen = QPen(QColor(0, 122, 255), 2)  # #007AFF Apple Blue
                    pen.setStyle(Qt.PenStyle.DashLine)
                    painter.setPen(pen)
                    painter.drawRect(selection)
        painter.end()

    def mousePressEvent(self, event: QMouseEvent) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户按下鼠标左键时开始选区操作。

        Code Logic（这个函数做什么）:
            记录鼠标按下位置作为选区起点，触发重绘。
        """
        if event.button() == Qt.MouseButton.LeftButton:
            self._origin = event.pos()
            self._current = event.pos()
            self.update()

    def mouseMoveEvent(self, event: QMouseEvent) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户拖动鼠标时需要实时更新选区范围并重绘。

        Code Logic（这个函数做什么）:
            更新当前鼠标位置并触发重绘，paintEvent 会根据新坐标绘制选区。
        """
        if self._origin:
            self._current = event.pos()
            self.update()

    def mouseReleaseEvent(self, event: QMouseEvent) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户释放鼠标时完成选区，需要裁剪选中区域的截图并发射信号。

        Code Logic（这个函数做什么）:
            计算归一化的选区矩形，如果宽高均 >= 10 像素则裁剪并发射
            screenshot_taken 信号；否则视为无效选区发射 cancelled 信号。
            最后关闭覆盖层并重置状态。
        """
        if (
            event.button() == Qt.MouseButton.LeftButton
            and self._origin
            and self._current
        ):
            selection: QRect = QRect(self._origin, self._current).normalized()
            if (
                selection.width() >= 10
                and selection.height() >= 10
                and self._screenshot
            ):
                cropped: QPixmap = self._screenshot.copy(selection)
                self.screenshot_taken.emit(cropped)
            else:
                self.screenshot_cancelled.emit()
            self.close()
            self._origin = None
            self._current = None

    def keyPressEvent(self, event: QKeyEvent) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户按 ESC 键时需要取消截图并关闭覆盖层。

        Code Logic（这个函数做什么）:
            检测 ESC 键按下，发射 cancelled 信号并关闭窗口。
        """
        if event.key() == Qt.Key.Key_Escape:
            self.screenshot_cancelled.emit()
            self.close()
