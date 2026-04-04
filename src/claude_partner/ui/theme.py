"""
Apple 风格集中式主题模块，支持浅色/深色模式自动切换。

Business Logic:
    项目 UI 需要统一的 Apple/macOS 视觉风格，包括颜色、字体、间距和组件样式。
    同时需要适配系统深色模式，在深色主题下自动切换颜色。
    将所有主题常量和 QSS 样式函数集中在一个模块中，避免各组件重复定义样式。

Code Logic:
    提供颜色/字体/间距常量、返回 QSS 字符串的组件样式函数、以及阴影等辅助函数。
    通过 apply_theme() 函数切换浅色/深色调色板，所有模块级颜色变量随之更新。
"""

from PyQt6.QtCore import QPointF, Qt
from PyQt6.QtGui import QColor, QFont, QIcon, QPainter, QPixmap
from PyQt6.QtWidgets import QGraphicsDropShadowEffect, QWidget

# ── 浅色调色板（默认）────────────────────────────────────────────────────

_LIGHT_PALETTE: dict[str, str] = {
    "ACCENT": "#007AFF",
    "ACCENT_HOVER": "#0062CC",
    "ACCENT_PRESSED": "#004999",
    "BG_PRIMARY": "#FFFFFF",
    "BG_SECONDARY": "#F5F5F7",
    "BG_TERTIARY": "#E8E8ED",
    "BORDER": "#E5E5EA",
    "BORDER_SUBTLE": "#F0F0F5",
    "TEXT_PRIMARY": "#1D1D1F",
    "TEXT_SECONDARY": "#86868B",
    "TEXT_TERTIARY": "#AEAEB2",
    "GREEN": "#34C759",
    "RED": "#FF3B30",
    "ORANGE": "#FF9500",
    "SHADOW_LIGHT": "rgba(0, 0, 0, 0.04)",
    "SHADOW_MEDIUM": "rgba(0, 0, 0, 0.08)",
    "DANGER_HOVER_BG": "#FFF0F0",
    "SCROLLBAR_HANDLE": "rgba(0, 0, 0, 0.15)",
    "SCROLLBAR_HANDLE_HOVER": "rgba(0, 0, 0, 0.3)",
    "STATUS_BG_TRANSFERRING": "#E8F0FE",
    "STATUS_BG_COMPLETED": "#E6F4EA",
    "STATUS_BG_FAILED": "#FDE7E7",
    "STATUS_BG_CANCELLED": "#FEF7E0",
}

# ── 深色调色板 ─────────────────────────────────────────────────────────────

_DARK_PALETTE: dict[str, str] = {
    "ACCENT": "#0A84FF",
    "ACCENT_HOVER": "#409CFF",
    "ACCENT_PRESSED": "#0066CC",
    "BG_PRIMARY": "#1C1C1E",
    "BG_SECONDARY": "#2C2C2E",
    "BG_TERTIARY": "#3A3A3C",
    "BORDER": "#48484A",
    "BORDER_SUBTLE": "#38383A",
    "TEXT_PRIMARY": "#F5F5F7",
    "TEXT_SECONDARY": "#98989D",
    "TEXT_TERTIARY": "#636366",
    "GREEN": "#30D158",
    "RED": "#FF453A",
    "ORANGE": "#FF9F0A",
    "SHADOW_LIGHT": "rgba(0, 0, 0, 0.16)",
    "SHADOW_MEDIUM": "rgba(0, 0, 0, 0.24)",
    "DANGER_HOVER_BG": "#3D1F1F",
    "SCROLLBAR_HANDLE": "rgba(255, 255, 255, 0.2)",
    "SCROLLBAR_HANDLE_HOVER": "rgba(255, 255, 255, 0.35)",
    "STATUS_BG_TRANSFERRING": "#1A3A5C",
    "STATUS_BG_COMPLETED": "#1A3C2A",
    "STATUS_BG_FAILED": "#3D1A1A",
    "STATUS_BG_CANCELLED": "#3D3319",
}

