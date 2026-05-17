# -*- coding: utf-8 -*-
"""
macOS 权限引导欢迎页。

Business Logic:
    macOS 上应用首次启动时，用户不清楚需要授予权限。直接弹出系统设置页面
    体验差且令人困惑。需要一个引导页清晰展示需要哪些权限、各自用途，
    让用户逐一前往系统设置授权。

Code Logic:
    深色背景的无边框 QWidget，展示应用图标、权限卡片列表和操作按钮。
    通过 QTimer 定时轮询权限状态，实时更新各卡片的授权状态。
"""

from __future__ import annotations

import logging
import subprocess
import sys
from typing import Callable

from PyQt6.QtCore import QTimer, Qt, pyqtSignal
from PyQt6.QtGui import QColor, QFont, QPainter, QPixmap
from PyQt6.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSizePolicy,
    QVBoxLayout,
    QWidget,
)

from claude_partner.ui import theme

logger: logging.Logger = logging.getLogger(__name__)


class PermissionCard(QFrame):
    """
    单个权限的卡片行，显示权限名称、说明、授权状态和操作按钮。

    Business Logic:
        每个权限需要清晰展示其名称、用途说明和当前授权状态，
        未授权时提供"去设置"按钮引导用户前往系统设置。

    Code Logic:
        QFrame 包含 QHBoxLayout：左侧权限名+说明，右侧状态指示+操作按钮。
        授权后按钮隐藏，显示绿色勾选状态。
    """

    def __init__(
        self,
        name: str,
        description: str,
        check_fn: Callable[[], bool],
        request_fn: Callable[[], None],
        parent: QWidget | None = None,
    ) -> None:
        """
        Business Logic:
            初始化单个权限卡片，传入权限名、说明文字、检查函数和请求函数。

        Code Logic:
            创建 QHBoxLayout，左侧放置权限名和说明的 QLabel，
            右侧放置状态 QLabel 和"去设置" QPushButton。
        """
        super().__init__(parent)
        self._check_fn: Callable[[], bool] = check_fn
        self._request_fn: Callable[[], None] = request_fn
        self._granted: bool = False

        self.setObjectName("permissionCard")
        self.setStyleSheet(self._card_style())
        self.setFixedHeight(72)

        layout: QHBoxLayout = QHBoxLayout(self)
        layout.setContentsMargins(16, 12, 16, 12)
        layout.setSpacing(8)

        # 左侧：权限名 + 说明
        left_layout: QVBoxLayout = QVBoxLayout()
        left_layout.setSpacing(2)

        self._name_label: QLabel = QLabel(name)
        self._name_label.setStyleSheet(
            "font-size: 14px; font-weight: 600; color: #FFFFFF;"
        )
        left_layout.addWidget(self._name_label)

        self._desc_label: QLabel = QLabel(description)
        self._desc_label.setStyleSheet("font-size: 12px; color: #86868B;")
        self._desc_label.setWordWrap(True)
        left_layout.addWidget(self._desc_label)

        layout.addLayout(left_layout, 1)

        # 右侧：状态 + 按钮
        self._status_label: QLabel = QLabel()
        self._status_label.setFixedWidth(16)
        self._status_label.setFixedHeight(16)
        layout.addWidget(self._status_label)

        self._action_btn: QPushButton = QPushButton("去设置")
        self._action_btn.setFixedSize(72, 32)
        self._action_btn.setStyleSheet(self._action_btn_style())
        self._action_btn.setCursor(Qt.CursorShape.PointingHandCursor)
        self._action_btn.clicked.connect(self._on_action)
        layout.addWidget(self._action_btn)

        self._granted_label: QLabel = QLabel("已授权")
        self._granted_label.setStyleSheet(
            "font-size: 13px; color: #34C759; font-weight: 500;"
        )
        self._granted_label.hide()
        layout.addWidget(self._granted_label)

        # 初始刷新状态
        self.refresh_status()

    @staticmethod
    def _card_style() -> str:
        """权限卡片样式。"""
        return """
            #permissionCard {
                background: #2C2C2E;
                border-radius: 10px;
                border: none;
            }
        """

    @staticmethod
    def _action_btn_style() -> str:
        """去设置按钮样式。"""
        return """
            QPushButton {
                background: #007AFF;
                color: white;
                border: none;
                border-radius: 8px;
                font-size: 12px;
                font-weight: 600;
            }
            QPushButton:hover {
                background: #0062CC;
            }
            QPushButton:pressed {
                background: #004999;
            }
        """

    def refresh_status(self) -> None:
        """
        Business Logic:
            用户在系统设置中授权后，欢迎页需要实时反映权限变化。

        Code Logic:
            调用 _check_fn 检查权限，更新状态指示灯和按钮可见性。
        """
        self._granted = self._check_fn()
        if self._granted:
            # 绿色圆点（通过 QPixmap 绘制）
            pixmap: QPixmap = QPixmap(16, 16)
            pixmap.fill(QColor(0, 0, 0, 0))
            painter: QPainter = QPainter(pixmap)
            painter.setRenderHint(QPainter.RenderHint.Antialiasing, True)
            painter.setPen(Qt.PenStyle.NoPen)
            painter.setBrush(QColor("#34C759"))
            painter.drawEllipse(2, 2, 12, 12)
            # 白色勾
            painter.setPen(QColor("white"))
            tick_font: QFont = QFont("Arial", 9, QFont.Weight.Bold)
            painter.setFont(tick_font)
            painter.drawText(pixmap.rect(), Qt.AlignmentFlag.AlignCenter, "✓")
            painter.end()
            self._status_label.setPixmap(pixmap)
            self._action_btn.hide()
            self._granted_label.show()
        else:
            # 红色圆点
            pixmap = QPixmap(16, 16)
            pixmap.fill(QColor(0, 0, 0, 0))
            painter = QPainter(pixmap)
            painter.setRenderHint(QPainter.RenderHint.Antialiasing, True)
            painter.setPen(Qt.PenStyle.NoPen)
            painter.setBrush(QColor("#FF3B30"))
            painter.drawEllipse(2, 2, 12, 12)
            painter.end()
            self._status_label.setPixmap(pixmap)
            self._action_btn.show()
            self._granted_label.hide()

    @property
    def granted(self) -> bool:
        """返回当前权限是否已授权。"""
        return self._granted

    def _on_action(self) -> None:
        """
        Business Logic:
            用户点击"去设置"按钮后，需要打开系统设置中对应的权限页面。

        Code Logic:
            调用 _request_fn 打开系统设置。
        """
        self._request_fn()


