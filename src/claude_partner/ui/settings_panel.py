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
    """

    settings_changed = pyqtSignal(object)  # 传出更新后的 AppConfig

    def __init__(self, config: AppConfig, parent: QWidget | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            设置面板需要基于当前配置初始化所有控件的显示值。

        Code Logic（这个函数做什么）:
            接收 AppConfig 实例，创建表单布局：设备名称输入、接收目录选择、
            快捷键下拉选择、Wayland 提示（Linux）、保存按钮。
            各控件的初始值从 config 对象读取。
        """
        super().__init__(parent)
        self._config: AppConfig = config

        layout = QVBoxLayout(self)
        layout.setContentsMargins(32, 24, 32, 24)
        layout.setSpacing(24)

        # 标题
        title = QLabel("设置")
        title.setStyleSheet(
            "font-size: 22px; font-weight: 700; color: #1D1D1F; "
            "border: none; background: transparent;"
        )
        layout.addWidget(title)

        # --- 设备设置 ---
        section1 = QLabel("设备")
        section1.setStyleSheet(
            "font-size: 13px; font-weight: 600; color: #86868B; "
            "text-transform: uppercase; border: none; background: transparent;"
        )
        layout.addWidget(section1)

        # 设备名称
        name_row = QHBoxLayout()
        name_label = QLabel("设备名称")
        name_label.setFixedWidth(120)
        name_label.setStyleSheet(
            "font-size: 14px; color: #1D1D1F; border: none; background: transparent;"
        )
        self._name_input = QLineEdit(config.device_name)
        self._name_input.setStyleSheet(
            "QLineEdit { border: 1px solid #E5E5EA; border-radius: 10px; "
            "padding: 10px 14px; font-size: 14px; background: white; }"
            "QLineEdit:focus { border-color: #007AFF; }"
        )
        name_row.addWidget(name_label)
        name_row.addWidget(self._name_input, stretch=1)
        layout.addLayout(name_row)

        # 接收目录
        dir_row = QHBoxLayout()
        dir_label = QLabel("接收目录")
        dir_label.setFixedWidth(120)
        dir_label.setStyleSheet(
            "font-size: 14px; color: #1D1D1F; border: none; background: transparent;"
        )
        self._dir_input = QLineEdit(config.receive_dir)
        self._dir_input.setStyleSheet(
            "QLineEdit { border: 1px solid #E5E5EA; border-radius: 10px; "
            "padding: 10px 14px; font-size: 14px; background: white; }"
            "QLineEdit:focus { border-color: #007AFF; }"
        )
        browse_btn = QPushButton("浏览")
        browse_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        browse_btn.setStyleSheet(
            "QPushButton { background: transparent; color: #007AFF; "
            "border: 1px solid #E5E5EA; border-radius: 10px; "
            "padding: 10px 16px; font-size: 14px; }"
            "QPushButton:hover { background: #F5F5F7; }"
        )
        browse_btn.clicked.connect(self._browse_dir)
        dir_row.addWidget(dir_label)
        dir_row.addWidget(self._dir_input, stretch=1)
        dir_row.addWidget(browse_btn)
        layout.addLayout(dir_row)

        # --- 快捷键设置 ---
        section2 = QLabel("快捷键")
        section2.setStyleSheet(
            "font-size: 13px; font-weight: 600; color: #86868B; "
            "text-transform: uppercase; border: none; background: transparent;"
        )
        layout.addWidget(section2)

        hotkey_row = QHBoxLayout()
        hotkey_label = QLabel("截图快捷键")
        hotkey_label.setFixedWidth(120)
        hotkey_label.setStyleSheet(
            "font-size: 14px; color: #1D1D1F; border: none; background: transparent;"
        )
        self._hotkey_combo = QComboBox()
        self._hotkey_combo.setStyleSheet(
            "QComboBox { border: 1px solid #E5E5EA; border-radius: 10px; "
            "padding: 10px 14px; font-size: 14px; background: white; min-height: 20px; }"
            "QComboBox:hover { border-color: #007AFF; }"
            "QComboBox::drop-down { subcontrol-origin: padding; "
            "subcontrol-position: top right; width: 30px; "
            "border-left: 1px solid #E5E5EA; "
            "border-top-right-radius: 10px; border-bottom-right-radius: 10px; "
            "background: #F5F5F7; }"
            "QComboBox::down-arrow { width: 10px; height: 10px; image: none; "
            "border-left: 4px solid transparent; border-right: 4px solid transparent; "
            "border-top: 5px solid #86868B; }"
            "QComboBox QAbstractItemView { border: 1px solid #E5E5EA; "
            "background: white; selection-background-color: #E8F0FE; "
            "selection-color: #1D1D1F; padding: 4px; font-size: 14px; }"
            "QComboBox QAbstractItemView::item { min-height: 32px; padding: 6px 10px; }"
        )
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

        hotkey_row.addWidget(hotkey_label)
        hotkey_row.addWidget(self._hotkey_combo, stretch=1)
        layout.addLayout(hotkey_row)

        # Wayland 提示（仅 Linux）
        if sys.platform.startswith("linux"):
            wayland_note = QLabel(
                "提示: Wayland 桌面环境下全局快捷键可能不可用，建议使用 X11"
            )
            wayland_note.setStyleSheet(
                "font-size: 11px; color: #AEAEB2; border: none; background: transparent;"
            )
            wayland_note.setWordWrap(True)
            layout.addWidget(wayland_note)

        layout.addStretch()

        # 保存按钮
        save_btn = QPushButton("保存设置")
        save_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        save_btn.setFixedWidth(160)
        save_btn.setStyleSheet(
            "QPushButton { background: #007AFF; color: white; border: none; "
            "border-radius: 10px; padding: 12px 24px; font-size: 15px; font-weight: 600; }"
            "QPushButton:hover { background: #0062CC; }"
            "QPushButton:pressed { background: #004999; }"
        )
        save_btn.clicked.connect(self._save)

        btn_row = QHBoxLayout()
        btn_row.addStretch()
        btn_row.addWidget(save_btn)
        layout.addLayout(btn_row)

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