# ── 当前模式状态 ────────────────────────────────────────────────────────────

_is_dark: bool = False

# ── 初始化模块级变量（浅色默认）──────────────────────────────────────────

ACCENT: str = _LIGHT_PALETTE["ACCENT"]
ACCENT_HOVER: str = _LIGHT_PALETTE["ACCENT_HOVER"]
ACCENT_PRESSED: str = _LIGHT_PALETTE["ACCENT_PRESSED"]

BG_PRIMARY: str = _LIGHT_PALETTE["BG_PRIMARY"]
BG_SECONDARY: str = _LIGHT_PALETTE["BG_SECONDARY"]
BG_TERTIARY: str = _LIGHT_PALETTE["BG_TERTIARY"]

BORDER: str = _LIGHT_PALETTE["BORDER"]
BORDER_SUBTLE: str = _LIGHT_PALETTE["BORDER_SUBTLE"]

TEXT_PRIMARY: str = _LIGHT_PALETTE["TEXT_PRIMARY"]
TEXT_SECONDARY: str = _LIGHT_PALETTE["TEXT_SECONDARY"]
TEXT_TERTIARY: str = _LIGHT_PALETTE["TEXT_TERTIARY"]

GREEN: str = _LIGHT_PALETTE["GREEN"]
RED: str = _LIGHT_PALETTE["RED"]
ORANGE: str = _LIGHT_PALETTE["ORANGE"]

SHADOW_LIGHT: str = _LIGHT_PALETTE["SHADOW_LIGHT"]
SHADOW_MEDIUM: str = _LIGHT_PALETTE["SHADOW_MEDIUM"]

DANGER_HOVER_BG: str = _LIGHT_PALETTE["DANGER_HOVER_BG"]
SCROLLBAR_HANDLE: str = _LIGHT_PALETTE["SCROLLBAR_HANDLE"]
SCROLLBAR_HANDLE_HOVER: str = _LIGHT_PALETTE["SCROLLBAR_HANDLE_HOVER"]

STATUS_BG_TRANSFERRING: str = _LIGHT_PALETTE["STATUS_BG_TRANSFERRING"]
STATUS_BG_COMPLETED: str = _LIGHT_PALETTE["STATUS_BG_COMPLETED"]
STATUS_BG_FAILED: str = _LIGHT_PALETTE["STATUS_BG_FAILED"]
STATUS_BG_CANCELLED: str = _LIGHT_PALETTE["STATUS_BG_CANCELLED"]

# ── 字体和间距 ────────────────────────────────────────────────────────────

FONT_FAMILY: str = (
    "-apple-system, 'SF Pro Display', 'Segoe UI', 'Noto Sans SC', sans-serif"
)
FONT_SIZE_TITLE: str = "17px"
FONT_SIZE_HEADING: str = "15px"
FONT_SIZE_BODY: str = "14px"
FONT_SIZE_CAPTION: str = "12px"
FONT_SIZE_SMALL: str = "11px"

RADIUS_LARGE: str = "12px"
RADIUS_MEDIUM: str = "10px"
RADIUS_SMALL: str = "8px"

# ── 标签色板（Apple 柔和色系：背景色, 文字色）──────────────────────────

TAG_COLORS: list[tuple[str, str]] = [
    ("#E8F0FE", "#1A73E8"),  # 蓝
    ("#E6F4EA", "#137333"),  # 绿
    ("#FEF7E0", "#B06000"),  # 橙
    ("#F3E8FD", "#7627BB"),  # 紫
    ("#E0F7FA", "#00796B"),  # 青
    ("#FDE7E7", "#C5221F"),  # 红
    ("#EDE7F6", "#5E35B1"),  # 深紫
    ("#E1F5FE", "#0277BD"),  # 浅蓝
]

