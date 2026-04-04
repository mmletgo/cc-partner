# -*- coding: utf-8 -*-
"""设置面板模块：提供应用配置的图形化编辑界面。"""

import sys
import logging

from PyQt6.QtWidgets import (
    QWidget,
    QVBoxLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QComboBox,
    QFileDialog,
    QMessageBox,
)
from PyQt6.QtCore import pyqtSignal, Qt
from PyQt6.QtGui import QCursor

from claude_partner.config import AppConfig
from claude_partner.hotkey.listener import (
    HOTKEY_PRESETS,
    pynput_to_display,
    display_to_pynput,
)
from claude_partner.ui import theme

logger = logging.getLogger(__name__)


class SettingsPanel(QWidget):
    """
    设置面板，提供应用配置的图形化编辑。

    Business Logic（为什么需要这个类）:
        用户需要配置设备名称、文件接收目录、截图快捷键等参数，
        需要一个直观的图形界面来完成这些设置操作。

    Code Logic（这个类做什么）:
        表单布局，读取 AppConfig 填充控件，保存时写回 config。
        包含设备名称输入框、接收目录选择、快捷键下拉选择等控件。
        保存后通过 settings_changed 信号通知外部组件配置已更新。
        所有颜色样式通过 theme 模块统一管理，支持深浅色主题切换。
    """

    settings_changed = pyqtSignal(object)  # 传出更新后的 AppConfig

    def __init__(self, config: AppConfig, parent: QWidget | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            设置面板需要基于当前配置初始化所有控件的显示值。

        Code Logic（这个函数做什么）:
            接收 AppConfig 实例，创建表单布局：设备名称输入、接收目录选择、
            快捷键下拉选择、Wayland 提示（Linux）、保存按钮。
            各控件的初始值从 config 对象读取，颜色样式通过 theme 模块获取。
        """
        super().__init__(parent)
        self._config: AppConfig = config

        layout: QVBoxLayout = QVBoxLayout(self)
        layout.setContentsMargins(32, 24, 32, 24)
        layout.setSpacing(24)

        # 标题
        self._title: QLabel = QLabel("设置")
        self._title.setStyleSheet(
            f"font-size: 22px; font-weight: 700; color: {theme.TEXT_PRIMARY}; "
            "border: none; background: transparent;"
        )
        layout.addWidget(self._title)

        # --- 设备设置 ---
        self._section1: QLabel = QLabel("设备")
        self._section1.setStyleSheet(
            f"font-size: 13px; font-weight: 600; color: {theme.TEXT_SECONDARY}; "
            "text-transform: uppercase; border: none; background: transparent;"
        )
        layout.addWidget(self._section1)

        # 设备名称
        name_row: QHBoxLayout = QHBoxLayout()
        self._name_label: QLabel = QLabel("设备名称")
        self._name_label.setFixedWidth(120)
        self._name_label.setStyleSheet(
            f"font-size: 14px; color: {theme.TEXT_PRIMARY}; border: none; background: transparent;"
        )
        self._name_input: QLineEdit = QLineEdit(config.device_name)
        self._name_input.setStyleSheet(theme.input_style())
        name_row.addWidget(self._name_label)
        name_row.addWidget(self._name_input, stretch=1)
        layout.addLayout(name_row)

        # 接收目录
        dir_row: QHBoxLayout = QHBoxLayout()
        self._dir_label: QLabel = QLabel("接收目录")
        self._dir_label.setFixedWidth(120)
        self._dir_label.setStyleSheet(
            f"font-size: 14px; color: {theme.TEXT_PRIMARY}; border: none; background: transparent;"
        )
        self._dir_input: QLineEdit = QLineEdit(config.receive_dir)
        self._dir_input.setStyleSheet(theme.input_style())
        self._browse_btn: QPushButton = QPushButton("浏览")
        self._browse_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._browse_btn.setStyleSheet(theme.button_secondary_style())
        self._browse_btn.clicked.connect(self._browse_dir)
        dir_row.addWidget(self._dir_label)
        dir_row.addWidget(self._dir_input, stretch=1)
        dir_row.addWidget(self._browse_btn)
        layout.addLayout(dir_row)

        # --- 快捷键设置 ---
        self._section2: QLabel = QLabel("快捷键")
        self._section2.setStyleSheet(
            f"font-size: 13px; font-weight: 600; color: {theme.TEXT_SECONDARY}; "
            "text-transform: uppercase; border: none; background: transparent;"
        )
        layout.addWidget(self._section2)

        hotkey_row: QHBoxLayout = QHBoxLayout()
        self._hotkey_label: QLabel = QLabel("截图快捷键")
        self._hotkey_label.setFixedWidth(120)
        self._hotkey_label.setStyleSheet(
            f"font-size: 14px; color: {theme.TEXT_PRIMARY}; border: none; background: transparent;"
        )
        self._hotkey_combo: QComboBox = QComboBox()
        self._hotkey_combo.setStyleSheet(theme.combo_style())
        # 填充预设
        current_display: str = pynput_to_display(config.screenshot_hotkey)
        for pynput_fmt, display_fmt in HOTKEY_PRESETS:
            self._hotkey_combo.addItem(display_fmt, pynput_fmt)
        # 选中当前配置
        idx: int = self._hotkey_combo.findData(config.screenshot_hotkey)
        if idx >= 0:
            self._hotkey_combo.setCurrentIndex(idx)
        else:
            # 自定义值，添加到列表
            self._hotkey_combo.addItem(current_display, config.screenshot_hotkey)
            self._hotkey_combo.setCurrentIndex(self._hotkey_combo.count() - 1)

        hotkey_row.addWidget(self._hotkey_label)
        hotkey_row.addWidget(self._hotkey_combo, stretch=1)
        layout.addLayout(hotkey_row)

        # Wayland 提示（仅 Linux）
        self._wayland_note: QLabel | None = None
        if sys.platform.startswith("linux"):
            self._wayland_note = QLabel(
                "提示: Wayland 桌面环境下全局快捷键可能不可用，建议使用 X11"
            )
            self._wayland_note.setStyleSheet(
                f"font-size: 11px; color: {theme.TEXT_TERTIARY}; border: none; background: transparent;"
            )
            self._wayland_note.setWordWrap(True)
            layout.addWidget(self._wayland_note)

        layout.addStretch()

        # 保存按钮
        self._save_btn: QPushButton = QPushButton("保存设置")
        self._save_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._save_btn.setFixedWidth(160)
        self._save_btn.setStyleSheet(theme.button_primary_style())
        self._save_btn.clicked.connect(self._save)

        btn_row: QHBoxLayout = QHBoxLayout()
        btn_row.addStretch()
        btn_row.addWidget(self._save_btn)
        layout.addLayout(btn_row)

    def _reapply_styles(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            当应用切换深浅色主题时，设置面板中所有控件的颜色样式需要同步更新，
            否则会出现颜色不匹配的视觉问题。

        Code Logic（这个函数做什么）:
            重新为所有保存的实例变量控件调用 theme 模块的样式函数或常量，
            生成新的 QSS 字符串并设置到对应控件上。
        """
        # 标题
        self._title.setStyleSheet(
            f"font-size: 22px; font-weight: 700; color: {theme.TEXT_PRIMARY}; "
            "border: none; background: transparent;"
        )

        # 分区标题
        section_style: str = (
            f"font-size: 13px; font-weight: 600; color: {theme.TEXT_SECONDARY}; "
            "text-transform: uppercase; border: none; background: transparent;"
        )
        self._section1.setStyleSheet(section_style)
        self._section2.setStyleSheet(section_style)

        # 标签
        label_style: str = (
            f"font-size: 14px; color: {theme.TEXT_PRIMARY}; "
            "border: none; background: transparent;"
        )
        self._name_label.setStyleSheet(label_style)
        self._dir_label.setStyleSheet(label_style)

        # 输入框
        input_ss: str = theme.input_style()
        self._name_input.setStyleSheet(input_ss)
        self._dir_input.setStyleSheet(input_ss)

        # 浏览按钮
        self._browse_btn.setStyleSheet(theme.button_secondary_style())

        # 快捷键标签
        self._hotkey_label.setStyleSheet(label_style)

        # 快捷键下拉框
        self._hotkey_combo.setStyleSheet(theme.combo_style())

        # Wayland 提示
        if self._wayland_note is not None:
            self._wayland_note.setStyleSheet(
                f"font-size: 11px; color: {theme.TEXT_TERTIARY}; "
                "border: none; background: transparent;"
            )

        # 保存按钮
        self._save_btn.setStyleSheet(theme.button_primary_style())

    def _browse_dir(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户选择文件接收目录时，需要通过系统原生的目录选择对话框来浏览文件系统。

        Code Logic（这个函数做什么）:
            打开 QFileDialog 目录选择对话框，以当前输入框路径为起始目录，
            用户选择后将路径填入输入框。
        """
        dir_path: str = QFileDialog.getExistingDirectory(
            self, "选择接收目录", self._dir_input.text()
        )
        if dir_path:
            self._dir_input.setText(dir_path)

    def _save(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击保存按钮后，需要将面板上的配置值写回 AppConfig 并持久化到磁盘，
            同时通知其他组件配置已更新。

        Code Logic（这个函数做什么）:
            从各控件读取当前值，更新 AppConfig 实例的对应字段，
            调用 config.save() 写入 JSON 文件，
            通过 settings_changed 信号传出更新后的配置，
            最后弹出保存成功提示。
        """
        self._config.device_name = self._name_input.text().strip()
        self._config.receive_dir = self._dir_input.text().strip()
        hotkey_data: str | None = self._hotkey_combo.currentData()
        if hotkey_data:
            self._config.screenshot_hotkey = hotkey_data
        self._config.save()
        self.settings_changed.emit(self._config)
        QMessageBox.information(self, "保存成功", "设置已保存")
