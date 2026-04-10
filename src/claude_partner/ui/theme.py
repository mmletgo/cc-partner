"""
Apple 风格拟态玻璃 (Glassmorphism + Neumorphism) 集中式主题模块，支持浅色/深色模式自动切换。

Business Logic:
    项目 UI 需要统一的 Apple 拟态玻璃视觉风格，在半透明背景、柔和阴影和渐变高光的基础上
    营造出毛玻璃和拟物质感。同时需要适配系统深色模式，在深色主题下自动切换调色板。
    将所有主题常量和 QSS 样式函数集中在一个模块中，避免各组件重复定义样式。

Code Logic:
    提供颜色/字体/间距常量、返回 QSS 字符串的组件样式函数、以及阴影等辅助函数。
    通过 apply_theme() 函数切换浅色/深色调色板，所有模块级颜色变量随之更新。
    由于 QSS 不支持 backdrop-filter 等 CSS 特性，玻璃效果通过半透明背景色 + 亮边框 +
    QGraphicsDropShadowEffect 阴影 + 渐变背景底色组合模拟。
"""

from PyQt6.QtCore import QPointF, Qt
from PyQt6.QtGui import QColor, QFont, QIcon, QPainter, QPixmap
from PyQt6.QtWidgets import QGraphicsDropShadowEffect, QWidget

# ── 浅色调色板（默认）────────────────────────────────────────────────────

_LIGHT_PALETTE: dict[str, str] = {
    "ACCENT": "#007AFF",
    "ACCENT_HOVER": "#0062CC",
    "ACCENT_PRESSED": "#004999",
    "BG_PRIMARY": "rgba(255, 255, 255, 0.72)",
    "BG_SECONDARY": "rgba(245, 245, 247, 0.65)",
    "BG_TERTIARY": "rgba(232, 232, 237, 0.60)",
    "BORDER": "rgba(255, 255, 255, 0.45)",
    "BORDER_SUBTLE": "rgba(0, 0, 0, 0.06)",
    "TEXT_PRIMARY": "#1D1D1F",
    "TEXT_SECONDARY": "#86868B",
    "TEXT_TERTIARY": "#AEAEB2",
    "GREEN": "#34C759",
    "RED": "#FF3B30",
    "ORANGE": "#FF9500",
    "SHADOW_LIGHT": "rgba(0, 0, 0, 0.06)",
    "SHADOW_MEDIUM": "rgba(0, 0, 0, 0.12)",
    "DANGER_HOVER_BG": "rgba(255, 59, 48, 0.10)",
    "SCROLLBAR_HANDLE": "rgba(0, 0, 0, 0.12)",
    "SCROLLBAR_HANDLE_HOVER": "rgba(0, 0, 0, 0.25)",
    "STATUS_BG_TRANSFERRING": "rgba(0, 122, 255, 0.10)",
    "STATUS_BG_COMPLETED": "rgba(52, 199, 89, 0.10)",
    "STATUS_BG_FAILED": "rgba(255, 59, 48, 0.10)",
    "STATUS_BG_CANCELLED": "rgba(255, 149, 0, 0.10)",
}

# ── 深色调色板 ─────────────────────────────────────────────────────────────