TAG_COLORS_DARK: list[tuple[str, str]] = [
    ("#1A3A5C", "#5AADFF"),  # 蓝
    ("#1A3C2A", "#4CAF50"),  # 绿
    ("#3D3319", "#FFB74D"),  # 橙
    ("#2D1F3D", "#CE93D8"),  # 紫
    ("#1A3D3D", "#4DB6AC"),  # 青
    ("#3D1A1A", "#EF9A9A"),  # 红
    ("#2A1F3D", "#B39DDB"),  # 深紫
    ("#1A2D3D", "#64B5F6"),  # 浅蓝
]


# ── 主题切换 ──────────────────────────────────────────────────────────────


def is_dark_mode() -> bool:
    """
    Business Logic:
        外部组件需要知道当前是否为深色模式，以便做条件判断。

    Code Logic:
        返回模块内部 _is_dark 状态。
    """
    return _is_dark


def apply_theme(dark: bool) -> str:
    """
    Business Logic:
        应用启动时和系统主题切换时需要将所有颜色变量切换为对应的调色板，
        使所有后续的样式函数调用返回正确的颜色。

    Code Logic:
        根据 dark 参数选择深色或浅色调色板，用 globals() 批量更新模块级变量，
        返回最新的全局 QSS 字符串供 QApplication 重新应用。
    """
    global _is_dark
    _is_dark = dark

    palette: dict[str, str] = _DARK_PALETTE if dark else _LIGHT_PALETTE
    for key, value in palette.items():
        globals()[key] = value

    return get_global_stylesheet()


def current_tag_colors() -> list[tuple[str, str]]:
    """
    Business Logic:
        标签颜色需要跟随主题切换，深色模式下使用低饱和度高亮标签颜色。

    Code Logic:
        根据当前 _is_dark 状态返回浅色或深色标签色板。
    """
    return TAG_COLORS_DARK if _is_dark else TAG_COLORS


# ── 组件样式函数 ──────────────────────────────────────────────────────────


def get_global_stylesheet() -> str:
    """
    Business Logic:
        应用启动时需要一次性设置全局 QSS，统一基础字体、滚动条和提示框风格。

    Code Logic:
        返回覆盖 QWidget 字体、QScrollBar（纵向/横向 6px 薄圆角半透明）、QToolTip 的 QSS 字符串。
    """
    return f"""
        QWidget {{
            font-family: {FONT_FAMILY};
            font-size: {FONT_SIZE_BODY};
            color: {TEXT_PRIMARY};
        }}

        /* ── 纵向滚动条 ── */
        QScrollBar:vertical {{
            width: 6px;
            background: transparent;
        }}
        QScrollBar::handle:vertical {{
            background: {SCROLLBAR_HANDLE};
            border-radius: 3px;
            min-height: 30px;
        }}
        QScrollBar::handle:vertical:hover {{
            background: {SCROLLBAR_HANDLE_HOVER};
        }}
        QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {{
            height: 0;
        }}
        QScrollBar::add-page:vertical, QScrollBar::sub-page:vertical {{
            background: transparent;
        }}

        /* ── 横向滚动条 ── */
        QScrollBar:horizontal {{
            height: 6px;
            background: transparent;
        }}
        QScrollBar::handle:horizontal {{
            background: {SCROLLBAR_HANDLE};
            border-radius: 3px;
            min-width: 30px;
        }}
        QScrollBar::handle:horizontal:hover {{
            background: {SCROLLBAR_HANDLE_HOVER};
        }}
        QScrollBar::add-line:horizontal, QScrollBar::sub-line:horizontal {{
            width: 0;
        }}
        QScrollBar::add-page:horizontal, QScrollBar::sub-page:horizontal {{
            background: transparent;
        }}

        /* ── 提示框 ── */
        QToolTip {{
            background: {BG_PRIMARY};
            color: {TEXT_PRIMARY};
            border: 1px solid {BORDER};
            border-radius: 6px;
            padding: 6px 10px;
            font-size: {FONT_SIZE_CAPTION};
        }}
    """


