"""
macOS 原生偏好设置面板扁平风格集中式主题模块，支持浅色/深色模式自动切换。

Business Logic:
    项目 UI 需要统一的 macOS 原生扁平视觉风格，参考系统偏好设置面板的简洁设计。
    使用实心纯色背景、扁平按钮、轻柔阴影和干净的细线边框，不使用任何半透明、
    渐变或毛玻璃效果。同时需要适配系统深色模式，在深色主题下自动切换调色板。
    将所有主题常量和 QSS 样式函数集中在一个模块中，避免各组件重复定义样式。

Code Logic:
    提供颜色/字体/间距常量、返回 QSS 字符串的组件样式函数、以及阴影等辅助函数。
    通过 apply_theme() 函数切换浅色/深色调色板，所有模块级颜色变量随之更新。
    所有背景色均为实心纯色，不使用 rgba 半透明或 qlineargradient 渐变。
"""

import math

from PyQt6.QtCore import QPointF, QRect, QRectF, Qt
from PyQt6.QtGui import QColor, QFont, QIcon, QPainter, QPen, QPixmap
from PyQt6.QtWidgets import QGraphicsDropShadowEffect, QProxyStyle, QStyle, QStyleOption, QWidget

# ── 浅色调色板（默认）────────────────────────────────────────────────────

_LIGHT_PALETTE: dict[str, str] = {
    "ACCENT": "#007AFF",
    "ACCENT_HOVER": "#0062CC",
    "ACCENT_PRESSED": "#004999",
    "BG_PRIMARY": "#FFFFFF",
    "BG_SECONDARY": "#F5F5F7",
    "BG_TERTIARY": "#E8E8ED",
    "BORDER": "#E5E5EA",
    "BORDER_SUBTLE": "#F0F0F0",
    "TEXT_PRIMARY": "#1D1D1F",
    "TEXT_SECONDARY": "#86868B",
    "TEXT_TERTIARY": "#AEAEB2",
    "GREEN": "#34C759",
    "RED": "#FF3B30",
    "ORANGE": "#FF9500",
    "SHADOW_LIGHT": "rgba(0, 0, 0, 10)",
    "SHADOW_MEDIUM": "rgba(0, 0, 0, 20)",
    "DANGER_HOVER_BG": "#FDE7E7",
    "SCROLLBAR_HANDLE": "rgba(0, 0, 0, 30)",
    "SCROLLBAR_HANDLE_HOVER": "rgba(0, 0, 0, 60)",
    "STATUS_BG_TRANSFERRING": "#E8F0FE",
    "STATUS_BG_COMPLETED": "#E6F4EA",
    "STATUS_BG_FAILED": "#FDE7E7",
    "STATUS_BG_CANCELLED": "#FEF7E0",
    "WINDOW_BG": "#F5F5F7",
}

# ── 深色调色板 ─────────────────────────────────────────────────────────────