_DARK_PALETTE: dict[str, str] = {
    "ACCENT": "#0A84FF",
    "ACCENT_HOVER": "#409CFF",
    "ACCENT_PRESSED": "#0066CC",
    "BG_PRIMARY": "rgba(44, 44, 46, 0.72)",
    "BG_SECONDARY": "rgba(58, 58, 60, 0.65)",
    "BG_TERTIARY": "rgba(72, 72, 74, 0.55)",
    "BORDER": "rgba(255, 255, 255, 0.12)",
    "BORDER_SUBTLE": "rgba(255, 255, 255, 0.05)",
    "TEXT_PRIMARY": "#F5F5F7",
    "TEXT_SECONDARY": "#98989D",
    "TEXT_TERTIARY": "#636366",
    "GREEN": "#30D158",
    "RED": "#FF453A",
    "ORANGE": "#FF9F0A",
    "SHADOW_LIGHT": "rgba(0, 0, 0, 0.20)",
    "SHADOW_MEDIUM": "rgba(0, 0, 0, 0.35)",
    "DANGER_HOVER_BG": "rgba(255, 69, 58, 0.15)",
    "SCROLLBAR_HANDLE": "rgba(255, 255, 255, 0.15)",
    "SCROLLBAR_HANDLE_HOVER": "rgba(255, 255, 255, 0.30)",
    "STATUS_BG_TRANSFERRING": "rgba(10, 132, 255, 0.15)",
    "STATUS_BG_COMPLETED": "rgba(48, 209, 88, 0.15)",
    "STATUS_BG_FAILED": "rgba(255, 69, 58, 0.15)",
    "STATUS_BG_CANCELLED": "rgba(255, 159, 10, 0.15)",
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

RADIUS_LARGE: str = "16px"
RADIUS_MEDIUM: str = "12px"
RADIUS_SMALL: str = "8px"

# ── 玻璃效果专用常量 ──────────────────────────────────────────────────────

GLASS_BORDER_WIDTH: str = "1px"

WINDOW_BG_LIGHT: str = (
    "qlineargradient(x1:0, y1:0, x2:1, y2:1, "
    "stop:0 #E8ECF4, stop:0.5 #F0F0F6, stop:1 #E4E8F0)"
)
WINDOW_BG_DARK: str = (
    "qlineargradient(x1:0, y1:0, x2:1, y2:1, "
    "stop:0 #1A1A2E, stop:0.5 #1C1C30, stop:1 #16213E)"
)
WINDOW_BG: str = WINDOW_BG_LIGHT

# ── 强调色渐变（按钮用）──────────────────────────────────────────────────

_ACCENT_GRADIENT_LIGHT: str = (
    "qlineargradient(x1:0, y1:0, x2:0, y2:1, "
    "stop:0 #007AFF, stop:1 #005ECB)"
)
_ACCENT_GRADIENT_DARK: str = (
    "qlineargradient(x1:0, y1:0, x2:0, y2:1, "
    "stop:0 #0A84FF, stop:1 #0060DF)"
)
ACCENT_GRADIENT: str = _ACCENT_GRADIENT_LIGHT

# ── 标签色板（玻璃风格半透明配色：背景色, 文字色）──────────────────────

TAG_COLORS: list[tuple[str, str]] = [
    ("rgba(0, 122, 255, 0.12)", "#0A84FF"),    # 蓝
    ("rgba(52, 199, 89, 0.12)", "#34C759"),     # 绿
    ("rgba(255, 149, 0, 0.12)", "#FF9500"),     # 橙
    ("rgba(175, 82, 222, 0.12)", "#AF52DE"),    # 紫
    ("rgba(90, 200, 250, 0.12)", "#5AC8FA"),    # 青
    ("rgba(255, 59, 48, 0.12)", "#FF3B30"),     # 红
    ("rgba(88, 86, 214, 0.12)", "#5856D6"),     # 靛
    ("rgba(0, 199, 190, 0.12)", "#00C7BE"),     # 薄荷
]

TAG_COLORS_DARK: list[tuple[str, str]] = [
    ("rgba(10, 132, 255, 0.20)", "#5AC8FA"),    # 蓝
    ("rgba(48, 209, 88, 0.20)", "#4ADE80"),     # 绿
    ("rgba(255, 159, 10, 0.20)", "#FFB74D"),    # 橙
    ("rgba(191, 90, 242, 0.20)", "#D8B4FE"),    # 紫
    ("rgba(100, 210, 255, 0.20)", "#93E3FD"),   # 青
    ("rgba(255, 69, 58, 0.20)", "#FF8A80"),     # 红
    ("rgba(94, 92, 230, 0.20)", "#A5A3F5"),     # 靛
    ("rgba(102, 212, 207, 0.20)", "#99E5E1"),   # 薄荷
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
        同时切换玻璃效果相关的辅助变量（窗口背景渐变、强调色渐变等）。

    Code Logic:
        根据 dark 参数选择深色或浅色调色板，用 globals() 批量更新模块级变量，
        同时更新 WINDOW_BG 和 ACCENT_GRADIENT 等玻璃效果专用变量，
        返回最新的全局 QSS 字符串供 QApplication 重新应用。
    """
    global _is_dark, WINDOW_BG, ACCENT_GRADIENT
    _is_dark = dark

    palette: dict[str, str] = _DARK_PALETTE if dark else _LIGHT_PALETTE
    for key, value in palette.items():
        globals()[key] = value

    # 切换玻璃效果专用变量
    WINDOW_BG = WINDOW_BG_DARK if dark else WINDOW_BG_LIGHT
    globals()["WINDOW_BG"] = WINDOW_BG

    ACCENT_GRADIENT = _ACCENT_GRADIENT_DARK if dark else _ACCENT_GRADIENT_LIGHT
    globals()["ACCENT_GRADIENT"] = ACCENT_GRADIENT

    return get_global_stylesheet()


def current_tag_colors() -> list[tuple[str, str]]:
    """
    Business Logic:
        标签颜色需要跟随主题切换，深色模式下使用半透明高亮标签颜色。

    Code Logic:
        根据当前 _is_dark 状态返回浅色或深色标签色板。
    """
    return TAG_COLORS_DARK if _is_dark else TAG_COLORS


# ── 组件样式函数 ──────────────────────────────────────────────────────────


def get_global_stylesheet() -> str:
    """
    Business Logic:
        应用启动时需要一次性设置全局 QSS，统一基础字体、滚动条和提示框风格。
        玻璃设计中 QWidget 基础背景设为 transparent，让父容器的渐变底色透出。

    Code Logic:
        返回覆盖 QWidget 字体（背景透明）、QScrollBar（纵向/横向 6px 薄圆角半透明）、
        QToolTip（玻璃风格半透明背景加亮边框）的 QSS 字符串。
    """
    return f"""
        QWidget {{
            font-family: {FONT_FAMILY};
            font-size: {FONT_SIZE_BODY};
            color: {TEXT_PRIMARY};
            background: transparent;
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

        /* ── 提示框（玻璃风格） ── */
        QToolTip {{
            background: {BG_PRIMARY};
            color: {TEXT_PRIMARY};
            border: {GLASS_BORDER_WIDTH} solid {BORDER};
            border-radius: 8px;
            padding: 6px 10px;
            font-size: {FONT_SIZE_CAPTION};
        }}
    """


def window_bg_style() -> str:
    """
    Business Logic:
        主窗口需要一个渐变底色背景，让上层半透明玻璃组件有东西可透，
        营造毛玻璃的纵深层次感。

    Code Logic:
        返回包含当前主题渐变背景（WINDOW_BG）的 QSS 字符串，
        供主窗口直接使用。
    """
    return f"background: {WINDOW_BG};"


def card_style() -> str:
    """
    Business Logic:
        卡片是 UI 中最常见的容器组件，玻璃风格下使用半透明背景 + 亮边框，
        营造浮在渐变底色上的毛玻璃效果。

    Code Logic:
        返回卡片容器的 QSS 字符串，使用半透明 BG_PRIMARY 和玻璃亮边框 BORDER。
        真正的阴影需用 apply_shadow() 或 apply_glass_shadow() 函数附加。
    """
    return f"""
        border: {GLASS_BORDER_WIDTH} solid {BORDER};
        border-radius: {RADIUS_LARGE};
        padding: 16px;
        background: {BG_PRIMARY};
    """


def input_style() -> str:
    """
    Business Logic:
        文本输入框需要统一外观：玻璃风格的半透明背景、亮边框和聚焦高亮。

    Code Logic:
        返回 QLineEdit 和 QTextEdit 的 QSS 字符串，使用半透明 BG_SECONDARY 背景，
        focus 时边框变为 ACCENT 色模拟发光效果。
    """
    return f"""
        QLineEdit, QTextEdit {{
            border: {GLASS_BORDER_WIDTH} solid {BORDER};
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
    Business Logic:
        主操作按钮（如"保存""确认"）使用渐变蓝色填充 + 玻璃高光亮边，
        在视觉上突出主要操作并保持玻璃质感。

    Code Logic:
        返回使用 ACCENT_GRADIENT 渐变背景的 QPushButton QSS，
        含 hover / pressed 状态的不同渐变色，
        以及 rgba(255,255,255,0.2) 的玻璃高光边框。
    """
    # 根据当前主题构建 hover/pressed 渐变
    if _is_dark:
        hover_gradient: str = (
            "qlineargradient(x1:0, y1:0, x2:0, y2:1, "
            "stop:0 #409CFF, stop:1 #0A84FF)"
        )
        pressed_gradient: str = (
            "qlineargradient(x1:0, y1:0, x2:0, y2:1, "
            "stop:0 #0066CC, stop:1 #004999)"
        )
    else:
        hover_gradient = (
            "qlineargradient(x1:0, y1:0, x2:0, y2:1, "
            "stop:0 #0062CC, stop:1 #004999)"
        )
        pressed_gradient = (
            "qlineargradient(x1:0, y1:0, x2:0, y2:1, "
            "stop:0 #004999, stop:1 #003D80)"
        )

    return f"""
        QPushButton {{
            background: {ACCENT_GRADIENT};
            color: white;
            border: {GLASS_BORDER_WIDTH} solid rgba(255, 255, 255, 0.20);
            border-radius: {RADIUS_MEDIUM};
            padding: 10px 20px;
            font-size: {FONT_SIZE_BODY};
            font-weight: 600;
            font-family: {FONT_FAMILY};
        }}
        QPushButton:hover {{
            background: {hover_gradient};
        }}
        QPushButton:pressed {{
            background: {pressed_gradient};
        }}
    """


def button_secondary_style() -> str:
    """
    Business Logic:
        次要操作按钮（如"取消""编辑"）使用半透明玻璃背景 + 亮边框，
        视觉层级低于主按钮但保持玻璃质感。

    Code Logic:
        返回半透明 BG_PRIMARY 背景、玻璃亮边框的 QPushButton QSS，
        hover 时背景切换到 BG_SECONDARY。
    """
    return f"""
        QPushButton {{
            background: {BG_PRIMARY};
            color: {ACCENT};
            border: {GLASS_BORDER_WIDTH} solid {BORDER};
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
        危险操作按钮（如"删除"）使用红色文字警示用户，保持玻璃风格的半透明背景。

    Code Logic:
        返回半透明背景、红色文字的 QPushButton QSS，hover 时背景为半透明红色。
    """
    return f"""
        QPushButton {{
            background: transparent;
            color: {RED};
            border: {GLASS_BORDER_WIDTH} solid {BORDER};
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
        传输任务卡片中的取消按钮需要紧凑的红色危险按钮，尺寸比标准 danger 按钮更小，
        保持玻璃风格。

    Code Logic:
        返回小尺寸红色文字 QPushButton QSS，padding 和字号更小，
        hover 时背景为半透明红色并加红色边框。
    """
    return f"""
        QPushButton {{
            background: {BG_PRIMARY};
            color: {RED};
            border: {GLASS_BORDER_WIDTH} solid {BORDER};
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
        下拉选择框在设备选择、标签筛选等场景中使用，需要玻璃风格的半透明背景和亮边框。

    Code Logic:
        返回 QComboBox 完整 QSS，包括半透明背景、玻璃亮边框、
        drop-down 按钮、箭头和下拉列表视图。
    """
    return f"""
        QComboBox {{
            border: {GLASS_BORDER_WIDTH} solid {BORDER};
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
            border: {GLASS_BORDER_WIDTH} solid {BORDER};
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
        Tab 栏是主窗口的核心导航组件，使用玻璃容器效果模拟 Apple 分段控件。

    Code Logic:
        返回 QTabWidget::pane 和 QTabBar 的 QSS，Tab 栏使用半透明 BG_SECONDARY 背景
        + 玻璃亮边框，选中 Tab 使用更透明的 BG_PRIMARY 背景 + 亮边框突出显示。
    """
    return f"""
        QTabWidget::pane {{
            border: none;
        }}
        QTabBar {{
            background: {BG_SECONDARY};
            border: {GLASS_BORDER_WIDTH} solid {BORDER};
            border-radius: 10px;
            padding: 3px;
        }}
        QTabBar::tab {{
            background: transparent;
            border-radius: 8px;
            padding: 6px 24px;
            margin: 2px;
            min-width: 80px;
            color: {TEXT_SECONDARY};
            font-size: 13px;
            font-family: {FONT_FAMILY};
        }}
        QTabBar::tab:selected {{
            background: {BG_PRIMARY};
            border: {GLASS_BORDER_WIDTH} solid {BORDER};
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
        滚动区域作为内容容器不应有可见边框或背景色，让玻璃效果透出。

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
        文件传输等场景需要进度条，使用更丰富的三段渐变营造玻璃般的光泽感。

    Code Logic:
        返回 QProgressBar 的 QSS，含圆角半透明容器和蓝-青-蓝三段渐变 chunk。
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
                stop:0 {ACCENT}, stop:0.5 #5AC8FA, stop:1 {ACCENT}
            );
        }}
    """


def tag_label_style(bg: str, fg: str) -> str:
    """
    Business Logic:
        标签需要按不同分类显示不同的半透明配色，匹配玻璃设计风格。

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
        弹窗对话框需要玻璃风格的半透明背景和圆角样式。

    Code Logic:
        返回 QDialog 的 QSS 字符串，使用半透明 BG_PRIMARY 和玻璃亮边框。
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
    Business Logic:
        应用的窗口图标和托盘图标（非 macOS）共用同一视觉样式，
        集中在 theme 模块避免重复绘制代码。

    Code Logic:
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
    blur: int = 20,
    offset_y: int = 4,
    alpha: int = 20,
) -> None:
    """
    Business Logic:
        卡片等组件需要柔和扩散的浮动阴影以增加层次感，QSS 无法实现真正的阴影效果。
        拟态风格下默认阴影更柔和、更扩散。

    Code Logic:
        使用 QGraphicsDropShadowEffect 为指定 widget 添加阴影，
        默认参数比传统风格更大（blur=20, offset_y=4, alpha=20）。
    """
    shadow: QGraphicsDropShadowEffect = QGraphicsDropShadowEffect(widget)
    shadow.setBlurRadius(blur)
    shadow.setOffset(QPointF(0, offset_y))
    shadow.setColor(QColor(0, 0, 0, alpha))
    widget.setGraphicsEffect(shadow)


def apply_glass_shadow(
    widget: QWidget,
    blur: int = 24,
    offset_y: int = 6,
    alpha: int = 30,
) -> None:
    """
    Business Logic:
        玻璃卡片等重要组件需要更强的阴影效果，以营造浮起的毛玻璃层次感，
        区别于普通组件的轻微阴影。

    Code Logic:
        使用 QGraphicsDropShadowEffect 为指定 widget 添加更强的玻璃阴影，
        默认参数比 apply_shadow 更大（blur=24, offset_y=6, alpha=30）。
    """
    shadow: QGraphicsDropShadowEffect = QGraphicsDropShadowEffect(widget)
    shadow.setBlurRadius(blur)
    shadow.setOffset(QPointF(0, offset_y))
    shadow.setColor(QColor(0, 0, 0, alpha))
    widget.setGraphicsEffect(shadow)