def card_style() -> str:
    """
    Business Logic:
        卡片是 UI 中最常见的容器组件，需要统一的圆角、边框和白色背景。

    Code Logic:
        返回卡片容器的 QSS 字符串。真正的阴影需用 apply_shadow() 函数附加。
    """
    return f"""
        border: 1px solid {BORDER};
        border-radius: {RADIUS_LARGE};
        padding: 16px;
        background: {BG_PRIMARY};
    """


def input_style() -> str:
    """
    Business Logic:
        文本输入框需要统一外观：圆角边框、Apple 风格字体和聚焦高亮。

    Code Logic:
        返回 QLineEdit 和 QTextEdit 的 QSS 字符串，含 :focus 伪状态。
    """
    return f"""
        QLineEdit, QTextEdit {{
            border: 1px solid {BORDER};
            border-radius: {RADIUS_MEDIUM};
            padding: 10px 14px;
            font-size: {FONT_SIZE_BODY};
            font-family: {FONT_FAMILY};
            background: {BG_PRIMARY};
            color: {TEXT_PRIMARY};
        }}
        QLineEdit:focus, QTextEdit:focus {{
            border-color: {ACCENT};
        }}
    """


def button_primary_style() -> str:
    """
    Business Logic:
        主操作按钮（如"保存""确认"）使用蓝色填充风格以突出主要操作。

    Code Logic:
        返回蓝底白字的 QPushButton QSS，含 hover / pressed 状态。
    """
    return f"""
        QPushButton {{
            background: {ACCENT};
            color: white;
            border: none;
            border-radius: {RADIUS_MEDIUM};
            padding: 10px 20px;
            font-size: {FONT_SIZE_BODY};
            font-weight: 600;
            font-family: {FONT_FAMILY};
        }}
        QPushButton:hover {{
            background: {ACCENT_HOVER};
        }}
        QPushButton:pressed {{
            background: {ACCENT_PRESSED};
        }}
    """


def button_secondary_style() -> str:
    """
    Business Logic:
        次要操作按钮（如"取消""编辑"）使用透明背景加边框，视觉层级低于主按钮。

    Code Logic:
        返回透明背景、蓝色文字、灰色边框的 QPushButton QSS。
    """
    return f"""
        QPushButton {{
            background: transparent;
            color: {ACCENT};
            border: 1px solid {BORDER};
            border-radius: {RADIUS_MEDIUM};
            padding: 8px 16px;
            font-size: {FONT_SIZE_CAPTION};
            font-family: {FONT_FAMILY};
        }}
        QPushButton:hover {{
            background: {BG_SECONDARY};
        }}
    """


def button_danger_style() -> str:
    """
    Business Logic:
        危险操作按钮（如"删除"）使用红色文字警示用户。

    Code Logic:
        返回透明背景、红色文字的 QPushButton QSS，hover 时背景微红。
    """
    return f"""
        QPushButton {{
            background: transparent;
            color: {RED};
            border: 1px solid {BORDER};
            border-radius: {RADIUS_MEDIUM};
            padding: 8px 16px;
            font-size: {FONT_SIZE_CAPTION};
            font-family: {FONT_FAMILY};
        }}
        QPushButton:hover {{
            background: {DANGER_HOVER_BG};
        }}
    """


def button_danger_compact_style() -> str:
    """
    Business Logic:
        传输任务卡片中的取消按钮需要紧凑的红色危险按钮，尺寸比标准 danger 按钮更小。

    Code Logic:
        返回小尺寸红色文字 QPushButton QSS，padding 和字号更小，hover 时背景微红。
    """
    return f"""
        QPushButton {{
            background: {BG_PRIMARY};
            color: {RED};
            border: 1px solid {BORDER};
            border-radius: 6px;
            padding: 2px 8px;
            font-size: {FONT_SIZE_SMALL};
            font-family: {FONT_FAMILY};
        }}
        QPushButton:hover {{
            background: {DANGER_HOVER_BG};
            border-color: {RED};
        }}
    """


