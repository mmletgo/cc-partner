# -*- coding: utf-8 -*-
"""标签输入/展示组件：支持交互式标签添加、删除和展示。"""

from PyQt6.QtWidgets import (
    QWidget,
    QHBoxLayout,
    QVBoxLayout,
    QLineEdit,
    QLabel,
    QPushButton,
    QLayout,
    QSizePolicy,
)
from PyQt6.QtCore import pyqtSignal, Qt, QRect, QSize, QPoint
from PyQt6.QtGui import QColor

from claude_partner.ui import theme


class FlowLayout(QLayout):
    """
    流式布局：子组件从左到右排列，空间不足时自动换行。

    Business Logic（为什么需要这个类）:
        标签数量不固定，水平排列时需要在一行放不下时自动换行，
        Qt 内置布局不直接支持此特性，因此需要自定义布局。

    Code Logic（这个类做什么）:
        继承 QLayout，重写 addItem / count / itemAt / takeAt / setGeometry /
        sizeHint / minimumSize 等方法，实现从左到右、自动折行的布局逻辑。
    """

    def __init__(self, parent: QWidget | None = None, spacing: int = 6) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化流式布局，设置组件间距。

        Code Logic（这个函数做什么）:
            调用父类初始化，保存布局项列表和间距参数。
        """
        super().__init__(parent)
        self._items: list = []
        self._spacing: int = spacing

    def addItem(self, item) -> None:  # type: ignore[override]
        """
        Business Logic（为什么需要这个函数）:
            QLayout 接口要求：添加布局项。

        Code Logic（这个函数做什么）:
            将布局项追加到内部列表。
        """
        self._items.append(item)

    def count(self) -> int:
        """
        Business Logic（为什么需要这个函数）:
            QLayout 接口要求：返回布局项数量。

        Code Logic（这个函数做什么）:
            返回内部列表长度。
        """
        return len(self._items)

    def itemAt(self, index: int):  # type: ignore[override]
        """
        Business Logic（为什么需要这个函数）:
            QLayout 接口要求：按索引获取布局项。

        Code Logic（这个函数做什么）:
            返回对应索引的布局项，越界返回 None。
        """
        if 0 <= index < len(self._items):
            return self._items[index]
        return None

    def takeAt(self, index: int):  # type: ignore[override]
        """
        Business Logic（为什么需要这个函数）:
            QLayout 接口要求：移除并返回指定索引的布局项。

        Code Logic（这个函数做什么）:
            从列表中弹出指定索引的布局项。
        """
        if 0 <= index < len(self._items):
            return self._items.pop(index)
        return None

    def hasHeightForWidth(self) -> bool:
        """
        Business Logic（为什么需要这个函数）:
            告知 Qt 布局系统此布局的高度取决于宽度（流式布局的特性）。

        Code Logic（这个函数做什么）:
            返回 True 表示布局高度依赖于可用宽度。
            这是 QScrollArea 正确计算滚动区域高度所必需的。
        """
        return True

    def heightForWidth(self, width: int) -> int:
        """
        Business Logic（为什么需要这个函数）:
            QScrollArea 需要根据可用宽度计算布局总高度来设置滚动区域大小。

        Code Logic（这个函数做什么）:
            模拟 _do_layout 的排列逻辑，计算给定宽度下的总高度。
        """
        return self._do_layout(QRect(0, 0, width, 0), test_only=True)

    def setGeometry(self, rect: QRect) -> None:
        """
        Business Logic（为什么需要这个函数）:
            QLayout 接口要求：当布局区域变化时重新排列所有子组件。

        Code Logic（这个函数做什么）:
            调用 _do_layout 按照流式规则排列子组件。
            同时更新父容器 widget 的最小高度，使 QScrollArea 能正确滚动。
        """
        super().setGeometry(rect)
        height: int = self._do_layout(rect)
        parent_widget: QWidget | None = self.parentWidget()
        if parent_widget is not None:
            parent_widget.setMinimumHeight(height)

    def sizeHint(self) -> QSize:
        """
        Business Logic（为什么需要这个函数）:
            QLayout 接口要求：返回推荐尺寸。

        Code Logic（这个函数做什么）:
            返回 minimumSize 作为推荐尺寸。
        """
        return self.minimumSize()

    def minimumSize(self) -> QSize:
        """
        Business Logic（为什么需要这个函数）:
            QLayout 接口要求：返回最小尺寸以保证内容可见。

        Code Logic（这个函数做什么）:
            遍历所有子组件取最大宽度和累加高度。
        """
        size: QSize = QSize(0, 0)
        for item in self._items:
            size = size.expandedTo(item.minimumSize())
        return size

    def _do_layout(self, rect: QRect, test_only: bool = False) -> int:
        """
        Business Logic（为什么需要这个函数）:
            实际执行流式布局排列计算。

        Code Logic（这个函数做什么）:
            从左到右放置子组件，当 x + 子组件宽度超出右边界时换行。
            test_only=True 时仅计算高度不实际移动组件（供 heightForWidth 使用）。
            返回布局总高度。
        """
        x: int = rect.x()
        y: int = rect.y()
        line_height: int = 0

        for item in self._items:
            widget: QWidget | None = item.widget()
            if widget is None or widget.isHidden():
                continue
            item_size: QSize = item.sizeHint()
            next_x: int = x + item_size.width() + self._spacing

            if next_x - self._spacing > rect.right() and line_height > 0:
                x = rect.x()
                y = y + line_height + self._spacing
                next_x = x + item_size.width() + self._spacing
                line_height = 0

            if not test_only:
                item.setGeometry(QRect(QPoint(x, y), item_size))
            x = next_x
            line_height = max(line_height, item_size.height())

        return y + line_height - rect.y()


class TagLabel(QWidget):
    """
    单个标签 pill 组件：彩色圆角背景 + 文字 + 删除按钮。

    Business Logic（为什么需要这个类）:
        标签需要以直观的 pill 样式展示，用户可以点击 x 按钮移除标签。

    Code Logic（这个类做什么）:
        水平布局包含一个 QLabel 显示标签文字和一个 QPushButton (x) 用于触发删除。
        使用 QSS 设置圆角背景色。
    """

    remove_clicked: pyqtSignal = pyqtSignal(str)

    _color_index: int = 0

    def __init__(self, tag: str, parent: QWidget | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            创建一个可视化的标签 pill，显示标签文本并提供删除功能。

        Code Logic（这个函数做什么）:
            初始化水平布局，添加文字标签和 x 按钮，
            根据颜色列表循环分配背景色和文字色。
        """
        super().__init__(parent)
        self._tag: str = tag

        colors: list[tuple[str, str]] = theme.current_tag_colors()
        bg_color, text_color = colors[TagLabel._color_index % len(colors)]
        TagLabel._color_index += 1

        layout: QHBoxLayout = QHBoxLayout(self)
        layout.setContentsMargins(8, 2, 4, 2)
        layout.setSpacing(2)

        label: QLabel = QLabel(tag)
        label.setStyleSheet(f"color: {text_color}; font-size: {theme.FONT_SIZE_SMALL}; background: transparent; border: none;")
        layout.addWidget(label)

        btn_remove: QPushButton = QPushButton("\u00d7")
        btn_remove.setFixedSize(16, 16)
        btn_remove.setCursor(Qt.CursorShape.PointingHandCursor)
        btn_remove.setStyleSheet(
            f"""
            QPushButton {{
                border: none;
                background: transparent;
                color: {text_color};
                font-size: {theme.FONT_SIZE_BODY};
                font-weight: bold;
                padding: 0px;
            }}
            QPushButton:hover {{
                color: {theme.RED};
            }}
            """
        )
        btn_remove.clicked.connect(lambda: self.remove_clicked.emit(self._tag))
        layout.addWidget(btn_remove)

        self.setStyleSheet(
            f"""
            TagLabel {{
                background-color: {bg_color};
                border-radius: {theme.RADIUS_SMALL};
            }}
            """
        )
        self.setFixedHeight(24)

    @property
    def tag(self) -> str:
        """
        Business Logic（为什么需要这个函数）:
            外部需要获取此标签组件对应的标签文本。

        Code Logic（这个函数做什么）:
            返回标签文本字符串。
        """
        return self._tag


