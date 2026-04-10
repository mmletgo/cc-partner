# -*- coding: utf-8 -*-
"""Prompt 管理面板：提供 Prompt 的搜索、筛选、新建、编辑、删除和复制功能。"""

from __future__ import annotations

import asyncio
import uuid
from datetime import datetime
from typing import TYPE_CHECKING

from PyQt6.QtWidgets import (
    QWidget,
    QVBoxLayout,
    QHBoxLayout,
    QLineEdit,
    QPushButton,
    QScrollArea,
    QDialog,
    QTextEdit,
    QComboBox,
    QLabel,
    QMessageBox,
    QSizePolicy,
    QFrame,
)
from PyQt6.QtCore import pyqtSignal, Qt, QTimer, QEvent, QObject
from PyQt6.QtGui import QGuiApplication

from claude_partner.models.prompt import Prompt
from claude_partner.storage.prompt_repo import PromptRepository
from claude_partner.config import AppConfig
from claude_partner.ui.widgets.tag_widget import TagWidget, FlowLayout
from claude_partner.ui import theme

import logging

_prompt_edit_logger = logging.getLogger(__name__)


class PromptEditDialog(QDialog):
    """
    Prompt 新建/编辑弹窗：包含标题、内容和标签的编辑表单。

    Business Logic（为什么需要这个类）:
        用户新建或编辑 Prompt 时需要一个弹窗式表单来填写标题、内容和标签信息。
        编辑模式下需要预填已有数据，新建模式下生成新 ID。

    Code Logic（这个类做什么）:
        QDialog 内包含标题 QLineEdit、内容 QTextEdit、标签 TagWidget
        和确定/取消按钮。通过 get_prompt() 方法返回填写好的 Prompt 对象。
    """

    def __init__(
        self,
        prompt: Prompt | None = None,
        device_id: str = "",
        parent: QWidget | None = None,
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化弹窗 UI，编辑模式时预填数据，新建模式时准备空表单。

        Code Logic（这个函数做什么）:
            构建表单布局，如果 prompt 不为 None 则为编辑模式（填充数据），
            否则为新建模式。保存 device_id 用于新建 Prompt 时设置设备标识。
        """
        super().__init__(parent)
        self._prompt: Prompt | None = prompt
        self._device_id: str = device_id

        self.setWindowTitle("编辑 Prompt" if prompt else "新建 Prompt")
        self.setMinimumSize(500, 450)
        self.resize(550, 500)
        self.setStyleSheet(f"QDialog {{ background: {theme.BG_PRIMARY}; }}")

        layout: QVBoxLayout = QVBoxLayout(self)
        layout.setSpacing(12)
        layout.setContentsMargins(24, 24, 24, 24)

        # 标题
        title_label: QLabel = QLabel("标题")
        title_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; font-weight: 600; color: {theme.TEXT_PRIMARY}; background: transparent;"
        )
        layout.addWidget(title_label)

        self._title_input: QLineEdit = QLineEdit()
        self._title_input.setPlaceholderText("输入 Prompt 标题...")
        self._title_input.setStyleSheet(theme.input_style())
        layout.addWidget(self._title_input)

        # 内容
        content_label: QLabel = QLabel("内容")
        content_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; font-weight: 600; color: {theme.TEXT_PRIMARY}; background: transparent;"
        )
        layout.addWidget(content_label)

        self._content_input: QTextEdit = QTextEdit()
        self._content_input.setPlaceholderText("输入 Prompt 内容...")
        self._content_input.setStyleSheet(theme.input_style())
        layout.addWidget(self._content_input, stretch=1)

        # 标签
        tags_label: QLabel = QLabel("标签")
        tags_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; font-weight: 600; color: {theme.TEXT_PRIMARY}; background: transparent;"
        )
        layout.addWidget(tags_label)

        self._tag_widget: TagWidget = TagWidget()
        layout.addWidget(self._tag_widget)

        # 按钮
        btn_layout: QHBoxLayout = QHBoxLayout()
        btn_layout.addStretch()

        cancel_btn: QPushButton = QPushButton("取消")
        cancel_btn.setCursor(Qt.CursorShape.PointingHandCursor)
        cancel_btn.setStyleSheet(theme.button_secondary_style())
        cancel_btn.clicked.connect(self.reject)
        btn_layout.addWidget(cancel_btn)

        ok_btn: QPushButton = QPushButton("确定")
        ok_btn.setCursor(Qt.CursorShape.PointingHandCursor)
        ok_btn.setStyleSheet(theme.button_primary_style())
        ok_btn.clicked.connect(self.accept)
        btn_layout.addWidget(ok_btn)

        layout.addLayout(btn_layout)

        # 如果是编辑模式，填充数据
        if prompt is not None:
            self._title_input.setText(prompt.title)
            self._content_input.setPlainText(prompt.content)
            self._tag_widget.set_tags(prompt.tags)

    def get_prompt(self) -> Prompt:
        """
        Business Logic（为什么需要这个函数）:
            弹窗确认后，外部需要获取用户编辑好的 Prompt 对象进行保存。

        Code Logic（这个函数做什么）:
            新建模式: 生成新 UUID，设置 vector_clock 为 {device_id: 1}。
            编辑模式: 复用原有 ID 和 created_at，递增 vector_clock 中本设备计数器。
            返回 Prompt 实例。
        """
        now: datetime = datetime.now()
        title: str = self._title_input.text().strip()
        content: str = self._content_input.toPlainText().strip()
        tags: list[str] = self._tag_widget.get_tags()

        if self._prompt is not None:
            # 编辑模式：递增向量时钟
            vector_clock: dict[str, int] = dict(self._prompt.vector_clock)
            vector_clock[self._device_id] = vector_clock.get(self._device_id, 0) + 1
            return Prompt(
                id=self._prompt.id,
                title=title,
                content=content,
                tags=tags,
                created_at=self._prompt.created_at,
                updated_at=now,
                device_id=self._device_id,
                vector_clock=vector_clock,
                deleted=False,
            )
        else:
            # 新建模式
            return Prompt(
                id=str(uuid.uuid4()),
                title=title,
                content=content,
                tags=tags,
                created_at=now,
                updated_at=now,
                device_id=self._device_id,
                vector_clock={self._device_id: 1},
                deleted=False,
            )


class PromptPanel(QWidget):
    """
    Prompt 管理主面板：搜索、标签筛选、卡片列表、新建/编辑/删除/复制。

    Business Logic（为什么需要这个类）:
        用户需要一个完整的 Prompt 管理界面来浏览、搜索、筛选和操作 Prompt，
        是应用的核心交互面板之一。

    Code Logic（这个类做什么）:
        顶部工具栏（搜索框 + 标签筛选 + 新建按钮），
        中间滚动区域展示 PromptCard 卡片列表。
        通过 PromptRepository 执行异步 CRUD 操作，
        使用 asyncio.ensure_future 桥接同步信号与异步方法。
    """

    prompt_changed: pyqtSignal = pyqtSignal(object)

    def __init__(
        self,
        prompt_repo: PromptRepository,
        config: AppConfig,
        parent: QWidget | None = None,
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化 Prompt 管理面板的 UI 和数据依赖。

        Code Logic（这个函数做什么）:
            保存 repo 和 config 引用，构建顶部工具栏和中间卡片滚动区域，
            连接搜索框和筛选控件的信号。
        """
        super().__init__(parent)
        self._repo: PromptRepository = prompt_repo
        self._config: AppConfig = config
        self._prompts: list[Prompt] = []
        self._current_keyword: str = ""
        self._current_tag_filter: str = ""
        self._refreshing: bool = False
        self._needs_refresh: bool = False

        main_layout: QVBoxLayout = QVBoxLayout(self)
        main_layout.setContentsMargins(20, 16, 20, 16)
        main_layout.setSpacing(16)

        # === 顶部工具栏 ===
        toolbar_layout: QHBoxLayout = QHBoxLayout()
        toolbar_layout.setSpacing(8)

        # 搜索框
        self._search_input: QLineEdit = QLineEdit()
        self._search_input.setPlaceholderText("搜索 Prompt...")
        self._search_input.setStyleSheet(theme.input_style())
        self._search_input.setMinimumWidth(200)
        # 使用 QTimer 实现搜索防抖
        self._search_timer: QTimer = QTimer()
        self._search_timer.setSingleShot(True)
        self._search_timer.setInterval(300)
        self._search_timer.timeout.connect(self._on_search_trigger)
        self._search_input.textChanged.connect(lambda _: self._search_timer.start())
        toolbar_layout.addWidget(self._search_input, stretch=1)

        # 标签筛选下拉框
        self._tag_combo: QComboBox = QComboBox()
        self._tag_combo.setMinimumWidth(120)
        self._tag_combo.setStyleSheet(theme.combo_style())
        self._tag_combo.addItem("全部标签")
        self._tag_combo.currentTextChanged.connect(self._on_tag_filter_trigger)
        toolbar_layout.addWidget(self._tag_combo)

        # 新建按钮
        self._btn_new: QPushButton = QPushButton("+ 新建")
        self._btn_new.setStyleSheet(theme.button_primary_style())
        self._btn_new.setCursor(Qt.CursorShape.PointingHandCursor)
        self._btn_new.clicked.connect(lambda: asyncio.ensure_future(self._on_new()))
        toolbar_layout.addWidget(self._btn_new)

        main_layout.addLayout(toolbar_layout)

        # === 卡片滚动区域 ===
        self._scroll_area: QScrollArea = QScrollArea()
        self._scroll_area.setWidgetResizable(True)
        self._scroll_area.setHorizontalScrollBarPolicy(
            Qt.ScrollBarPolicy.ScrollBarAlwaysOff
        )
        self._scroll_area.setStyleSheet(theme.scroll_area_style())

        self._card_container: QWidget = QWidget()
        self._card_container.setStyleSheet("background: transparent;")
        self._card_layout: FlowLayout = FlowLayout(self._card_container, spacing=12)
        self._card_layout.setContentsMargins(0, 0, 0, 0)

        self._scroll_area.setWidget(self._card_container)
        main_layout.addWidget(self._scroll_area, stretch=1)

        # 空状态提示
        self._empty_label: QLabel = QLabel("暂无 Prompt，点击「+ 新建」创建第一条")
        self._empty_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._empty_label.setStyleSheet(
            f"color: {theme.TEXT_SECONDARY}; font-size: {theme.FONT_SIZE_BODY}; padding: 40px;"
        )
        self._empty_label.hide()
        main_layout.addWidget(self._empty_label)

    def _reapply_styles(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            系统主题切换时工具栏控件和空提示的颜色需要同步更新。

        Code Logic（这个函数做什么）:
            重新应用搜索框、标签筛选框、新建按钮、空提示标签的样式，
            并重建卡片列表以使用新的主题颜色。
        """
        self._search_input.setStyleSheet(theme.input_style())
        self._tag_combo.setStyleSheet(theme.combo_style())
        self._btn_new.setStyleSheet(theme.button_primary_style())
        self._empty_label.setStyleSheet(
            f"color: {theme.TEXT_SECONDARY}; font-size: {theme.FONT_SIZE_BODY}; padding: 40px;"
        )
        self._rebuild_cards()

    async def refresh(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            外部触发（如同步完成、初始化）时需要刷新 Prompt 列表展示。

        Code Logic（这个函数做什么）:
            根据当前搜索关键词和标签筛选条件从 repo 获取数据，
            刷新标签下拉框选项，重建卡片列表。
            使用并发守卫防止多次 refresh 同时执行导致状态混乱。
        """
        # 并发守卫
        if self._refreshing:
            self._needs_refresh = True
            return
        self._refreshing = True
        try:
            await self._do_refresh()
        finally:
            self._refreshing = False
            if self._needs_refresh:
                self._needs_refresh = False
                await self.refresh()

    async def _do_refresh(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            refresh() 的实际执行逻辑，由 refresh() 在并发守卫内调用。

        Code Logic（这个函数做什么）:
            获取筛选后的 Prompt 列表，刷新标签下拉框选项，重建卡片列表。
        """
        # 获取筛选后的 Prompt 列表
        if self._current_keyword and self._current_tag_filter:
            # 搜索和标签同时生效：先按标签筛选，再在结果中搜索
            tag_results: list[Prompt] = await self._repo.filter_by_tags(
                [self._current_tag_filter]
            )
            keyword_lower: str = self._current_keyword.lower()
            self._prompts = [
                p
                for p in tag_results
                if keyword_lower in p.title.lower()
                or keyword_lower in p.content.lower()
            ]
        elif self._current_keyword:
            self._prompts = await self._repo.search(self._current_keyword)
        elif self._current_tag_filter:
            self._prompts = await self._repo.filter_by_tags(
                [self._current_tag_filter]
            )
        else:
            self._prompts = await self._repo.get_all()

        # 刷新标签下拉框
        all_tags: list[str] = await self._repo.get_all_tags()
        current_tag: str = self._tag_combo.currentText()
        self._tag_combo.blockSignals(True)
        self._tag_combo.clear()
        self._tag_combo.addItem("全部标签")
        for tag in all_tags:
            self._tag_combo.addItem(tag)
        # 恢复之前的选择
        idx: int = self._tag_combo.findText(current_tag)
        if idx >= 0:
            self._tag_combo.setCurrentIndex(idx)
        else:
            # 标签已不存在，重置筛选状态
            self._current_tag_filter = ""
            self._tag_combo.setCurrentIndex(0)
        self._tag_combo.blockSignals(False)

        # 重建卡片列表
        self._rebuild_cards()

    def _rebuild_cards(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            数据变更后需要重新构建卡片 UI 展示最新的 Prompt 列表。

        Code Logic（这个函数做什么）:
            清空现有卡片，根据 self._prompts 列表重新创建 PromptCard 并添加到
            FlowLayout 网格布局中（每行 2-3 张卡片，根据窗口宽度自适应）。
            无数据时显示空状态提示。
        """
        # 延迟导入避免循环引用
        from claude_partner.ui.widgets.prompt_card import PromptCard

        # 清空现有卡片
        while self._card_layout.count() > 0:
            item = self._card_layout.takeAt(0)
            widget: QWidget | None = item.widget()
            if widget is not None:
                widget.deleteLater()

        if not self._prompts:
            self._empty_label.show()
            self._scroll_area.hide()
            return

        self._empty_label.hide()
        self._scroll_area.show()

        for prompt in self._prompts:
            card: PromptCard = PromptCard(prompt, parent=self._card_container)
            card.copy_clicked.connect(
                lambda pid: asyncio.ensure_future(self._on_copy(pid))
            )
            card.edit_clicked.connect(
                lambda pid: asyncio.ensure_future(self._on_edit(pid))
            )
            card.delete_clicked.connect(
                lambda pid: asyncio.ensure_future(self._on_delete(pid))
            )
            self._card_layout.addWidget(card)

    def _on_search_trigger(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            搜索防抖计时器到期后触发异步搜索操作。

        Code Logic（这个函数做什么）:
            读取搜索框文本，启动异步搜索协程。
        """
        keyword: str = self._search_input.text().strip()
        asyncio.ensure_future(self._on_search(keyword))

    def _on_tag_filter_trigger(self, tag_text: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            标签筛选下拉框选项变更时触发异步筛选操作。

        Code Logic（这个函数做什么）:
            读取选中的标签文本，启动异步筛选协程。
        """
        asyncio.ensure_future(self._on_tag_filter(tag_text))

    async def _on_search(self, keyword: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户在搜索框中输入关键词后需要过滤展示匹配的 Prompt。

        Code Logic（这个函数做什么）:
            更新当前搜索关键词并刷新列表。
        """
        self._current_keyword = keyword
        await self.refresh()

    async def _on_tag_filter(self, tag: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户选择标签筛选后需要只展示含该标签的 Prompt。

        Code Logic（这个函数做什么）:
            如果选择的是「全部标签」则清除筛选，否则设置当前标签过滤条件并刷新列表。
        """
        if tag == "全部标签":
            self._current_tag_filter = ""
        else:
            self._current_tag_filter = tag
        await self.refresh()

    async def _on_new(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击新建按钮后弹出编辑弹窗创建新 Prompt。

        Code Logic（这个函数做什么）:
            打开 PromptEditDialog（新建模式），用户确认后调用 repo.create 保存，
            发射 prompt_changed 信号并刷新列表。
        """
        dialog: PromptEditDialog = PromptEditDialog(
            prompt=None, device_id=self._config.device_id, parent=self
        )
        if dialog.exec() == QDialog.DialogCode.Accepted:
            new_prompt: Prompt = dialog.get_prompt()
            await self._repo.create(new_prompt)
            self.prompt_changed.emit(new_prompt)
            await self.refresh()

    async def _on_edit(self, prompt_id: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击编辑按钮或卡片后弹出编辑弹窗修改 Prompt。

        Code Logic（这个函数做什么）:
            从 repo 获取完整 Prompt 数据，打开 PromptEditDialog（编辑模式），
            用户确认后调用 repo.update 保存，发射 prompt_changed 信号并刷新列表。
        """
        prompt: Prompt | None = await self._repo.get_by_id(prompt_id)
        if prompt is None:
            return

        dialog: PromptEditDialog = PromptEditDialog(
            prompt=prompt, device_id=self._config.device_id, parent=self
        )
        if dialog.exec() == QDialog.DialogCode.Accepted:
            updated_prompt: Prompt = dialog.get_prompt()
            await self._repo.update(updated_prompt)
            self.prompt_changed.emit(updated_prompt)
            await self.refresh()

    async def _on_delete(self, prompt_id: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击删除按钮后需要确认并软删除 Prompt。

        Code Logic（这个函数做什么）:
            弹出确认对话框，用户确认后调用 repo.delete（软删除），
            发射 prompt_changed 信号并刷新列表。
        """
        reply: QMessageBox.StandardButton = QMessageBox.question(
            self,
            "确认删除",
            "确定要删除这条 Prompt 吗？",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        if reply == QMessageBox.StandardButton.Yes:
            await self._repo.delete(prompt_id)
            self.prompt_changed.emit(None)
            await self.refresh()

    async def _on_copy(self, prompt_id: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击复制按钮后将 Prompt 内容复制到系统剪贴板。

        Code Logic（这个函数做什么）:
            从 repo 获取 Prompt，调用 copy_content() 获取文本，
            写入系统剪贴板。
        """
        prompt: Prompt | None = await self._repo.get_by_id(prompt_id)
        if prompt is None:
            return

        clipboard = QGuiApplication.clipboard()
        if clipboard is not None:
            clipboard.setText(prompt.copy_content())
