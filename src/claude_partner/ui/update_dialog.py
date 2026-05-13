# -*- coding: utf-8 -*-
"""
版本更新对话框模块：展示新版本信息、下载进度和安装操作。

Business Logic:
    用户需要了解新版本的更新内容，并完成下载和安装操作。
    该对话框参考 macOS 应用（如 Mos）的更新提示风格：
    顶部图标 + 标题 + 版本对比说明，中部 Markdown 渲染的更新说明卡片，
    底部左侧"跳过这个版本"、右侧"安装更新"。

Code Logic:
    基于 QDialog 实现状态机驱动的更新对话框，状态包括
    IDLE / DOWNLOADING / READY_TO_INSTALL / CANCELLED / FAILED。
    更新说明使用 QTextEdit.setMarkdown() 渲染。
    通过信号通知外部控制器执行实际的下载和安装操作。
"""

from __future__ import annotations

from enum import Enum, auto

from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtGui import QCursor, QPixmap
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

from claude_partner import __version__
from claude_partner.ui import theme


# 应用展示名称
_APP_NAME: str = "Claude Partner"


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
        当检测到新版本时，需要向用户展示版本号对比、Markdown 格式的更新说明
        和安装包大小，并提供下载进度反馈和安装操作入口。视觉风格参考 macOS
        原生应用更新提示，营造熟悉的桌面体验。

    Code Logic（这个类做什么）:
        基于 QDialog 的状态机对话框，通过 _state 管理当前界面状态。
        状态转换：IDLE → DOWNLOADING → READY_TO_INSTALL，
        也可转入 CANCELLED 或 FAILED。外部通过信号驱动状态变化。
        更新说明用 QTextEdit.setMarkdown() 渲染。
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
            创建参考 Mos 风格的更新提示布局（顶部图标+标题、
            Markdown 渲染的更新说明卡片、底部跳过/安装按钮），
            连接按钮信号到对应的状态转换逻辑。
        """
        super().__init__(parent)
        self._update_info: "UpdateInfo" = update_info
        self._state: _DialogState = _DialogState.IDLE
        self._downloaded_file_path: str = ""

        self.setWindowTitle("软件更新")
        self.setMinimumWidth(560)
        self.setStyleSheet(theme.dialog_style())

        self._setup_ui()

    def _setup_ui(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户需要看到清晰的版本对比、更新说明和操作按钮。

        Code Logic（这个函数做什么）:
            构建三段式布局：
            1. 顶部 header（应用图标 + 标题 + 版本对比说明）
            2. 中部 Markdown 更新说明卡片（QTextEdit + setMarkdown）
            3. 底部按钮行（跳过这个版本 / 安装更新）
            进度条与状态文字位于底部按钮上方，初始隐藏。
        """
        layout: QVBoxLayout = QVBoxLayout(self)
        layout.setContentsMargins(24, 24, 24, 20)
        layout.setSpacing(16)

        # ── 顶部 header：图标 + 标题块 ──
        layout.addLayout(self._build_header())

        # ── Markdown 更新说明卡片 ──
        self._notes_edit: QTextEdit | None = None
        if self._update_info.body:
            self._notes_edit = self._build_notes_edit(self._update_info.body)
            layout.addWidget(self._notes_edit, stretch=1)

        # ── 进度区域（初始隐藏） ──
        self._progress_bar: QProgressBar = QProgressBar()
        self._progress_bar.setRange(0, 100)
        self._progress_bar.setValue(0)
        self._progress_bar.setStyleSheet(theme.progress_bar_style())
        self._progress_bar.hide()
        layout.addWidget(self._progress_bar)

        self._progress_label: QLabel = QLabel("")
        self._progress_label.setStyleSheet(theme.label_caption_style())
        self._progress_label.hide()
        layout.addWidget(self._progress_label)

        # ── 底部按钮行 ──
        layout.addLayout(self._build_button_row())

    def _build_header(self) -> QHBoxLayout:
        """
        Business Logic（为什么需要这个函数）:
            顶部 header 是用户第一眼看到的区域，需要应用图标 +
            "新版本的 X 已经发布" 标题 + 版本对比说明，
            参考 macOS 原生更新提示的视觉层级。

        Code Logic（这个函数做什么）:
            构建水平布局：左侧应用图标（64x64），右侧两行文字
            （加粗标题 + 普通说明文字，含新旧版本对比和安装包大小）。
        """
        header: QHBoxLayout = QHBoxLayout()
        header.setSpacing(16)
        header.setContentsMargins(0, 0, 0, 0)

        # 左侧应用图标
        icon_label: QLabel = QLabel()
        icon_pixmap: QPixmap = theme.create_app_icon(64).pixmap(64, 64)
        icon_label.setPixmap(icon_pixmap)
        icon_label.setFixedSize(64, 64)
        icon_label.setAlignment(Qt.AlignmentFlag.AlignTop)
        header.addWidget(icon_label, alignment=Qt.AlignmentFlag.AlignTop)

        # 右侧标题块
        text_col: QVBoxLayout = QVBoxLayout()
        text_col.setSpacing(6)
        text_col.setContentsMargins(0, 2, 0, 0)

        self._title_label: QLabel = QLabel(f"新版本的 {_APP_NAME} 已经发布")
        self._title_label.setStyleSheet(self._title_style())
        text_col.addWidget(self._title_label)

        size_mb: float = self._update_info.download_size / (1024 * 1024)
        size_text: str = (
            f"（安装包 {size_mb:.1f} MB）" if self._update_info.download_size > 0 else ""
        )
        self._subtitle_label: QLabel = QLabel(
            f"{_APP_NAME} {self._update_info.version} 可供下载，"
            f"您现在的版本是 {__version__}。要现在下载吗？{size_text}"
        )
        self._subtitle_label.setStyleSheet(self._subtitle_style())
        self._subtitle_label.setWordWrap(True)
        text_col.addWidget(self._subtitle_label)
        text_col.addStretch()

        header.addLayout(text_col, stretch=1)
        return header

    def _build_notes_edit(self, markdown_body: str) -> QTextEdit:
        """
        Business Logic（为什么需要这个函数）:
            GitHub Release 的 body 通常是 Markdown 格式（含标题、列表、
            嵌套列表等），需要按原始格式渲染以匹配仓库发布说明的视觉效果。

        Code Logic（这个函数做什么）:
            创建只读 QTextEdit，调用 setMarkdown() 渲染 GitHub 风格的
            更新说明，并应用卡片化的边框/圆角样式。
        """
        edit: QTextEdit = QTextEdit()
        edit.setReadOnly(True)
        edit.setMarkdown(markdown_body)
        edit.setMinimumHeight(200)
        edit.setStyleSheet(self._notes_style())
        return edit

    def _build_button_row(self) -> QHBoxLayout:
        """
        Business Logic（为什么需要这个函数）:
            按钮行需要清晰区分次要操作（跳过版本/取消）和主操作
            （安装更新/安装并重启），参考 Mos 的"跳过这个版本"左对齐、
            "安装更新"右对齐的布局。

        Code Logic（这个函数做什么）:
            构建水平按钮布局：左侧次要按钮（默认文本"跳过这个版本"），
            中间弹簧，右侧主按钮（默认文本"安装更新"）。
        """
        btn_layout: QHBoxLayout = QHBoxLayout()
        btn_layout.setContentsMargins(0, 4, 0, 0)

        self._secondary_btn: QPushButton = QPushButton("跳过这个版本")
        self._secondary_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._secondary_btn.setStyleSheet(theme.button_secondary_style())
        self._secondary_btn.setMinimumHeight(34)
        self._secondary_btn.clicked.connect(self.reject)

        self._primary_btn: QPushButton = QPushButton("安装更新")
        self._primary_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._primary_btn.setStyleSheet(theme.button_primary_style())
        self._primary_btn.setMinimumHeight(34)
        self._primary_btn.setDefault(True)
        self._primary_btn.clicked.connect(self._on_primary_clicked)

        btn_layout.addWidget(self._secondary_btn)
        btn_layout.addStretch()
        btn_layout.addWidget(self._primary_btn)
        return btn_layout

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
            用户点击"安装更新"后，需要显示下载进度并防止重复操作。

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
        self._primary_btn.setText("下载中…")
        self._secondary_btn.setText("取消")

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
        self._title_label.setStyleSheet(self._title_style())
        self._subtitle_label.setStyleSheet(self._subtitle_style())
        self._progress_bar.setStyleSheet(theme.progress_bar_style())
        self._progress_label.setStyleSheet(theme.label_caption_style())
        self._primary_btn.setStyleSheet(theme.button_primary_style())
        self._secondary_btn.setStyleSheet(theme.button_secondary_style())

        if self._notes_edit is not None:
            self._notes_edit.setStyleSheet(self._notes_style())

    @staticmethod
    def _title_style() -> str:
        """
        Business Logic（为什么需要这个函数）:
            更新对话框的主标题需要比通用 title 更醒目一些
            （Mos 风格：17~18px 粗体）。

        Code Logic（这个函数做什么）:
            返回 QLabel 的加粗大标题 QSS，使用 theme 颜色常量。
        """
        return f"""
            font-size: 17px;
            font-weight: 700;
            font-family: {theme.FONT_FAMILY};
            color: {theme.TEXT_PRIMARY};
        """

    @staticmethod
    def _subtitle_style() -> str:
        """
        Business Logic（为什么需要这个函数）:
            副标题需要与主标题形成层级对比，使用次级文字色 + 正文字号。

        Code Logic（这个函数做什么）:
            返回 QLabel 的副标题 QSS。
        """
        return f"""
            font-size: {theme.FONT_SIZE_BODY};
            font-family: {theme.FONT_FAMILY};
            color: {theme.TEXT_SECONDARY};
        """

    @staticmethod
    def _notes_style() -> str:
        """
        Business Logic（为什么需要这个函数）:
            Markdown 渲染区需要卡片化的视觉容器：圆角、细边框、
            柔和的内边距，整体观感接近 Mos 的更新说明卡片。

        Code Logic（这个函数做什么）:
            返回带圆角 + BORDER 细边框 + BG_PRIMARY 背景的 QTextEdit QSS。
        """
        return f"""
            QTextEdit {{
                border: 1px solid {theme.BORDER};
                border-radius: {theme.RADIUS_MEDIUM};
                background: {theme.BG_PRIMARY};
                color: {theme.TEXT_PRIMARY};
                padding: 12px 16px;
                font-size: {theme.FONT_SIZE_BODY};
                font-family: {theme.FONT_FAMILY};
            }}
        """