def combo_style() -> str:
    """
    Business Logic:
        下拉选择框在设备选择、标签筛选等场景中使用，需要完整的 Apple 风格样式。

    Code Logic:
        返回 QComboBox 完整 QSS，包括 drop-down 按钮、箭头和下拉列表视图。
    """
    return f"""
        QComboBox {{
            border: 1px solid {BORDER};
            border-radius: {RADIUS_MEDIUM};
            padding: 8px 14px;
            padding-right: 30px;
            font-size: {FONT_SIZE_BODY};
            font-family: {FONT_FAMILY};
            background: {BG_PRIMARY};
            color: {TEXT_PRIMARY};
        }}
        QComboBox:focus {{
            border-color: {ACCENT};
        }}
        QComboBox::drop-down {{
            subcontrol-origin: padding;
            subcontrol-position: center right;
            width: 30px;
            border: none;
        }}
        QComboBox::down-arrow {{
            image: none;
            width: 0;
            height: 0;
            border-left: 4px solid transparent;
            border-right: 4px solid transparent;
            border-top: 5px solid {TEXT_SECONDARY};
        }}
        QComboBox QAbstractItemView {{
            border: 1px solid {BORDER};
            border-radius: 8px;
            background: {BG_PRIMARY};
            selection-background-color: {BG_SECONDARY};
            selection-color: {TEXT_PRIMARY};
            padding: 4px;
            outline: none;
        }}
        QComboBox QAbstractItemView::item {{
            padding: 8px 14px;
            border-radius: 6px;
            min-height: 32px;
        }}
        QComboBox QAbstractItemView::item:hover {{
            background: {BG_SECONDARY};
        }}
    """


def tab_bar_style() -> str:
    """
    Business Logic:
        Tab 栏是主窗口的核心导航组件，需要模拟 Apple 分段控件的外观。

    Code Logic:
        返回 QTabWidget::pane 和 QTabBar 的 QSS，选中态为白色背景 + 加粗文字。
    """
    return f"""
        QTabWidget::pane {{
            border: none;
        }}
        QTabBar {{
            background: {BG_TERTIARY};
            border-radius: 8px;
            padding: 2px;
        }}
        QTabBar::tab {{
            background: transparent;
            border-radius: 6px;
            padding: 6px 24px;
            margin: 2px;
            min-width: 80px;
            color: {TEXT_SECONDARY};
            font-size: 13px;
            font-family: {FONT_FAMILY};
        }}
        QTabBar::tab:selected {{
            background: {BG_PRIMARY};
            color: {TEXT_PRIMARY};
            font-weight: 600;
        }}
        QTabBar::tab:hover:!selected {{
            color: {TEXT_PRIMARY};
        }}
    """


def scroll_area_style() -> str:
    """
    Business Logic:
        滚动区域作为内容容器不应有可见边框或背景色。

    Code Logic:
        返回 QScrollArea 的透明无边框 QSS。
    """
    return """
        QScrollArea {
            border: none;
            background: transparent;
        }
    """


def progress_bar_style() -> str:
    """
    Business Logic:
        文件传输等场景需要进度条，使用 Apple 蓝色渐变风格。

    Code Logic:
        返回 QProgressBar 的 QSS，含圆角容器和蓝色渐变 chunk。
    """
    return f"""
        QProgressBar {{
            border: none;
            border-radius: 4px;
            background: {BG_TERTIARY};
            text-align: center;
            font-size: {FONT_SIZE_SMALL};
            color: {TEXT_SECONDARY};
            height: 8px;
        }}
        QProgressBar::chunk {{
            border-radius: 4px;
            background: qlineargradient(
                x1:0, y1:0, x2:1, y2:0,
                stop:0 {ACCENT}, stop:1 #5AC8FA
            );
        }}
    """


