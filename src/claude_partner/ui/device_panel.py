# -*- coding: utf-8 -*-
"""设备列表面板：展示局域网中发现的在线设备。"""

from __future__ import annotations

from PyQt6.QtWidgets import (
    QWidget,
    QVBoxLayout,
    QHBoxLayout,
    QLabel,
    QFrame,
    QScrollArea,
)
from PyQt6.QtCore import Qt

from claude_partner.models.device import Device
from claude_partner.ui import theme


class DeviceCard(QFrame):
    """
    设备卡片，展示单个设备的名称、网络地址和在线状态。

    Business Logic（为什么需要这个类）:
        设备列表中每个设备需要以卡片形式直观展示其关键信息（名称、IP:端口）
        和在线状态指示灯，方便用户快速识别可用设备。

    Code Logic（这个类做什么）:
        使用 QFrame 容器，水平布局排列：设备名称/地址标签和在线状态指示灯。
        样式与 PromptCard 一致的扁平圆角边框和 hover 效果。
    """

    def __init__(self, device: Device, parent: QWidget | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            根据传入的 Device 数据构建设备卡片的完整 UI。

        Code Logic（这个函数做什么）:
            保存 device_id 和在线状态，创建设备名称标签、地址标签和在线状态指示灯。
        """
        super().__init__(parent)
        self._device_id: str = device.id
        self._online: bool = device.online

        self.setFrameShape(QFrame.Shape.StyledPanel)
        self.setStyleSheet(
            f"""
            DeviceCard {{
                {theme.card_style()}
            }}
            DeviceCard:hover {{
                background: {theme.BG_SECONDARY};
            }}
            """
        )
        theme.apply_shadow(self)

        main_layout: QHBoxLayout = QHBoxLayout(self)
        main_layout.setContentsMargins(16, 12, 16, 12)
        main_layout.setSpacing(14)

        # 在线状态指示灯
        self._status_dot: QLabel = QLabel()
        self._status_dot.setFixedSize(12, 12)
        self._update_status_dot(device.online)
        main_layout.addWidget(self._status_dot)

        # 设备信息区（名称 + 地址）
        info_layout: QVBoxLayout = QVBoxLayout()
        info_layout.setSpacing(4)

        self._name_label: QLabel = QLabel(device.name)
        self._name_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; font-weight: 600; color: {theme.TEXT_PRIMARY}; "
            f"background: transparent; border: none;"
        )
        info_layout.addWidget(self._name_label)

        self._addr_label: QLabel = QLabel(f"{device.host}:{device.port}")
        self._addr_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_CAPTION}; color: {theme.TEXT_SECONDARY}; "
            f"background: transparent; border: none;"
        )
        info_layout.addWidget(self._addr_label)

        main_layout.addLayout(info_layout, stretch=1)

        # 在线/离线文字标签
        status_text: str = "在线" if device.online else "离线"
        status_color: str = theme.GREEN if device.online else theme.TEXT_TERTIARY
        self._status_text: QLabel = QLabel(status_text)
        self._status_text.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_CAPTION}; font-weight: 600; color: {status_color}; "
            f"background: transparent; border: none;"
        )
        self._status_text.setAlignment(
            Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter
        )
        main_layout.addWidget(self._status_text)

    def _reapply_styles(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            系统主题切换时设备卡片的颜色需要同步更新，否则文字和背景色不匹配。

        Code Logic（这个函数做什么）:
            重新应用卡片边框/背景、名称标签、地址标签、状态指示灯和状态文字的样式。
        """
        self.setStyleSheet(
            f"""
            DeviceCard {{
                {theme.card_style()}
            }}
            DeviceCard:hover {{
                background: {theme.BG_SECONDARY};
            }}
            """
        )
        self._name_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; font-weight: 600; color: {theme.TEXT_PRIMARY}; "
            f"background: transparent; border: none;"
        )
        self._addr_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_CAPTION}; color: {theme.TEXT_SECONDARY}; "
            f"background: transparent; border: none;"
        )
        self._update_status_dot(self._online)
        status_text: str = "在线" if self._online else "离线"
        status_color: str = theme.GREEN if self._online else theme.TEXT_TERTIARY
        self._status_text.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_CAPTION}; font-weight: 600; color: {status_color}; "
            f"background: transparent; border: none;"
        )

    def _update_status_dot(self, online: bool) -> None:
        """
        Business Logic（为什么需要这个函数）:
            在线状态指示灯需要根据设备是否在线切换颜色（绿色/灰色）。

        Code Logic（这个函数做什么）:
            设置圆形 QLabel 的背景色和边框半径，在线时为绿色，离线时为灰色。
        """
        color: str = theme.GREEN if online else theme.TEXT_TERTIARY
        self._status_dot.setStyleSheet(
            f"""
            background-color: {color};
            border-radius: 6px;
            border: none;
            """
        )

    def update_device(self, device: Device) -> None:
        """
        Business Logic（为什么需要这个函数）:
            设备信息（名称、地址、在线状态）可能发生变化，需要更新卡片显示。

        Code Logic（这个函数做什么）:
            更新名称、地址、在线状态指示灯和状态文字。
        """
        self._name_label.setText(device.name)
        self._addr_label.setText(f"{device.host}:{device.port}")
        self._update_status_dot(device.online)

        status_text: str = "在线" if device.online else "离线"
        status_color: str = theme.GREEN if device.online else theme.TEXT_TERTIARY
        self._status_text.setText(status_text)
        self._status_text.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_CAPTION}; font-weight: 600; color: {status_color}; "
            f"background: transparent; border: none;"
        )

    @property
    def device_id(self) -> str:
        """
        Business Logic（为什么需要这个函数）:
            外部需要获取此卡片对应的设备 ID。

        Code Logic（这个函数做什么）:
            返回 device_id 字符串。
        """
        return self._device_id