class WelcomeWindow(QWidget):
    """
    macOS 权限引导欢迎页。

    Business Logic:
        首次启动或权限缺失时，展示深色背景的引导页，
        让用户理解每个权限的用途并逐一前往系统设置授权。
        权限全部获得后自动启用"继续使用"按钮。

    Code Logic:
        无边框 QWidget，QVBoxLayout 居中布局，
        包含应用图标、标题、说明文字、权限卡片列表和底部按钮。
        QTimer 每 2 秒轮询权限状态。
    """

    # 所有权限均已授权
    all_permissions_granted: pyqtSignal = pyqtSignal()
    # 用户点击跳过
    skip_requested: pyqtSignal = pyqtSignal()

    def __init__(
        self,
        check_screen_capture: Callable[[], bool],
        check_input_monitoring: Callable[[], bool],
        request_screen_capture: Callable[[], None],
        request_input_monitoring: Callable[[], None],
        parent: QWidget | None = None,
    ) -> None:
        """
        Business Logic:
            创建欢迎页，传入各权限的检查和请求函数。

        Code Logic:
            构建完整 UI 布局，初始化权限卡片和定时器。
        """
        super().__init__(parent)
        self._cards: list[PermissionCard] = []
        self._setup_window()
        self._build_ui(
            check_screen_capture,
            check_input_monitoring,
            request_screen_capture,
            request_input_monitoring,
        )
        self._setup_poll_timer()

    def _setup_window(self) -> None:
        """配置窗口属性：无边框、固定大小、居中。"""
        # 不能加 WindowStaysOnTopHint：用户去"系统设置 → 隐私与安全性"
        # 授权时本窗口会盖住系统设置，导致用户无法操作。
        self.setWindowFlags(Qt.WindowType.FramelessWindowHint)
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground, False)
        self.setFixedSize(480, 520)
        self.setStyleSheet(
            "QWidget { background: #1D1D1F; }"
        )
        # 居中显示
        from PyQt6.QtWidgets import QApplication
        screen = QApplication.primaryScreen()
        if screen is not None:
            geo = screen.availableGeometry()
            self.move(
                (geo.width() - self.width()) // 2 + geo.x(),
                (geo.height() - self.height()) // 2 + geo.y(),
            )

    def _build_ui(
        self,
        check_screen_capture: Callable[[], bool],
        check_input_monitoring: Callable[[], bool],
        request_screen_capture: Callable[[], None],
        request_input_monitoring: Callable[[], None],
    ) -> None:
        """构建 UI 布局。"""
        root: QVBoxLayout = QVBoxLayout(self)
        root.setContentsMargins(40, 40, 40, 32)
        root.setSpacing(0)

        # ── 应用图标 ──
        icon_label: QLabel = QLabel()
        icon: QPixmap = theme.create_app_icon(128).pixmap(128, 128)
        icon_label.setPixmap(icon)
        icon_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        icon_label.setFixedHeight(100)
        root.addWidget(icon_label)

        root.addSpacing(16)

        # ── 标题 ──
        title: QLabel = QLabel("Claude Partner")
        title.setAlignment(Qt.AlignmentFlag.AlignCenter)
        title.setStyleSheet(
            "font-size: 24px; font-weight: 700; color: #FFFFFF;"
        )
        root.addWidget(title)

        root.addSpacing(8)

        # ── 说明文字 ──
        desc: QLabel = QLabel(
            "为了正常使用截图和快捷键功能，\n应用需要以下系统权限。"
        )
        desc.setAlignment(Qt.AlignmentFlag.AlignCenter)
        desc.setStyleSheet("font-size: 13px; color: #86868B; line-height: 1.5;")
        root.addWidget(desc)

        root.addSpacing(24)

        # ── 权限卡片列表 ──
        cards_layout: QVBoxLayout = QVBoxLayout()
        cards_layout.setSpacing(10)

        screen_card: PermissionCard = PermissionCard(
            name="屏幕录制",
            description="用于区域截图功能，捕获屏幕内容",
            check_fn=check_screen_capture,
            request_fn=request_screen_capture,
        )
        self._cards.append(screen_card)
        cards_layout.addWidget(screen_card)

        input_card: PermissionCard = PermissionCard(
            name="输入监控",
            description="用于全局快捷键，在任何应用中触发截图",
            check_fn=check_input_monitoring,
            request_fn=request_input_monitoring,
        )
        self._cards.append(input_card)
        cards_layout.addWidget(input_card)

        root.addLayout(cards_layout)

        root.addSpacing(1)  # 弹簧，把按钮推到底部
        root.addStretch(1)

        # ── 底部按钮区域 ──
        btn_layout: QVBoxLayout = QVBoxLayout()
        btn_layout.setSpacing(8)
        btn_layout.setAlignment(Qt.AlignmentFlag.AlignCenter)

        self._continue_btn: QPushButton = QPushButton("继续使用")
        self._continue_btn.setFixedSize(200, 40)
        self._continue_btn.setStyleSheet(self._continue_btn_style())
        self._continue_btn.setCursor(Qt.CursorShape.PointingHandCursor)
        self._continue_btn.clicked.connect(self._on_continue)
        btn_layout.addWidget(self._continue_btn, alignment=Qt.AlignmentFlag.AlignCenter)

        self._skip_btn: QPushButton = QPushButton("暂时跳过")
        self._skip_btn.setFixedSize(100, 28)
        self._skip_btn.setStyleSheet(
            """
            QPushButton {
                background: transparent;
                color: #86868B;
                border: none;
                font-size: 12px;
            }
            QPushButton:hover {
                color: #AEAEB2;
            }
            """
        )
        self._skip_btn.setCursor(Qt.CursorShape.PointingHandCursor)
        self._skip_btn.clicked.connect(self.skip_requested.emit)
        btn_layout.addWidget(self._skip_btn, alignment=Qt.AlignmentFlag.AlignCenter)

        root.addLayout(btn_layout)

        # 初始状态刷新
        self._update_continue_btn()

    @staticmethod
    def _continue_btn_style() -> str:
        """继续按钮样式。"""
        return """
            QPushButton {
                background: #007AFF;
                color: white;
                border: none;
                border-radius: 10px;
                font-size: 15px;
                font-weight: 600;
            }
            QPushButton:hover {
                background: #0062CC;
            }
            QPushButton:pressed {
                background: #004999;
            }
            QPushButton:disabled {
                background: #3A3A3C;
                color: #636366;
            }
        """

    def _setup_poll_timer(self) -> None:
        """设置定时器轮询权限状态。"""
        self._poll_timer: QTimer = QTimer(self)
        self._poll_timer.timeout.connect(self._poll_permissions)
        self._poll_timer.start(2000)  # 2 秒轮询

    def _poll_permissions(self) -> None:
        """
        Business Logic:
            用户在系统设置中授权后，欢迎页需要自动检测到变化。

        Code Logic:
            刷新每个权限卡片的状态，如果全部已授权则启用"继续"按钮
            并发射 all_permissions_granted 信号。
        """
        all_granted: bool = True
        for card in self._cards:
            card.refresh_status()
            if not card.granted:
                all_granted = False

        self._update_continue_btn()

        if all_granted:
            logger.info("所有权限已授予")
            self.all_permissions_granted.emit()

    def _update_continue_btn(self) -> None:
        """根据权限状态更新继续按钮的启用状态。"""
        all_granted: bool = all(card.granted for card in self._cards)
        self._continue_btn.setEnabled(all_granted)

    def _on_continue(self) -> None:
        """用户点击"继续使用"。"""
        self.all_permissions_granted.emit()

    def closeEvent(self, event: object) -> None:
        """关闭时停止轮询定时器。"""
        self._poll_timer.stop()
        super().closeEvent(event)  # type: ignore[arg-type]


