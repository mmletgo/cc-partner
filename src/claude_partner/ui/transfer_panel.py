# -*- coding: utf-8 -*-
"""文件传输面板：展示传输任务列表，支持发送文件和拖拽发送。"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING

import subprocess
import sys

from PyQt6.QtWidgets import (
    QWidget,
    QVBoxLayout,
    QHBoxLayout,
    QPushButton,
    QComboBox,
    QLabel,
    QProgressBar,
    QScrollArea,
    QFrame,
    QFileDialog,
)
from PyQt6.QtCore import pyqtSignal, Qt
from PyQt6.QtGui import QCursor, QDragEnterEvent, QDropEvent, QMouseEvent

from claude_partner.models.device import Device
from claude_partner.models.transfer import TransferStatus, TransferDirection, TransferTask
from claude_partner.ui import theme

if TYPE_CHECKING:
    from claude_partner.transfer.sender import FileSender
    from claude_partner.transfer.receiver import FileReceiver


def _format_size(size_bytes: int) -> str:
    """
    Business Logic（为什么需要这个函数）:
        UI 展示文件大小时需要自动选择合适的单位（B/KB/MB/GB），
        让用户直观理解文件大小。

    Code Logic（这个函数做什么）:
        根据字节数逐级除以 1024，选择最合适的单位并格式化为
        最多两位小数的字符串。
    """
    if size_bytes < 1024:
        return f"{size_bytes} B"
    elif size_bytes < 1024 * 1024:
        return f"{size_bytes / 1024:.1f} KB"
    elif size_bytes < 1024 * 1024 * 1024:
        return f"{size_bytes / (1024 * 1024):.1f} MB"
    else:
        return f"{size_bytes / (1024 * 1024 * 1024):.2f} GB"


# 传输状态到中文标签和颜色的映射（Apple 色系）
_STATUS_DISPLAY: dict[TransferStatus, tuple[str, str]] = {
    TransferStatus.PENDING: ("等待中", theme.TEXT_SECONDARY),
    TransferStatus.TRANSFERRING: ("传输中", theme.ACCENT),
    TransferStatus.COMPLETED: ("已完成", theme.GREEN),
    TransferStatus.FAILED: ("失败", theme.RED),
    TransferStatus.CANCELLED: ("已取消", theme.ORANGE),
}

# 传输状态到卡片背景色的映射（Apple 柔和色系）
_STATUS_BG: dict[TransferStatus, str] = {
    TransferStatus.PENDING: theme.BG_PRIMARY,
    TransferStatus.TRANSFERRING: theme.STATUS_BG_TRANSFERRING,
    TransferStatus.COMPLETED: theme.STATUS_BG_COMPLETED,
    TransferStatus.FAILED: theme.STATUS_BG_FAILED,
    TransferStatus.CANCELLED: theme.STATUS_BG_CANCELLED,
}


class TransferItemWidget(QFrame):
    """
    单个传输任务项，在传输列表中展示文件名、方向、进度条、大小和状态。

    Business Logic（为什么需要这个类）:
        传输列表中的每个任务需要独立展示其传输进度、状态和操作按钮，
        让用户可以监控每个任务的进展并在需要时取消传输。

    Code Logic（这个类做什么）:
        使用 QFrame 容器，水平布局排列：方向图标、文件名/大小标签、
        进度条、状态标签和取消按钮。提供 update_progress 和
        update_status 方法供外部更新显示。
    """

    cancel_clicked: pyqtSignal = pyqtSignal(str)  # transfer_id

    def __init__(self, task: TransferTask, parent: QWidget | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            根据传入的 TransferTask 数据构建任务项的完整 UI。

        Code Logic（这个函数做什么）:
            保存 transfer_id 和 task 引用，创建方向图标、文件名标签、
            进度条、大小/状态标签和取消按钮，设置扁平圆角边框样式和柔和阴影。
        """
        super().__init__(parent)
        self._transfer_id: str = task.id
        self._task: TransferTask = task
        self._saved_path: str | None = None  # 接收完成后的文件保存路径

        self.setFrameShape(QFrame.Shape.StyledPanel)
        self._apply_status_style(task.status)
        theme.apply_shadow(self)

        main_layout: QVBoxLayout = QVBoxLayout(self)
        main_layout.setContentsMargins(14, 12, 14, 12)
        main_layout.setSpacing(8)

        # 第一行：方向图标 + 文件名 + 取消按钮
        top_layout: QHBoxLayout = QHBoxLayout()
        top_layout.setSpacing(10)

        direction_text: str = "\u2191 发送" if task.direction == TransferDirection.SEND else "\u2193 接收"
        direction_color: str = theme.ACCENT if task.direction == TransferDirection.SEND else theme.GREEN
        self._direction_label: QLabel = QLabel(direction_text)
        self._direction_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_CAPTION}; font-weight: 600; color: {direction_color}; "
            f"background: transparent; border: none;"
        )
        self._direction_label.setFixedWidth(60)
        top_layout.addWidget(self._direction_label)

        self._filename_label: QLabel = QLabel(task.filename)
        self._filename_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; font-weight: 600; color: {theme.TEXT_PRIMARY}; "
            f"background: transparent; border: none;"
        )
        self._filename_label.setWordWrap(True)
        top_layout.addWidget(self._filename_label, stretch=1)

        self._cancel_btn: QPushButton = QPushButton("取消")
        self._cancel_btn.setFixedSize(50, 26)
        self._cancel_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._cancel_btn.setStyleSheet(theme.button_danger_compact_style())
        self._cancel_btn.clicked.connect(
            lambda: self.cancel_clicked.emit(self._transfer_id)
        )
        # 完成/失败/取消状态下隐藏取消按钮
        if task.status in (
            TransferStatus.COMPLETED,
            TransferStatus.FAILED,
            TransferStatus.CANCELLED,
        ):
            self._cancel_btn.hide()
        top_layout.addWidget(self._cancel_btn)

        main_layout.addLayout(top_layout)

        # 第二行：进度条
        self._progress_bar: QProgressBar = QProgressBar()
        self._progress_bar.setRange(0, 100)
        self._progress_bar.setValue(int(task.progress() * 100))
        self._progress_bar.setFixedHeight(8)
        self._progress_bar.setStyleSheet(theme.progress_bar_style())
        main_layout.addWidget(self._progress_bar)

        # 第三行：大小 + 状态
        bottom_layout: QHBoxLayout = QHBoxLayout()
        bottom_layout.setSpacing(10)

        self._size_label: QLabel = QLabel(
            f"{_format_size(task.transferred_bytes)} / {_format_size(task.size)}"
        )
        self._size_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_SMALL}; color: {theme.TEXT_SECONDARY}; "
            f"background: transparent; border: none;"
        )
        bottom_layout.addWidget(self._size_label)

        bottom_layout.addStretch()

        status_text, status_color = _STATUS_DISPLAY.get(
            task.status, ("未知", theme.TEXT_TERTIARY)
        )
        self._status_label: QLabel = QLabel(status_text)
        self._status_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_SMALL}; font-weight: 600; color: {status_color}; "
            f"background: transparent; border: none;"
        )
        bottom_layout.addWidget(self._status_label)

        main_layout.addLayout(bottom_layout)

    def _apply_status_style(self, status: TransferStatus) -> None:
        """
        Business Logic（为什么需要这个函数）:
            不同传输状态需要用不同的背景色直观区分，让用户快速识别任务状态。

        Code Logic（这个函数做什么）:
            根据传输状态设置卡片的 QSS 样式（背景色、边框、圆角）。
        """
        bg_color: str = _STATUS_BG.get(status, theme.BG_PRIMARY)
        self.setStyleSheet(
            f"""
            TransferItemWidget {{
                border: 1px solid {theme.BORDER};
                border-radius: {theme.RADIUS_LARGE};
                padding: 4px;
                background: {bg_color};
            }}
            """
        )

    def update_progress(self, progress: float) -> None:
        """
        Business Logic（为什么需要这个函数）:
            传输过程中需要实时更新进度条和大小显示，让用户了解传输进展。

        Code Logic（这个函数做什么）:
            接收 0.0~1.0 的进度值，更新进度条百分比和已传输/总大小标签。
        """
        percent: int = int(progress * 100)
        self._progress_bar.setValue(percent)
        transferred: int = int(progress * self._task.size)
        self._size_label.setText(
            f"{_format_size(transferred)} / {_format_size(self._task.size)}"
        )

    def update_status(self, status: TransferStatus) -> None:
        """
        Business Logic（为什么需要这个函数）:
            传输完成、失败或取消时需要更新状态标签和卡片背景色，
            并隐藏不再需要的取消按钮。

        Code Logic（这个函数做什么）:
            更新状态标签文字和颜色，更新卡片背景色样式，
            终态（完成/失败/取消）时隐藏取消按钮。
        """
        status_text, status_color = _STATUS_DISPLAY.get(status, ("未知", theme.TEXT_SECONDARY))
        self._status_label.setText(status_text)
        self._status_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_SMALL}; font-weight: 600; color: {status_color}; "
            f"background: transparent; border: none;"
        )
        self._apply_status_style(status)

        if status in (
            TransferStatus.COMPLETED,
            TransferStatus.FAILED,
            TransferStatus.CANCELLED,
        ):
            self._cancel_btn.hide()

    @property
    def transfer_id(self) -> str:
        """
        Business Logic（为什么需要这个函数）:
            外部需要获取此任务项对应的传输 ID。

        Code Logic（这个函数做什么）:
            返回 transfer_id 字符串。
        """
        return self._transfer_id

    def set_saved_path(self, path: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            接收完成后记录文件保存路径，用户点击卡片时可以打开所在目录。

        Code Logic（这个函数做什么）:
            保存文件路径，设置手型光标和 tooltip 提示用户可点击。
        """
        self._saved_path = path
        self.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self.setToolTip(f"点击打开文件所在目录: {path}")

    def mousePressEvent(self, event: QMouseEvent | None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击已完成的接收卡片时，打开文件所在目录方便查找文件。

        Code Logic（这个函数做什么）:
            如果有保存路径且是左键点击，调用系统文件管理器打开所在目录并选中文件。
            跨平台处理：Linux 用 xdg-open，macOS 用 open，Windows 用 explorer。
        """
        if (
            event is not None
            and event.button() == Qt.MouseButton.LeftButton
            and self._saved_path is not None
        ):
            import os
            parent_dir: str = os.path.dirname(self._saved_path)
            if not os.path.exists(parent_dir):
                return
            if sys.platform == "linux":
                # 尝试用 dbus 选中文件，失败则打开目录
                try:
                    subprocess.Popen(
                        ["dbus-send", "--print-reply", "--dest=org.freedesktop.FileManager1",
                         "/org/freedesktop/FileManager1",
                         "org.freedesktop.FileManager1.ShowItems",
                         f"array:string:file://{self._saved_path}", "string:"],
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    )
                except FileNotFoundError:
                    subprocess.Popen(["xdg-open", parent_dir])
            elif sys.platform == "darwin":
                subprocess.Popen(["open", "-R", self._saved_path])
            elif sys.platform == "win32":
                subprocess.Popen(["explorer", "/select,", self._saved_path])
            return
        super().mousePressEvent(event)


class TransferPanel(QWidget):
    """
    文件传输管理面板，包含目标设备选择、发送文件按钮和传输任务列表。

    Business Logic（为什么需要这个类）:
        用户需要一个统一的界面来选择目标设备、发送文件、查看所有传输任务
        （发送和接收）的进度和状态，并支持拖拽文件快速发送。

    Code Logic（这个类做什么）:
        顶部是目标设备下拉框和发送文件按钮，中间是可滚动的传输任务列表。
        连接 FileSender 和 FileReceiver 的信号来更新任务进度和状态。
        支持拖拽文件到面板以快速发送。
    """

    def __init__(
        self,
        file_sender: FileSender,
        file_receiver: FileReceiver,
        parent: QWidget | None = None,
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            构建传输面板 UI，绑定发送器和接收器的信号。

        Code Logic（这个函数做什么）:
            创建目标设备下拉框、发送按钮、滚动区域和任务列表布局，
            连接 FileSender/FileReceiver 的进度、完成、失败信号，
            启用拖拽功能。
        """
        super().__init__(parent)
        self._file_sender: FileSender = file_sender
        self._file_receiver: FileReceiver = file_receiver
        self._task_widgets: dict[str, TransferItemWidget] = {}

        self._setup_ui()
        self._connect_signals()
        self.setAcceptDrops(True)

    def _setup_ui(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            将面板的 UI 构建逻辑独立出来，保持 __init__ 清晰。

        Code Logic（这个函数做什么）:
            创建顶部操作栏（设备选择+发送按钮）和可滚动的传输列表区域。
        """
        main_layout: QVBoxLayout = QVBoxLayout(self)
        main_layout.setContentsMargins(0, 0, 0, 0)
        main_layout.setSpacing(12)

        # 顶部操作栏
        top_bar: QHBoxLayout = QHBoxLayout()
        top_bar.setSpacing(12)

        self._device_label: QLabel = QLabel("目标设备:")
        self._device_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; color: {theme.TEXT_PRIMARY}; font-weight: 600;"
        )
        top_bar.addWidget(self._device_label)

        self._device_combo: QComboBox = QComboBox()
        self._device_combo.setMinimumWidth(200)
        self._device_combo.setPlaceholderText("请选择设备...")
        self._device_combo.setStyleSheet(theme.combo_style())
        top_bar.addWidget(self._device_combo, stretch=1)

        self._send_btn: QPushButton = QPushButton("发送文件")
        self._send_btn.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self._send_btn.setStyleSheet(theme.button_primary_style())
        self._send_btn.clicked.connect(self._on_send_file)
        top_bar.addWidget(self._send_btn)

        main_layout.addLayout(top_bar)

        # 传输列表（可滚动区域）
        self._scroll_area: QScrollArea = QScrollArea()
        self._scroll_area.setWidgetResizable(True)
        self._scroll_area.setHorizontalScrollBarPolicy(
            Qt.ScrollBarPolicy.ScrollBarAlwaysOff
        )
        self._scroll_area.setStyleSheet(theme.scroll_area_style())

        self._list_container: QWidget = QWidget()
        self._list_layout: QVBoxLayout = QVBoxLayout(self._list_container)
        self._list_layout.setContentsMargins(0, 0, 0, 0)
        self._list_layout.setSpacing(10)

        # 空提示标签
        self._empty_label: QLabel = QLabel("暂无传输任务\n拖拽文件到此处或点击「发送文件」开始传输")
        self._empty_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._empty_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; color: {theme.TEXT_TERTIARY}; padding: 40px;"
        )
        self._list_layout.addWidget(self._empty_label)
        self._list_layout.addStretch()

        self._scroll_area.setWidget(self._list_container)
        main_layout.addWidget(self._scroll_area)

    def _reapply_styles(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            主题切换时，面板内的控件样式需要重新应用以匹配新的配色方案，
            确保用户在浅色/深色模式之间切换时 UI 表现一致。

        Code Logic（这个函数做什么）:
            重新设置 device_label、device_combo、send_btn 和 empty_label 的样式，
            使用 theme 模块的最新颜色值生成 QSS。
        """
        self._device_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; color: {theme.TEXT_PRIMARY}; font-weight: 600;"
        )
        self._device_combo.setStyleSheet(theme.combo_style())
        self._send_btn.setStyleSheet(theme.button_primary_style())
        self._empty_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; color: {theme.TEXT_TERTIARY}; padding: 40px;"
        )

    def _connect_signals(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            需要监听发送器和接收器的信号以实时更新 UI 中任务的进度和状态。

        Code Logic（这个函数做什么）:
            连接 FileSender 和 FileReceiver 的 progress_updated、
            transfer_completed、transfer_failed 信号到对应的槽函数。
        """
        self._file_sender.progress_updated.connect(self._on_progress)
        self._file_sender.transfer_completed.connect(self._on_completed)
        self._file_sender.transfer_failed.connect(self._on_failed)

        self._file_receiver.transfer_initiated.connect(self._on_receive_initiated)
        self._file_receiver.progress_updated.connect(self._on_progress)
        self._file_receiver.transfer_completed.connect(self._on_receive_completed)
        self._file_receiver.transfer_failed.connect(self._on_failed)

    def update_devices(self, devices: dict[str, Device]) -> None:
        """
        Business Logic（为什么需要这个函数）:
            当设备发现模块检测到设备上线/下线时，需要同步更新目标设备下拉框
            的可选项，确保用户只能选择在线设备。

        Code Logic（这个函数做什么）:
            清空并重新填充 QComboBox，每个 item 的 displayText 为设备名称(IP:端口)，
            userData 存储 (device_id, base_url) 元组。
        """
        current_data: tuple[str, str] | None = self._device_combo.currentData()
        self._device_combo.clear()

        for device_id, device in devices.items():
            display_text: str = f"{device.name} ({device.host}:{device.port})"
            self._device_combo.addItem(display_text, (device_id, device.base_url()))

        # 尝试恢复之前选中的设备
        if current_data is not None:
            for i in range(self._device_combo.count()):
                if self._device_combo.itemData(i) == current_data:
                    self._device_combo.setCurrentIndex(i)
                    break

    def _on_send_file(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击「发送文件」按钮时，需要选择文件并发起异步传输。

        Code Logic（这个函数做什么）:
            检查是否选择了目标设备，弹出文件选择对话框获取文件路径，
            调用 asyncio.ensure_future 启动异步发送任务。
        """
        device_data: tuple[str, str] | None = self._device_combo.currentData()
        if device_data is None:
            return

        file_path, _ = QFileDialog.getOpenFileName(
            self, "选择要发送的文件", "", "所有文件 (*)"
        )
        if not file_path:
            return

        peer_device_id: str = device_data[0]
        peer_base_url: str = device_data[1]
        self._start_send(file_path, peer_base_url, peer_device_id)

    def _start_send(
        self, file_path: str, peer_base_url: str, peer_device_id: str
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            将文件发送的异步调用封装为独立方法，供按钮点击和拖拽共用。

        Code Logic（这个函数做什么）:
            调用 FileSender.send_file 发起异步传输，在 Future 的回调中
            将返回的 TransferTask 添加到列表。
        """

        async def _do_send() -> None:
            """
            Business Logic（为什么需要这个函数）:
                异步执行文件发送并将返回的任务添加到 UI 列表。

            Code Logic（这个函数做什么）:
                await send_file 获取 TransferTask，然后调用 _add_task_widget。
                send_file 内部会 emit progress/completed/failed 信号，
                但 widget 可能在信号之后才创建，所以此处以 task 最终状态为准。
            """
            task: TransferTask = await self._file_sender.send_file(
                file_path, peer_base_url, peer_device_id
            )
            if task.id not in self._task_widgets:
                self._add_task_widget(task)

        asyncio.ensure_future(_do_send())

    def _add_task_widget(self, task: TransferTask) -> None:
        """
        Business Logic（为什么需要这个函数）:
            新的传输任务需要在列表中创建对应的 UI 组件。

        Code Logic（这个函数做什么）:
            创建 TransferItemWidget 并插入到列表布局的顶部（最新的在最上面），
            连接取消按钮信号，隐藏空提示标签。
        """
        if task.id in self._task_widgets:
            return

        self._empty_label.hide()

        item_widget: TransferItemWidget = TransferItemWidget(task)
        item_widget.cancel_clicked.connect(self._on_cancel)
        self._task_widgets[task.id] = item_widget

        # 插入到 stretch 之前（即列表顶部位置）
        # 列表布局: [item1, item2, ..., stretch]
        insert_index: int = self._list_layout.count() - 1  # stretch 之前
        if insert_index < 0:
            insert_index = 0
        self._list_layout.insertWidget(insert_index, item_widget)

    def _on_progress(self, transfer_id: str, progress: float) -> None:
        """
        Business Logic（为什么需要这个函数）:
            传输进度更新时需要实时刷新对应任务项的进度条和大小显示。

        Code Logic（这个函数做什么）:
            根据 transfer_id 查找对应的 TransferItemWidget，调用其 update_progress。
        """
        widget: TransferItemWidget | None = self._task_widgets.get(transfer_id)
        if widget is not None:
            widget.update_progress(progress)

    def _on_completed(self, transfer_id: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            发送完成时需要更新任务项状态为已完成。

        Code Logic（这个函数做什么）:
            查找对应的 widget 并更新状态和进度到 100%。
        """
        widget: TransferItemWidget | None = self._task_widgets.get(transfer_id)
        if widget is not None:
            widget.update_progress(1.0)
            widget.update_status(TransferStatus.COMPLETED)

    def _on_receive_completed(self, transfer_id: str, saved_path: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            接收完成时需要更新 UI 状态，并记录文件保存路径供用户点击打开。

        Code Logic（这个函数做什么）:
            更新 widget 状态为 COMPLETED，将保存路径设置到 widget 上，
            用户可以左键点击卡片打开文件所在目录。
        """
        widget: TransferItemWidget | None = self._task_widgets.get(transfer_id)
        if widget is not None:
            widget.update_status(TransferStatus.COMPLETED)
            widget.set_saved_path(saved_path)

    def _on_failed(self, transfer_id: str, error: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            传输失败时需要更新任务项状态为失败并展示错误信息。

        Code Logic（这个函数做什么）:
            查找对应的 widget，更新状态为 FAILED。
            错误详情可通过 tooltip 展示。
        """
        widget: TransferItemWidget | None = self._task_widgets.get(transfer_id)
        if widget is not None:
            widget.update_status(TransferStatus.FAILED)
            widget.setToolTip(f"错误: {error}")

    def _on_cancel(self, transfer_id: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击取消按钮时需要通知发送器或接收器停止传输。

        Code Logic（这个函数做什么）:
            同时调用 FileSender 和 FileReceiver 的 cancel 方法
            （两者中只有持有该任务的一方会实际生效），并更新 UI 状态。
        """
        self._file_sender.cancel(transfer_id)
        self._file_receiver.cancel(transfer_id)

        widget: TransferItemWidget | None = self._task_widgets.get(transfer_id)
        if widget is not None:
            widget.update_status(TransferStatus.CANCELLED)

    def _on_receive_initiated(self, task: object) -> None:
        """
        Business Logic（为什么需要这个函数）:
            对端发起文件传输时，接收端需要在 UI 上显示新的接收任务。

        Code Logic（这个函数做什么）:
            接收 FileReceiver.transfer_initiated 信号传来的 TransferTask，
            调用 _add_task_widget 创建 UI 卡片。
        """
        from claude_partner.models.transfer import TransferTask as TT
        if isinstance(task, TT):
            self._add_task_widget(task)

    def add_receive_task(self, task: TransferTask) -> None:
        """
        Business Logic（为什么需要这个函数）:
            当接收器初始化一个新的接收任务时，需要在传输列表中显示该任务。

        Code Logic（这个函数做什么）:
            调用 _add_task_widget 将接收任务添加到列表。
        """
        self._add_task_widget(task)

    # -- 拖拽支持 --

    def dragEnterEvent(self, event: QDragEnterEvent | None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户拖拽文件进入面板时需要判断是否接受该拖拽操作。

        Code Logic（这个函数做什么）:
            检查拖拽数据是否包含文件 URL，如果是则接受拖拽操作。
        """
        if event is not None and event.mimeData() is not None and event.mimeData().hasUrls():
            event.acceptProposedAction()
        else:
            super().dragEnterEvent(event)

    def dropEvent(self, event: QDropEvent | None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户拖拽文件到面板并释放时，自动发送文件到选中的目标设备。

        Code Logic（这个函数做什么）:
            从 mimeData 提取文件路径列表，检查是否已选择目标设备，
            逐个发起文件发送任务。
        """
        if event is None or event.mimeData() is None:
            super().dropEvent(event)
            return

        device_data: tuple[str, str] | None = self._device_combo.currentData()
        if device_data is None:
            return

        peer_device_id: str = device_data[0]
        peer_base_url: str = device_data[1]

        for url in event.mimeData().urls():
            file_path: str = url.toLocalFile()
            if file_path:
                self._start_send(file_path, peer_base_url, peer_device_id)

        event.acceptProposedAction()