def tag_label_style(bg: str, fg: str) -> str:
    """
    Business Logic:
        标签需要按不同分类显示不同的柔和配色。

    Code Logic:
        接收背景色和文字色，返回圆角标签 QSS。
    """
    return f"""
        border-radius: {RADIUS_SMALL};
        padding: 3px 10px;
        font-size: {FONT_SIZE_SMALL};
        background-color: {bg};
        color: {fg};
        border: none;
    """


def dialog_style() -> str:
    """
    Business Logic:
        弹窗对话框需要统一的白色背景和圆角样式。

    Code Logic:
        返回 QDialog 的 QSS 字符串。
    """
    return f"""
        QDialog {{
            background: {BG_PRIMARY};
            border-radius: {RADIUS_LARGE};
        }}
    """


def label_title_style() -> str:
    """
    Business Logic:
        标题级文字（如面板标题）需要较大字号和加粗。

    Code Logic:
        返回 QLabel 标题样式 QSS。
    """
    return f"""
        font-size: {FONT_SIZE_TITLE};
        font-weight: 700;
        font-family: {FONT_FAMILY};
        color: {TEXT_PRIMARY};
    """


def label_body_style() -> str:
    """
    Business Logic:
        正文级文字用于一般内容展示。

    Code Logic:
        返回 QLabel 正文样式 QSS。
    """
    return f"""
        font-size: {FONT_SIZE_BODY};
        font-family: {FONT_FAMILY};
        color: {TEXT_PRIMARY};
    """


def label_caption_style() -> str:
    """
    Business Logic:
        辅助说明文字（如时间戳、计数）使用较小字号和浅色。

    Code Logic:
        返回 QLabel 辅助文字样式 QSS。
    """
    return f"""
        font-size: {FONT_SIZE_CAPTION};
        font-family: {FONT_FAMILY};
        color: {TEXT_SECONDARY};
    """


# ── 辅助函数 ──────────────────────────────────────────────────────────────


def create_app_icon(size: int = 64) -> QIcon:
    """
    Business Logic（为什么需要这个函数）:
        应用的窗口图标和托盘图标（非 macOS）共用同一视觉样式，
        集中在 theme 模块避免重复绘制代码。

    Code Logic（这个函数做什么）:
        创建指定尺寸的 QPixmap，绘制蓝色圆形背景 + 白色 "CP" 文字，
        返回 QIcon。
    """
    pixmap: QPixmap = QPixmap(size, size)
    pixmap.fill(QColor(0, 0, 0, 0))

    painter: QPainter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing, True)

    # 蓝色圆形背景
    painter.setPen(Qt.PenStyle.NoPen)
    painter.setBrush(QColor(ACCENT))
    painter.drawEllipse(2, 2, size - 4, size - 4)

    # 白色 "CP" 文字
    painter.setPen(QColor("white"))
    font_size: int = max(8, size * 20 // 64)
    font: QFont = QFont("Arial", font_size, QFont.Weight.Bold)
    painter.setFont(font)
    painter.drawText(pixmap.rect(), Qt.AlignmentFlag.AlignCenter, "CP")

    painter.end()
    return QIcon(pixmap)


def apply_shadow(
    widget: QWidget,
    blur: int = 12,
    offset_y: int = 2,
    alpha: int = 25,
) -> None:
    """
    Business Logic:
        卡片等组件需要微妙的浮动阴影以增加层次感，QSS 无法实现真正的阴影效果。

    Code Logic:
        使用 QGraphicsDropShadowEffect 为指定 widget 添加阴影。
    """
    shadow = QGraphicsDropShadowEffect(widget)
    shadow.setBlurRadius(blur)
    shadow.setOffset(QPointF(0, offset_y))
    shadow.setColor(QColor(0, 0, 0, alpha))
    widget.setGraphicsEffect(shadow)