# ── 权限检查辅助函数（供外部调用） ──


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
            return bool(Quartz.CGPreflightScreenCaptureAccess())
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

        def _dummy(proxy: object, etype: int, event: object, ref: object) -> object:
            return event

        tap = Quartz.CGEventTapCreate(
            Quartz.kCGHIDEventTap,
            Quartz.kCGHeadInsertEventTap,
            Quartz.kCGEventTapOptionListenOnly,
            Quartz.CGEventMaskBit(Quartz.kCGEventKeyDown),
            _dummy,
            None,
        )
        if tap is None:
            return False
        Quartz.CFMachPortInvalidate(tap)
        return True
    except ImportError:
        pass
    return True


def request_screen_capture() -> None:
    """
    Business Logic:
        用户点击"去设置"后需要打开系统设置的屏幕录制页面。

    Code Logic:
        调用 Quartz.CGRequestScreenCaptureAccess() 打开系统设置。
    """
    try:
        import Quartz  # type: ignore[import-untyped]
        if hasattr(Quartz, "CGRequestScreenCaptureAccess"):
            Quartz.CGRequestScreenCaptureAccess()
            logger.info("已请求 macOS 屏幕录制权限")
    except ImportError:
        pass


def request_input_monitoring() -> None:
    """
    Business Logic:
        用户点击"去设置"后需要打开系统设置的输入监控页面。

    Code Logic:
        通过 URL scheme 打开系统设置的输入监控面板。
    """
    subprocess.Popen([
        "open",
        "x-apple.systempreferences:"
        "com.apple.preference.security?Privacy_ListenEvent",
    ])
    logger.info("已打开 macOS 输入监控设置页面")


def needs_welcome() -> bool:
    """
    Business Logic:
        启动时需要判断是否需要显示欢迎页，避免已授权用户看到不必要的引导。

    Code Logic:
        仅在 macOS 打包环境中检查，任一权限缺失返回 True。
    """
    return not (check_screen_capture_access() and check_input_monitoring_access())