_DARK_PALETTE: dict[str, str] = {
    "ACCENT": "#0A84FF",
    "ACCENT_HOVER": "#409CFF",
    "ACCENT_PRESSED": "#0066CC",
    "BG_PRIMARY": "#2C2C2E",
    "BG_SECONDARY": "#3A3A3C",
    "BG_TERTIARY": "#48484A",
    "BORDER": "#48484A",
    "BORDER_SUBTLE": "#3A3A3C",
    "TEXT_PRIMARY": "#F5F5F7",
    "TEXT_SECONDARY": "#98989D",
    "TEXT_TERTIARY": "#636366",
    "GREEN": "#30D158",
    "RED": "#FF453A",
    "ORANGE": "#FF9F0A",
    "SHADOW_LIGHT": "rgba(0, 0, 0, 30)",
    "SHADOW_MEDIUM": "rgba(0, 0, 0, 50)",
    "DANGER_HOVER_BG": "#3D2222",
    "SCROLLBAR_HANDLE": "rgba(255, 255, 255, 40)",
    "SCROLLBAR_HANDLE_HOVER": "rgba(255, 255, 255, 80)",
    "STATUS_BG_TRANSFERRING": "#1A2D4A",
    "STATUS_BG_COMPLETED": "#1A3D2A",
    "STATUS_BG_FAILED": "#3D1A1A",
    "STATUS_BG_CANCELLED": "#3D2E1A",
    "WINDOW_BG": "#1C1C1E",
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

WINDOW_BG: str = _LIGHT_PALETTE["WINDOW_BG"]

# ── 字体和间距 ────────────────────────────────────────────────────────────

FONT_FAMILY: str = (
    "-apple-system, 'SF Pro Display', 'Segoe UI', 'Noto Sans SC', sans-serif"
)
FONT_SIZE_TITLE: str = "17px"
FONT_SIZE_HEADING: str = "15px"
FONT_SIZE_BODY: str = "14px"
FONT_SIZE_CAPTION: str = "12px"
FONT_SIZE_SMALL: str = "11px"

RADIUS_LARGE: str = "16px"
RADIUS_MEDIUM: str = "12px"
RADIUS_SMALL: str = "8px"

# ── 标签色板（柔和配色：背景色, 文字色）──────────────────────────────────

TAG_COLORS: list[tuple[str, str]] = [
    ("rgba(0, 122, 255, 31)", "#0A84FF"),      # 蓝  alpha 12%
    ("rgba(52, 199, 89, 31)", "#34C759"),       # 绿
    ("rgba(255, 149, 0, 31)", "#FF9500"),       # 橙
    ("rgba(175, 82, 222, 31)", "#AF52DE"),      # 紫
    ("rgba(90, 200, 250, 31)", "#5AC8FA"),      # 青
    ("rgba(255, 59, 48, 31)", "#FF3B30"),       # 红
    ("rgba(88, 86, 214, 31)", "#5856D6"),       # 靛
    ("rgba(0, 199, 190, 31)", "#00C7BE"),       # 薄荷
]

TAG_COLORS_DARK: list[tuple[str, str]] = [
    ("rgba(10, 132, 255, 51)", "#5AC8FA"),     # 蓝  alpha 20%
    ("rgba(48, 209, 88, 51)", "#4ADE80"),       # 绿
    ("rgba(255, 159, 10, 51)", "#FFB74D"),      # 橙
    ("rgba(191, 90, 242, 51)", "#D8B4FE"),      # 紫
    ("rgba(100, 210, 255, 51)", "#93E3FD"),     # 青
    ("rgba(255, 69, 58, 51)", "#FF8A80"),       # 红
    ("rgba(94, 92, 230, 51)", "#A5A3F5"),       # 靛
    ("rgba(102, 212, 207, 51)", "#99E5E1"),     # 薄荷
]


# ── 主题切换 ──────────────────────────────────────────────────────────────


def is_dark_mode() -> bool:
    """
    Business Logic（为什么需要这个函数）:
        外部组件需要知道当前是否为深色模式，以便做条件判断（如图标颜色、动态样式等）。

    Code Logic（这个函数做什么）:
        返回模块内部 _is_dark 状态布尔值。
    """
    return _is_dark


def apply_theme(dark: bool) -> str:
    """
    Business Logic（为什么需要这个函数）:
        应用启动时和系统主题切换时需要将所有颜色变量切换为对应的调色板，
        使所有后续的样式函数调用返回正确的颜色值。

    Code Logic（这个函数做什么）:
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
    Business Logic（为什么需要这个函数）:
        标签颜色需要跟随主题切换，深色模式下使用更亮的标签颜色以保证可读性。

    Code Logic（这个函数做什么）:
        根据当前 _is_dark 状态返回浅色或深色标签色板列表。
    """
    return TAG_COLORS_DARK if _is_dark else TAG_COLORS


# ── 组件样式函数 ──────────────────────────────────────────────────────────


def get_global_stylesheet() -> str:
    """
    Business Logic（为什么需要这个函数）:
        应用启动时需要一次性设置全局 QSS，统一基础字体、背景色、滚动条和提示框风格。
        扁平设计中所有背景均为实心纯色。

    Code Logic（这个函数做什么）:
        返回覆盖 QWidget（实心 WINDOW_BG 背景）、QMainWindow（实心 WINDOW_BG）、
        QDialog（实心 BG_PRIMARY）、QMenu（实心 BG_PRIMARY + 实心边框）、
        QScrollBar（纵向/横向 6px 薄圆角）、QToolTip（实心背景 + 实心边框）的 QSS 字符串。
    """
    return f"""
        QWidget {{
            font-family: {FONT_FAMILY};
            font-size: {FONT_SIZE_BODY};
            color: {TEXT_PRIMARY};
            background: {WINDOW_BG};
        }}


        /* ── 主窗口纯色背景 ── */
        QMainWindow {{
            background: {WINDOW_BG};
        }}

        /* ── 弹窗和菜单使用实心背景 ── */
        QDialog {{
            background: {BG_PRIMARY};
        }}
        QMenu {{
            background: {BG_PRIMARY};
            border: 1px solid {BORDER};
            border-radius: 8px;
            padding: 4px;
        }}
        QMenu::item {{
            padding: 6px 24px;
            border-radius: 4px;
        }}
        QMenu::item:selected {{
            background: {BG_SECONDARY};
        }}
        QMessageBox {{
            background: {BG_PRIMARY};
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

        /* ── 提示框（扁平风格） ── */
        QToolTip {{
            background: {BG_PRIMARY};
            color: {TEXT_PRIMARY};
            border: 1px solid {BORDER};
            border-radius: 6px;
            padding: 6px 10px;
            font-size: {FONT_SIZE_CAPTION};
        }}
    """


def window_bg_style() -> str:
    """
    Business Logic（为什么需要这个函数）:
        主窗口和大面积容器需要统一的纯色背景样式。

    Code Logic（这个函数做什么）:
        返回包含当前主题 WINDOW_BG 纯色背景的 QSS 字符串。
    """
    return f"background: {WINDOW_BG};"


def card_style() -> str:
    """
    Business Logic（为什么需要这个函数）:
        卡片是 UI 中最常见的容器组件，扁平风格下使用实心白色背景 + 灰色细线边框，
        营造干净简洁的分区效果。

    Code Logic（这个函数做什么）:
        返回卡片容器的 QSS 字符串，使用实心 BG_PRIMARY 背景和实心 BORDER 边框。
    """
    return f"""
        border: 1px solid {BORDER};
        border-radius: {RADIUS_LARGE};
        padding: 16px;
        background: {BG_PRIMARY};
    """


def input_style() -> str:
    """
    Business Logic（为什么需要这个函数）:
        文本输入框需要统一外观：扁平风格的实心背景、灰色边框和聚焦高亮。

    Code Logic（这个函数做什么）:
        返回 QLineEdit 和 QTextEdit 的 QSS 字符串，使用实心 BG_SECONDARY 背景，
        focus 时边框变为 ACCENT 蓝色。
    """
    return f"""
        QLineEdit, QTextEdit {{
            border: 1px solid {BORDER};
            border-radius: {RADIUS_MEDIUM};
            padding: 10px 14px;
            font-size: {FONT_SIZE_BODY};
            font-family: {FONT_FAMILY};
            background: {BG_SECONDARY};
            color: {TEXT_PRIMARY};
        }}
        QLineEdit:focus, QTextEdit:focus {{
            border-color: {ACCENT};
        }}
    """


def button_primary_style() -> str:
    """
    Business Logic（为什么需要这个函数）:
        主操作按钮（如"保存""确认"）使用纯色蓝色填充，
        在视觉上突出主要操作，扁平风格不使用渐变。

    Code Logic（这个函数做什么）:
        返回使用 ACCENT 纯色背景的 QPushButton QSS，白色文字，
        含 hover（ACCENT_HOVER）和 pressed（ACCENT_PRESSED）状态。
    """
    return f"""
        QPushButton {{
            background: {ACCENT};
            color: white;
            border: 1px solid {ACCENT};
            border-radius: {RADIUS_MEDIUM};
            padding: 10px 20px;
            font-size: {FONT_SIZE_BODY};
            font-weight: 600;
            font-family: {FONT_FAMILY};
        }}
        QPushButton:hover {{
            background: {ACCENT_HOVER};
            border-color: {ACCENT_HOVER};
        }}
        QPushButton:pressed {{
            background: {ACCENT_PRESSED};
            border-color: {ACCENT_PRESSED};
        }}
    """


def button_secondary_style() -> str:
    """
    Business Logic（为什么需要这个函数）:
        次要操作按钮（如"取消""编辑"）使用白色/浅灰实心背景 + 灰色边框，
        视觉层级低于主按钮。

    Code Logic（这个函数做什么）:
        返回实心 BG_PRIMARY 背景、实心 BORDER 边框的 QPushButton QSS，
        蓝色文字，hover 时背景切换到 BG_SECONDARY。
    """
    return f"""
        QPushButton {{
            background: {BG_PRIMARY};
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
    Business Logic（为什么需要这个函数）:
        危险操作按钮（如"删除"）使用红色文字警示用户，扁平风格的实心边框。

    Code Logic（这个函数做什么）:
        返回透明背景、红色文字的 QPushButton QSS，hover 时背景为浅红色。
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
    Business Logic（为什么需要这个函数）:
        传输任务卡片中的取消按钮需要紧凑的红色危险按钮，尺寸比标准 danger 按钮更小。

    Code Logic（这个函数做什么）:
        返回小尺寸红色文字 QPushButton QSS，padding 和字号更小，
        hover 时背景为浅红色并加红色边框。
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
    Business Logic（为什么需要这个函数）:
        下拉选择框在设备选择、标签筛选等场景中使用，需要扁平风格的实心背景和边框。

    Code Logic（这个函数做什么）:
        返回 QComboBox 完整 QSS，包括实心 BG_PRIMARY 背景、实心 BORDER 边框、
        drop-down 按钮、箭头和下拉列表视图。
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
    Business Logic（为什么需要这个函数）:
        Tab 栏是主窗口的核心导航组件，改为 macOS 偏好设置面板风格的图标工具栏，
        每个 Tab 显示图标 + 文字（图标在上文字在下），选中 Tab 有浅蓝色背景。

    Code Logic（这个函数做什么）:
        返回 QTabWidget::pane 和 QTabBar 的 QSS。Tab 栏位于顶部，
        选中 Tab 使用浅蓝色圆角矩形背景，未选中 Tab 透明背景，
        底部有一条实心分隔线。Tab 高度足够容纳图标 + 文字。
    """
    # 选中 Tab 的浅蓝色背景
    selected_bg: str = "rgba(0, 122, 255, 26)" if not _is_dark else "rgba(10, 132, 255, 38)"

    return f"""
        QTabWidget::pane {{
            border: none;
            border-top: 1px solid {BORDER};
        }}
        QTabBar {{
            background: {WINDOW_BG};
            border: none;
            border-bottom: 1px solid {BORDER};
            qproperty-drawBase: 0;
        }}
        QTabBar::tab {{
            background: transparent;
            border: none;
            border-radius: 8px;
            padding: 8px 16px;
            margin: 4px 2px;
            min-width: 72px;
            min-height: 48px;
            color: {TEXT_SECONDARY};
            font-size: {FONT_SIZE_SMALL};
            font-family: {FONT_FAMILY};
        }}
        QTabBar::tab:selected {{
            background: {selected_bg};
            color: {ACCENT};
            font-weight: 600;
        }}
        QTabBar::tab:hover:!selected {{
            background: {BG_SECONDARY};
            color: {TEXT_PRIMARY};
        }}
    """


def scroll_area_style() -> str:
    """
    Business Logic（为什么需要这个函数）:
        滚动区域作为内容容器不应有可见边框或额外背景色，保持透明。

    Code Logic（这个函数做什么）:
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
    Business Logic（为什么需要这个函数）:
        文件传输等场景需要进度条，扁平风格使用纯色填充，不使用渐变。

    Code Logic（这个函数做什么）:
        返回 QProgressBar 的 QSS，含圆角容器和纯色 ACCENT chunk。
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
            background: {ACCENT};
        }}
    """


def tag_label_style(bg: str, fg: str) -> str:
    """
    Business Logic（为什么需要这个函数）:
        标签需要按不同分类显示不同的配色，在扁平设计中保持柔和的半透明底色。

    Code Logic（这个函数做什么）:
        接收背景色和文字色参数，返回圆角标签 QSS。
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
    Business Logic（为什么需要这个函数）:
        弹窗对话框需要实心背景和圆角样式，与扁平风格一致。

    Code Logic（这个函数做什么）:
        返回 QDialog 的 QSS 字符串，使用实心 BG_PRIMARY 背景。
    """
    return f"""
        QDialog {{
            background: {BG_PRIMARY};
            border-radius: {RADIUS_LARGE};
        }}
    """


def label_title_style() -> str:
    """
    Business Logic（为什么需要这个函数）:
        标题级文字（如面板标题）需要较大字号和加粗，确保视觉层级。

    Code Logic（这个函数做什么）:
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
    Business Logic（为什么需要这个函数）:
        正文级文字用于一般内容展示，保持标准字号和颜色。

    Code Logic（这个函数做什么）:
        返回 QLabel 正文样式 QSS。
    """
    return f"""
        font-size: {FONT_SIZE_BODY};
        font-family: {FONT_FAMILY};
        color: {TEXT_PRIMARY};
    """


def label_caption_style() -> str:
    """
    Business Logic（为什么需要这个函数）:
        辅助说明文字（如时间戳、计数）使用较小字号和浅色，降低视觉权重。

    Code Logic（这个函数做什么）:
        返回 QLabel 辅助文字样式 QSS。
    """
    return f"""
        font-size: {FONT_SIZE_CAPTION};
        font-family: {FONT_FAMILY};
        color: {TEXT_SECONDARY};
    """


# ── 辅助函数 ──────────────────────────────────────────────────────────────


class _NoFocusRectStyle(QProxyStyle):
    """
    全局禁用焦点虚线框的代理样式。

    Business Logic（为什么需要这个类）:
        macOS Cocoa 原生渲染器会在获得焦点的控件（按钮、Tab 等）上绘制虚线焦点框，
        这与扁平 UI 风格不协调。QSS 的 outline:none 对原生焦点框无效，
        需要通过 QProxyStyle 拦截绘制调用来全局禁用。

    Code Logic（这个类做什么）:
        继承 QProxyStyle，重写 drawPrimitive 方法，当绘制元素为 PE_FrameFocusRect 时
        直接跳过，其余元素正常绘制。
    """

    def drawPrimitive(
        self,
        element: QStyle.PrimitiveElement,
        option: QStyleOption | None,
        painter: QPainter | None,
        widget: QWidget | None = None,
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            拦截所有原始图元绘制请求，跳过焦点框的绘制。

        Code Logic（这个函数做什么）:
            如果 element 是 PE_FrameFocusRect 则直接返回不绘制，
            否则调用父类正常绘制。
        """
        if element == QStyle.PrimitiveElement.PE_FrameFocusRect:
            return
        super().drawPrimitive(element, option, painter, widget)


def create_no_focus_style() -> QProxyStyle:
    """
    Business Logic（为什么需要这个函数）:
        应用启动时需要全局禁用焦点虚线框，提供工厂函数供 app.py 调用
        QApplication.setStyle()。

    Code Logic（这个函数做什么）:
        返回一个 _NoFocusRectStyle 实例，调用方通过 app.setStyle() 应用。
    """
    return _NoFocusRectStyle()


def create_tab_icon(name: str, selected: bool = False) -> QIcon:
    """
    Business Logic（为什么需要这个函数）:
        macOS 偏好设置面板风格的标签栏需要每个 Tab 显示图标，
        选中时蓝色、未选中时灰色，使用 QPainter 绘制简单矢量图标。

    Code Logic（这个函数做什么）:
        根据 name 参数（"prompt", "transfer", "device", "scratchpad", "settings"）
        绘制对应的简单图标。selected 控制图标颜色：选中蓝色 / 未选中灰色。
        返回 32x32 的 QIcon。
    """
    size: int = 32
    pixmap: QPixmap = QPixmap(size, size)
    pixmap.fill(QColor(0, 0, 0, 0))

    painter: QPainter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing, True)

    # 选中蓝色 / 未选中灰色
    icon_color: QColor = QColor(ACCENT) if selected else QColor(TEXT_SECONDARY)
    pen: QPen = QPen(icon_color, 2.0)
    painter.setPen(pen)
    painter.setBrush(Qt.BrushStyle.NoBrush)

    if name == "prompt":
        _draw_prompt_icon(painter, size, icon_color)
    elif name == "transfer":
        _draw_transfer_icon(painter, size, icon_color)
    elif name == "device":
        _draw_device_icon(painter, size, icon_color)
    elif name == "scratchpad":
        _draw_scratchpad_icon(painter, size, icon_color)
    elif name == "settings":
        _draw_settings_icon(painter, size, icon_color)

    painter.end()
    return QIcon(pixmap)


def _draw_prompt_icon(painter: QPainter, size: int, color: QColor) -> None:
    """
    Business Logic（为什么需要这个函数）:
        绘制 "prompt" Tab 的文档图标，由矩形轮廓加横线组成。

    Code Logic（这个函数做什么）:
        在给定尺寸内绘制一个带折角的文档矩形和三条代表文字的横线。
    """
    m: int = 6  # margin
    # 文档矩形
    painter.drawRoundedRect(QRectF(m, m, size - 2 * m, size - 2 * m), 2, 2)
    # 横线（代表文字）
    pen: QPen = QPen(color, 1.5)
    painter.setPen(pen)
    line_x1: int = m + 4
    line_x2: int = size - m - 4
    for i, y_offset in enumerate([12, 16, 20]):
        # 第三行稍短
        x2: int = line_x2 if i < 2 else line_x2 - 5
        painter.drawLine(line_x1, y_offset, x2, y_offset)


def _draw_transfer_icon(painter: QPainter, size: int, color: QColor) -> None:
    """
    Business Logic（为什么需要这个函数）:
        绘制 "transfer" Tab 的双箭头图标，代表文件传输。

    Code Logic（这个函数做什么）:
        绘制上下两个箭头，分别指向上和下方向。
    """
    cx: int = size // 2
    # 上箭头
    painter.drawLine(cx, 6, cx, 16)
    painter.drawLine(cx - 4, 10, cx, 6)
    painter.drawLine(cx + 4, 10, cx, 6)
    # 下箭头
    painter.drawLine(cx, 16, cx, 26)
    painter.drawLine(cx - 4, 22, cx, 26)
    painter.drawLine(cx + 4, 22, cx, 26)


def _draw_device_icon(painter: QPainter, size: int, color: QColor) -> None:
    """
    Business Logic（为什么需要这个函数）:
        绘制 "device" Tab 的显示器图标，代表设备。

    Code Logic（这个函数做什么）:
        绘制一个显示器轮廓（矩形屏幕 + 底座支架）。
    """
    m: int = 5
    # 屏幕
    painter.drawRoundedRect(QRectF(m, m, size - 2 * m, size - 2 * m - 6), 2, 2)
    # 底座
    cx: int = size // 2
    painter.drawLine(cx, size - m - 4, cx, size - m)
    painter.drawLine(cx - 5, size - m, cx + 5, size - m)


def _draw_scratchpad_icon(painter: QPainter, size: int, color: QColor) -> None:
    """
    Business Logic（为什么需要这个函数）:
        绘制 "scratchpad" Tab 的笔记图标，由本子和铅笔组成。

    Code Logic（这个函数做什么）:
        绘制一个本子矩形和一支倾斜的铅笔线条。
    """
    m: int = 6
    # 本子
    painter.drawRoundedRect(QRectF(m, m, size - 2 * m - 4, size - 2 * m), 2, 2)
    # 铅笔（倾斜线 + 笔尖）
    pen: QPen = QPen(color, 1.5)
    painter.setPen(pen)
    painter.drawLine(size - m - 2, m + 2, size - m - 8, m + 14)
    painter.drawLine(size - m - 8, m + 14, size - m - 6, m + 15)


def _draw_settings_icon(painter: QPainter, size: int, color: QColor) -> None:
    """
    Business Logic（为什么需要这个函数）:
        绘制 "settings" Tab 的齿轮图标，代表设置。

    Code Logic（这个函数做什么）:
        绘制一个圆形中心和周围的齿，形成齿轮造型。
    """
    cx: float = size / 2.0
    cy: float = size / 2.0
    inner_r: float = 4.0
    outer_r: float = 9.0
    teeth: int = 8

    # 中心圆
    painter.drawEllipse(QRectF(cx - inner_r, cy - inner_r, inner_r * 2, inner_r * 2))

    # 齿
    pen: QPen = QPen(color, 2.5)
    painter.setPen(pen)
    for i in range(teeth):
        angle: float = 2 * math.pi * i / teeth
        x1: float = cx + (inner_r + 1) * math.cos(angle)
        y1: float = cy + (inner_r + 1) * math.sin(angle)
        x2: float = cx + outer_r * math.cos(angle)
        y2: float = cy + outer_r * math.sin(angle)
        painter.drawLine(QPointF(x1, y1), QPointF(x2, y2))


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
    blur: int = 8,
    offset_y: int = 2,
    alpha: int = 10,
) -> None:
    """
    Business Logic（为什么需要这个函数）:
        卡片等组件需要轻柔的阴影以增加层次感，扁平风格下阴影参数更小更柔和。

    Code Logic（这个函数做什么）:
        使用 QGraphicsDropShadowEffect 为指定 widget 添加阴影，
        默认参数轻柔（blur=8, offset_y=2, alpha=10）。
    """
    shadow: QGraphicsDropShadowEffect = QGraphicsDropShadowEffect(widget)
    shadow.setBlurRadius(blur)
    shadow.setOffset(QPointF(0, offset_y))
    shadow.setColor(QColor(0, 0, 0, alpha))
    widget.setGraphicsEffect(shadow)
