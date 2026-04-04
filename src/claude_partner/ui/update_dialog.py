# -*- coding: utf-8 -*-
"""
版本更新对话框模块：展示新版本信息、下载进度和安装操作。

Business Logic:
    用户需要了解新版本的更新内容，并完成下载和安装操作。
    该对话框提供完整的版本更新交互流程：查看更新 → 下载 → 安装重启。

Code Logic:
    基于 QDialog 实现状态机驱动的更新对话框，状态包括
    IDLE / DOWNLOADING / READY_TO_INSTALL / CANCELLED / FAILED。
    通过信号通知外部控制器执行实际的下载和安装操作。
"""

from __future__ import annotations

from enum import Enum, auto

from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtGui import QCursor
from PyQt6.QtWidgets import (
    QDialog,
    QHBoxLayout,
    QLabel,
    QProgressBar,
    QPushButton,
    QTextEdit,
    QVBoxLayout,
    QWidget,
)

from claude_partner.ui import theme


class _DialogState(Enum):
    """对话框内部状态枚举。"""
    IDLE = auto()
    DOWNLOADING = auto()
    READY_TO_INSTALL = auto()
    CANCELLED = auto()
    FAILED = auto()


class UpdateDialog(QDialog):
    """
    版本更新对话框，展示新版本信息并引导用户完成下载和安装。

    Business Logic（为什么需要这个类）:
        当检测到新版本时，需要向用户展示版本号、更新内容和安装包大小，
        并提供下载进度反馈和安装操作入口。

    Code Logic（这个类做什么）:
        基于 QDialog 的状态机对话框，通过 _state 管理当前界面状态。
        状态转换：IDLE → DOWNLOADING → READY_TO_INSTALL，
        也可转入 CANCELLED 或 FAILED。外部通过信号驱动状态变化。
    """

    # 用户点击"立即更新"时发射
    download_requested = pyqtSignal()
    # 用户点击"安装并重启"时发射，参数为下载文件路径
    install_requested = pyqtSignal(str)

    def __init__(self, update_info: "UpdateInfo", parent: QWidget | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            对话框需要基于版本更新信息初始化所有 UI 控件和状态。

        Code Logic（这个函数做什么）:
            接收 UpdateInfo 实例，初始化内部状态为 IDLE，
            创建完整的对话框布局（标题、更新内容、文件大小、进度条、按钮），
            连接按钮信号到对应的状态转换逻辑。
        """
        super().__init__(parent)
        self._update_info: "UpdateInfo" = update_info
        self._state: _DialogState = _DialogState.IDLE
        self._downloaded_file_path: str = ""

        self.setWindowTitle("版本更新")
        self.setMinimumWidth(420)
        self.setStyleSheet(theme.dialog_style())

        self._setup_ui()

    def _setup_ui(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户需要看到清晰的版本更新信息和操作按钮。

        Code Logic（这个函数做什么）:
            创建 QVBoxLayout 主布局（spacing=16, margins=24,24,24,24），
            依次添加标题标签、更新内容区域（可选）、文件大小标签、
            进度区域（初始隐藏）、按钮行。
        """
        layout: QVBoxLayout = QVBoxLayout(self)
        layout.setContentsMargins(24, 24, 24, 24)
        layout.setSpacing(16)

        # 标题
        self._title_label: QLabel = QLabel(
            f"新版本 {self._update_info.version} 可用"
        )
        self._title_label.setStyleSheet(theme.label_title_style())
        layout.addWidget(self._title_label)

        # Release Notes（如果有 body 内容）
        if self._update_info.body:
            notes_title: QLabel = QLabel("更新内容：")
            notes_title.setStyleSheet(theme.label_body_style())
            layout.addWidget(notes_title)

            self._notes_edit: QTextEdit = QTextEdit()
            self._notes_edit.setReadOnly(True)
            self._notes_edit.setMaximumHeight(120)
            self._notes_edit.setPlainText(self._update_info.body)
            self._notes_edit.setStyleSheet(theme.input_style())
            layout.addWidget(self._notes_edit)

        # 文件大小
        size_mb: float = self._update_info.download_size / (1024 * 1024)
        self._size_label: QLabel = QLabel(f"安装包大小: {size_mb:.1f} MB")
        self._size_label.setStyleSheet(theme.label_caption_style())
        layout.addWidget(self._size_label)

        # 进度区域（初始隐藏）
        self._progress_bar: QProgressBar = QProgressBar()
        self._progress_bar.setRange(0, 100)
        self._progress_bar.setValue(0)
        self._progress_bar.setStyleSheet(theme.progress_bar_style())
        self._progress_bar.hide()

        self._progress_label: QLabel = QLabel("")
        self._progress_label.setStyleSheet(theme.label_caption_style())
        self._progress_label.hide()

        layout.addWidget(self._progress_bar)
        layout.addWidget(self._progress_label)

        # 按钮行
        btn_layout: QHBoxLayout = QHBoxLayout()
        btn_layout.addStretch()

        self._secondary_btn: QPushButton = QPushButton("稍后再说")
        self._secondary_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._secondary_btn.setStyleSheet(theme.button_secondary_style())
        self._secondary_btn.clicked.connect(self.reject)

        self._primary_btn: QPushButton = QPushButton("立即更新")
        self._primary_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._primary_btn.setStyleSheet(theme.button_primary_style())
        self._primary_btn.clicked.connect(self._on_primary_clicked)

        btn_layout.addWidget(self._secondary_btn)
        btn_layout.addWidget(self._primary_btn)
        layout.addLayout(btn_layout)

    def _on_primary_clicked(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            主按钮在不同状态下执行不同的操作：发起下载、安装重启或重试下载。

        Code Logic（这个函数做什么）:
            根据当前 _state 判断操作类型：
            - IDLE / FAILED：发射 download_requested 信号
            - READY_TO_INSTALL：发射 install_requested 信号（携带文件路径）
        """
        if self._state in (_DialogState.IDLE, _DialogState.FAILED):
            self.download_requested.emit()
        elif self._state == _DialogState.READY_TO_INSTALL:
            self.install_requested.emit(self._downloaded_file_path)

    def show_download_state(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击"立即更新"后，需要显示下载进度并防止重复操作。

        Code Logic（这个函数做什么）:
            将状态切换为 DOWNLOADING，显示进度条和状态文字，
            禁用主按钮，将次要按钮文本改为"取消"。
        """
        self._state = _DialogState.DOWNLOADING

        self._progress_bar.setValue(0)
        self._progress_bar.show()
        self._progress_label.setText("正在下载...")
        self._progress_label.show()

        self._primary_btn.setEnabled(False)
        self._secondary_btn.setText("取消")

        # 重新应用样式确保主题一致
        self._reapply_styles()

    def set_download_progress(self, progress: float) -> None:
        """
        Business Logic（为什么需要这个函数）:
            下载过程中用户需要实时看到进度百分比和预估状态。

        Code Logic（这个函数做什么）:
            接收 0.0~1.0 的浮点进度值，更新进度条和状态文字。
            如果当前状态不是 DOWNLOADING 则忽略。
        """
        if self._state != _DialogState.DOWNLOADING:
            return

        percent: int = int(progress * 100)
        self._progress_bar.setValue(percent)
        self._progress_label.setText(f"正在下载... {percent}%")

    def set_download_completed(self, file_path: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            下载完成后用户需要立即安装新版本。

        Code Logic（这个函数做什么）:
            将状态切换为 READY_TO_INSTALL，记录下载文件路径，
            进度条设为 100%，主按钮变为"安装并重启"并启用，
            次要按钮恢复为"稍后再说"。
        """
        self._state = _DialogState.READY_TO_INSTALL
        self._downloaded_file_path = file_path

        self._progress_bar.setValue(100)
        self._progress_label.setText("下载完成，可以安装")

        self._primary_btn.setText("安装并重启")
        self._primary_btn.setEnabled(True)
        self._secondary_btn.setText("稍后再说")

    def set_download_failed(self, error: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            下载失败时用户需要了解失败原因并能重试。

        Code Logic（这个函数做什么）:
            将状态切换为 FAILED，隐藏进度条，
            显示失败原因文字，主按钮变为"重试"并启用，
            次要按钮恢复为"稍后再说"。
        """
        self._state = _DialogState.FAILED

        self._progress_bar.hide()
        self._progress_label.setText(f"下载失败: {error}")
        self._progress_label.show()

        self._primary_btn.setText("重试")
        self._primary_btn.setEnabled(True)
        self._secondary_btn.setText("稍后再说")

    def get_download_url(self) -> str:
        """
        Business Logic（为什么需要这个函数）:
            外部下载控制器需要知道从哪个 URL 下载安装包。

        Code Logic（这个函数做什么）:
            返回 update_info 中的 download_url 字段。
        """
        return self._update_info.download_url

    def get_download_filename(self) -> str:
        """
        Business Logic（为什么需要这个函数）:
            外部下载控制器需要知道保存文件时应使用的文件名。

        Code Logic（这个函数做什么）:
            返回 update_info 中的 download_filename 字段。
        """
        return self._update_info.download_filename

    def _reapply_styles(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            主题切换时对话框需要重新应用所有样式以保持视觉一致。

        Code Logic（这个函数做什么）:
            重新设置对话框和所有子控件的 QSS 样式。
        """
        self.setStyleSheet(theme.dialog_style())
        self._title_label.setStyleSheet(theme.label_title_style())
        self._size_label.setStyleSheet(theme.label_caption_style())
        self._progress_bar.setStyleSheet(theme.progress_bar_style())
        self._progress_label.setStyleSheet(theme.label_caption_style())
        self._primary_btn.setStyleSheet(theme.button_primary_style())
        self._secondary_btn.setStyleSheet(theme.button_secondary_style())

        if hasattr(self, "_notes_edit"):
            self._notes_edit.setStyleSheet(theme.input_style())
