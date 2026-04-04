# -*- coding: utf-8 -*-
"""速记本面板：临时 Markdown 笔记区域，退出即清空，支持一键复制。"""

from PyQt6.QtCore import Qt
from PyQt6.QtGui import QCursor, QFont, QGuiApplication
from PyQt6.QtWidgets import (
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from claude_partner.ui import theme


class ScratchpadPanel(QWidget):
    """
    临时速记本面板：提供纯文本编辑区，用于快速记录想法。

    Business Logic（为什么需要这个类）:
        用户在使用 Claude Partner 时，经常需要临时记录一些想法或备注，
        这些内容不需要持久化存储，只需在当前会话中保留，并能快速复制到剪贴板。

    Code Logic（这个类做什么）:
        QWidget 面板，包含一个 QPlainTextEdit 编辑区（等宽字体）和工具栏按钮
        （复制全部、清空）。内容仅存在于内存中，应用退出后自动丢失。
        实现 _reapply_styles() 以支持深浅色主题切换。
    """

    def __init__(self, parent: QWidget | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化速记本面板的 UI 布局和交互控件。

        Code Logic（这个函数做什么）:
            创建顶部标题行（标题 + 字数统计）、工具栏（复制/清空按钮）、
            以及占据主要空间的纯文本编辑区。
        """
        super().__init__(parent)

        layout: QVBoxLayout = QVBoxLayout(self)
        layout.setContentsMargins(24, 20, 24, 20)
        layout.setSpacing(12)

        # ── 顶部标题行 ──
        header_layout: QHBoxLayout = QHBoxLayout()
        header_layout.setSpacing(8)

        title_label: QLabel = QLabel("速记本")
        title_label.setStyleSheet(theme.label_title_style())

        self._char_count_label: QLabel = QLabel("0 字")
        self._char_count_label.setStyleSheet(theme.label_caption_style())

        header_layout.addWidget(title_label)
        header_layout.addStretch()
        header_layout.addWidget(self._char_count_label)

        layout.addLayout(header_layout)

        # ── 提示文字 ──
        hint_label: QLabel = QLabel("临时记录你的想法，内容不会保存，退出应用时自动清空")
        hint_label.setStyleSheet(theme.label_caption_style())
        layout.addWidget(hint_label)

        # ── 工具栏 ──
        toolbar_layout: QHBoxLayout = QHBoxLayout()
        toolbar_layout.setSpacing(10)

        self._copy_btn: QPushButton = QPushButton("复制全部")
        self._copy_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._copy_btn.setStyleSheet(theme.button_primary_style())
        self._copy_btn.clicked.connect(self._on_copy)

        self._clear_btn: QPushButton = QPushButton("清空")
        self._clear_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._clear_btn.setStyleSheet(theme.button_danger_style())
        self._clear_btn.clicked.connect(self._on_clear)

        toolbar_layout.addWidget(self._copy_btn)
        toolbar_layout.addWidget(self._clear_btn)
        toolbar_layout.addStretch()

        layout.addLayout(toolbar_layout)

        # ── 编辑区 ──
        self._editor: QPlainTextEdit = QPlainTextEdit()
        self._editor.setPlaceholderText("在这里写下你的想法...")
        self._editor.textChanged.connect(self._on_text_changed)

        # 等宽字体，适合 Markdown
        editor_font: QFont = QFont(
            "Menlo, Consolas, 'Courier New', monospace", 13
        )
        self._editor.setFont(editor_font)

        self._apply_editor_style()
        layout.addWidget(self._editor, stretch=1)

    def _apply_editor_style(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            编辑区的样式需要跟随深浅色主题变化。

        Code Logic（这个函数做什么）:
            设置 QPlainTextEdit 的 QSS 样式，包括背景色、边框、圆角等。
        """
        self._editor.setStyleSheet(f"""
            QPlainTextEdit {{
                border: 1px solid {theme.BORDER};
                border-radius: {theme.RADIUS_MEDIUM};
                padding: 12px 16px;
                font-size: 14px;
                background: {theme.BG_PRIMARY};
                color: {theme.TEXT_PRIMARY};
                selection-background-color: {theme.BG_TERTIARY};
            }}
            QPlainTextEdit:focus {{
                border-color: {theme.ACCENT};
            }}
        """)

    def _on_text_changed(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户编辑内容时，需要实时更新字数统计，帮助用户了解内容长度。

        Code Logic（这个函数做什么）:
            获取编辑区纯文本，计算字符数并更新字数标签。
        """
        text: str = self._editor.toPlainText()
        char_count: int = len(text)
        self._char_count_label.setText(f"{char_count} 字")

    def _on_copy(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户记录想法后，最常见的需求就是复制内容到剪贴板，
            以便粘贴到 Claude Code 或其他应用中使用。

        Code Logic（这个函数做什么）:
            获取编辑区全部文本，通过 QGuiApplication.clipboard() 写入系统剪贴板。
            如果内容为空则不执行复制。
        """
        text: str = self._editor.toPlainText()
        if not text:
            return

        clipboard = QGuiApplication.clipboard()
        if clipboard is not None:
            clipboard.setText(text)

    def _on_clear(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户可能需要清空当前内容重新开始记录，
            但为了避免误操作，需要确认弹窗。

        Code Logic（这个函数做什么）:
            弹出确认对话框，用户确认后清空编辑区内容。
        """
        text: str = self._editor.toPlainText()
        if not text:
            return

        reply: QMessageBox.StandardButton = QMessageBox.question(
            self,
            "确认清空",
            "确定要清空所有内容吗？",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        if reply == QMessageBox.StandardButton.Yes:
            self._editor.clear()

    def _reapply_styles(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            系统主题切换时，速记本面板的所有组件样式需要更新。

        Code Logic（这个函数做什么）:
            重新应用标题、提示文字、按钮和编辑区的样式。
        """
        self._apply_editor_style()