class DevicePanel(QWidget):
    """
    设备列表面板，展示局域网中发现的所有在线设备。

    Business Logic（为什么需要这个类）:
        用户需要一个面板查看局域网中有哪些 Claude Partner 设备在线，
        以便了解当前的网络协作状况。设备发现模块发现新设备或检测到
        设备离线时自动更新此面板。

    Code Logic（这个类做什么）:
        顶部标题显示「在线设备」和设备计数，下方是可滚动的设备卡片列表。
        无设备时显示提示文字。提供 add_device 和 remove_device 方法
        供 DeviceDiscovery 信号连接。
    """

    def __init__(self, parent: QWidget | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            构建设备列表面板 UI。

        Code Logic（这个函数做什么）:
            创建标题栏、可滚动的设备卡片列表区域和空提示标签。
        """
        super().__init__(parent)
        self._device_cards: dict[str, DeviceCard] = {}
        self._setup_ui()

    def _setup_ui(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            将面板的 UI 构建逻辑独立出来，保持 __init__ 清晰。

        Code Logic（这个函数做什么）:
            创建标题行（标题+计数标签）和可滚动的卡片列表区域。
        """
        main_layout: QVBoxLayout = QVBoxLayout(self)
        main_layout.setContentsMargins(0, 0, 0, 0)
        main_layout.setSpacing(12)

        # 标题行
        header_layout: QHBoxLayout = QHBoxLayout()
        header_layout.setContentsMargins(0, 0, 0, 4)

        self._title_label: QLabel = QLabel("在线设备")
        self._title_label.setStyleSheet(theme.label_title_style())
        header_layout.addWidget(self._title_label)

        self._count_label: QLabel = QLabel("(0)")
        self._count_label.setStyleSheet(theme.label_caption_style())
        header_layout.addWidget(self._count_label)

        header_layout.addStretch()
        main_layout.addLayout(header_layout)

        # 设备卡片列表（可滚动区域）
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
        self._empty_label: QLabel = QLabel(
            "暂无发现其他设备\n请确保在同一局域网"
        )
        self._empty_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._empty_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; color: {theme.TEXT_TERTIARY}; padding: 30px;"
        )
        self._list_layout.addWidget(self._empty_label)
        self._list_layout.addStretch()

        self._scroll_area.setWidget(self._list_container)
        main_layout.addWidget(self._scroll_area)

    def _reapply_styles(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            系统主题切换时设备面板的标题、计数和空提示样式需要同步更新。

        Code Logic（这个函数做什么）:
            重新应用标题标签、计数标签、空提示标签和所有设备卡片的样式。
        """
        self._title_label.setStyleSheet(theme.label_title_style())
        self._count_label.setStyleSheet(theme.label_caption_style())
        self._empty_label.setStyleSheet(
            f"font-size: {theme.FONT_SIZE_BODY}; color: {theme.TEXT_TERTIARY}; padding: 30px;"
        )
        for card in self._device_cards.values():
            card._reapply_styles()

    def add_device(self, device: Device) -> None:
        """
        Business Logic（为什么需要这个函数）:
            当 DeviceDiscovery 发现新设备时，需要在列表中添加对应的设备卡片。

        Code Logic（这个函数做什么）:
            如果设备已存在则更新卡片信息，否则创建新的 DeviceCard 并插入列表。
            更新设备计数显示，隐藏空提示标签。
        """
        if device.id in self._device_cards:
            self._device_cards[device.id].update_device(device)
            return

        self._empty_label.hide()

        card: DeviceCard = DeviceCard(device)
        self._device_cards[device.id] = card

        # 插入到 stretch 之前
        insert_index: int = self._list_layout.count() - 1
        if insert_index < 0:
            insert_index = 0
        self._list_layout.insertWidget(insert_index, card)

        self._update_count()

    def remove_device(self, device_id: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            当 DeviceDiscovery 检测到设备离线时，需要从列表中移除对应的设备卡片。

        Code Logic（这个函数做什么）:
            从字典中取出对应的 DeviceCard，从布局中移除并销毁。
            如果列表为空则重新显示空提示标签。
        """
        card: DeviceCard | None = self._device_cards.pop(device_id, None)
        if card is None:
            return

        self._list_layout.removeWidget(card)
        card.deleteLater()

        self._update_count()

        if len(self._device_cards) == 0:
            self._empty_label.show()

    def get_device_count(self) -> int:
        """
        Business Logic（为什么需要这个函数）:
            系统托盘需要获取当前在线设备数量来更新提示文字。

        Code Logic（这个函数做什么）:
            返回设备卡片字典的长度。
        """
        return len(self._device_cards)

    def _update_count(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            设备数量变化时需要更新标题旁的计数显示。

        Code Logic（这个函数做什么）:
            更新计数标签的文字为当前设备数量。
        """
        self._count_label.setText(f"({len(self._device_cards)})")