class TagWidget(QWidget):
    """
    标签编辑组件：显示已有标签列表 + 输入框添加新标签。

    Business Logic（为什么需要这个类）:
        Prompt 的标签管理需要一个综合组件，既能展示现有标签（可删除），
        又能通过输入框添加新标签。

    Code Logic（这个类做什么）:
        上部使用 FlowLayout 展示 TagLabel 列表，
        下部使用 QLineEdit 输入新标签（Enter 添加）。
        标签变更时发射 tags_changed 信号。
    """

    tags_changed: pyqtSignal = pyqtSignal(list)

    def __init__(self, parent: QWidget | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化标签编辑组件的 UI 布局。

        Code Logic（这个函数做什么）:
            创建垂直布局，包含流式标签展示区和底部输入框。
        """
        super().__init__(parent)
        self._tags: list[str] = []
        self._tag_labels: list[TagLabel] = []

        main_layout: QVBoxLayout = QVBoxLayout(self)
        main_layout.setContentsMargins(0, 0, 0, 0)
        main_layout.setSpacing(6)

        # 标签展示区域
        self._tags_container: QWidget = QWidget()
        self._flow_layout: FlowLayout = FlowLayout(self._tags_container, spacing=6)
        self._tags_container.setLayout(self._flow_layout)
        main_layout.addWidget(self._tags_container)

        # 输入框
        self._input: QLineEdit = QLineEdit()
        self._input.setPlaceholderText("输入标签后按 Enter 添加...")
        self._input.setStyleSheet(theme.input_style())
        self._input.returnPressed.connect(self._on_enter_pressed)
        main_layout.addWidget(self._input)

    def set_tags(self, tags: list[str]) -> None:
        """
        Business Logic（为什么需要这个函数）:
            外部需要批量设置标签列表，例如编辑 Prompt 时加载已有标签。

        Code Logic（这个函数做什么）:
            清空现有标签展示，用新的标签列表重新构建 TagLabel 组件。
        """
        self._clear_tag_labels()
        self._tags = list(tags)
        for tag in self._tags:
            self._create_tag_label(tag)

    def get_tags(self) -> list[str]:
        """
        Business Logic（为什么需要这个函数）:
            外部需要获取当前的标签列表，例如保存 Prompt 时。

        Code Logic（这个函数做什么）:
            先将输入框中未提交的文本自动加入标签列表（用户可能输入了标签
            但没按 Enter 就点了确定），然后返回当前标签列表的副本。
        """
        pending: str = self._input.text().strip()
        if pending:
            self._add_tag(pending)
            self._input.clear()
        return list(self._tags)

    def _add_tag(self, tag: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户输入新标签后需要添加到列表中并更新展示。

        Code Logic（这个函数做什么）:
            检查标签是否已存在（去重），添加到列表并创建对应的 TagLabel。
            发射 tags_changed 信号。
        """
        tag = tag.strip()
        if not tag or tag in self._tags:
            return
        self._tags.append(tag)
        self._create_tag_label(tag)
        self.tags_changed.emit(list(self._tags))

    def _remove_tag(self, tag: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户点击标签上的 x 按钮后需要移除该标签。

        Code Logic（这个函数做什么）:
            从标签列表中移除目标标签，删除对应的 TagLabel 组件。
            发射 tags_changed 信号。
        """
        if tag not in self._tags:
            return
        self._tags.remove(tag)
        for tag_label in self._tag_labels:
            if tag_label.tag == tag:
                self._flow_layout.removeWidget(tag_label)
                tag_label.deleteLater()
                self._tag_labels.remove(tag_label)
                break
        self.tags_changed.emit(list(self._tags))

    def _on_enter_pressed(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户在输入框中按下 Enter 时触发添加标签。

        Code Logic（这个函数做什么）:
            读取输入框文本，调用 _add_tag 添加标签，清空输入框。
        """
        text: str = self._input.text().strip()
        if text:
            self._add_tag(text)
            self._input.clear()

    def _create_tag_label(self, tag: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            为每个标签创建可视化的 pill 组件并添加到流式布局中。

        Code Logic（这个函数做什么）:
            实例化 TagLabel，连接其 remove_clicked 信号，
            添加到流式布局和内部列表。
        """
        tag_label: TagLabel = TagLabel(tag, self._tags_container)
        tag_label.remove_clicked.connect(self._remove_tag)
        self._flow_layout.addWidget(tag_label)
        self._tag_labels.append(tag_label)
        tag_label.show()

    def _clear_tag_labels(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            重新设置标签列表前需要清空现有的可视化组件。

        Code Logic（这个函数做什么）:
            遍历所有 TagLabel，从布局中移除并销毁。
        """
        for tag_label in self._tag_labels:
            self._flow_layout.removeWidget(tag_label)
            tag_label.deleteLater()
        self._tag_labels.clear()
