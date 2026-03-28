# -*- mode: python ; coding: utf-8 -*-
"""PyInstaller 构建配置文件，支持 Mac/Windows/Ubuntu 三平台。"""

import sys
import os
from pathlib import Path

block_cipher = None
project_dir = os.path.dirname(os.path.abspath(SPEC))
src_dir = os.path.join(project_dir, 'src')
scripts_dir = os.path.join(project_dir, 'scripts')

# 平台判断
is_mac = sys.platform == 'darwin'
is_win = sys.platform == 'win32'
is_linux = sys.platform.startswith('linux')

# 图标
icon_file = None
if is_win:
    icon_file = os.path.join(scripts_dir, 'icon.ico')
elif is_mac:
    icon_file = os.path.join(scripts_dir, 'icon.icns')
elif is_linux:
    icon_file = os.path.join(scripts_dir, 'icon.png')

# 定位 PyQt6 自带的正确 Qt6 插件路径（避免 conda Qt5 插件污染）
import PyQt6
_pyqt6_qt6_dir = os.path.join(os.path.dirname(PyQt6.__file__), 'Qt6')

a = Analysis(
    [os.path.join(src_dir, 'claude_partner', 'app.py')],
    pathex=[src_dir],
    binaries=[],
    datas=[],
    hiddenimports=[
        'claude_partner',
        'claude_partner.config',
        'claude_partner.models',
        'claude_partner.models.prompt',
        'claude_partner.models.device',
        'claude_partner.models.transfer',
        'claude_partner.storage',
        'claude_partner.storage.database',
        'claude_partner.storage.prompt_repo',
        'claude_partner.network',
        'claude_partner.network.discovery',
        'claude_partner.network.server',
        'claude_partner.network.client',
        'claude_partner.network.protocol',
        'claude_partner.sync',
        'claude_partner.sync.vector_clock',
        'claude_partner.sync.merger',
        'claude_partner.sync.engine',
        'claude_partner.transfer',
        'claude_partner.transfer.sender',
        'claude_partner.transfer.receiver',
        'claude_partner.screenshot',
        'claude_partner.screenshot.capture',
        'claude_partner.screenshot.overlay',
        'claude_partner.ui',
        'claude_partner.ui.main_window',
        'claude_partner.ui.prompt_panel',
        'claude_partner.ui.transfer_panel',
        'claude_partner.ui.device_panel',
        'claude_partner.ui.tray',
        'claude_partner.ui.widgets',
        'claude_partner.ui.widgets.tag_widget',
        'claude_partner.ui.widgets.prompt_card',
        'claude_partner.ui.theme',
        'claude_partner.ui.settings_panel',
        'claude_partner.hotkey',
        'claude_partner.hotkey.listener',
        # 第三方库隐藏导入
        'pynput',
        'pynput.keyboard',
        'pynput.keyboard._darwin',
        'qasync',
        'aiohttp',
        'aiosqlite',
        'zeroconf',
        'PyQt6',
        'PyQt6.QtCore',
        'PyQt6.QtGui',
        'PyQt6.QtWidgets',
        'PyQt6.sip',
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[
        'tkinter',
        'matplotlib',
        'numpy',
        'scipy',
        'pandas',
        'PIL',
        'cv2',
        'PyQt5',
        'PySide2',
        'PySide6',
    ],
    noarchive=False,
    optimize=0,
)

# ---- 修复 conda 环境下 Qt5 插件污染问题 ----
# PyInstaller 从 QLibraryInfo 获取插件路径，在 conda 中这指向 Qt5。
# 需要将被错误收集的 Qt5 插件替换为 PyQt6 自带的正确 Qt6 插件。
_qt5_plugin_markers = ('libQt5', 'Qt5Core', 'Qt5Gui', 'Qt5Widgets', 'Qt5DBus', 'Qt5PrintSupport')

def _is_qt5_artifact(name, src):
    """判断是否是被错误收集的 Qt5 文件。"""
    basename = os.path.basename(src)
    return any(marker in basename for marker in _qt5_plugin_markers)

# 从 binaries 中移除 Qt5 相关文件
a.binaries = [(name, src, typ) for name, src, typ in a.binaries if not _is_qt5_artifact(name, src)]
a.datas = [(name, src, typ) for name, src, typ in a.datas if not _is_qt5_artifact(name, src)]

# 替换 Qt6 插件：用 PyQt6 wheel 自带的正确版本覆盖 conda Qt5 的
_pyqt6_plugins_dir = os.path.join(_pyqt6_qt6_dir, 'plugins')
if os.path.isdir(_pyqt6_plugins_dir):
    for plugin_subdir in os.listdir(_pyqt6_plugins_dir):
        subdir_path = os.path.join(_pyqt6_plugins_dir, plugin_subdir)
        if not os.path.isdir(subdir_path):
            continue
        for plugin_file in os.listdir(subdir_path):
            if plugin_file.endswith(('.dylib', '.so', '.dll')):
                src = os.path.join(subdir_path, plugin_file)
                dest = os.path.join('PyQt6', 'Qt6', 'plugins', plugin_subdir, plugin_file)
                # 移除已有的同名条目
                a.binaries = [(n, s, t) for n, s, t in a.binaries if n != dest]
                a.binaries.append((dest, src, 'BINARY'))

# 同时收集 PyQt6 的 Qt6 lib 目录中的 dylib（Qt6 框架库）
_pyqt6_lib_dir = os.path.join(_pyqt6_qt6_dir, 'lib')
if os.path.isdir(_pyqt6_lib_dir):
    for lib_file in os.listdir(_pyqt6_lib_dir):
        if lib_file.endswith('.dylib'):
            src = os.path.join(_pyqt6_lib_dir, lib_file)
            dest = os.path.join('PyQt6', 'Qt6', 'lib', lib_file)
            a.binaries = [(n, s, t) for n, s, t in a.binaries if n != dest]
            a.binaries.append((dest, src, 'BINARY'))

# ---- 结束修复 ----

pyz = PYZ(a.pure, a.zipped_data, cipher=block_cipher)

if is_mac:
    # macOS: onedir 模式 + BUNDLE 生成 .app
    exe = EXE(
        pyz,
        a.scripts,
        [],
        exclude_binaries=True,
        name='ClaudePartner',
        debug=False,
        bootloader_ignore_signals=False,
        strip=False,
        upx=True,
        console=False,
        disable_windowed_traceback=False,
        argv_emulation=False,
        target_arch=None,
        codesign_identity=None,
        entitlements_file=None,
        icon=icon_file,
    )
    coll = COLLECT(
        exe,
        a.binaries,
        a.datas,
        strip=False,
        upx=True,
        upx_exclude=[],
        name='ClaudePartner',
    )
    app = BUNDLE(
        coll,
        name='ClaudePartner.app',
        icon=icon_file,
        bundle_identifier='com.claude-partner.app',
        info_plist={
            'CFBundleName': 'Claude Partner',
            'CFBundleDisplayName': 'Claude Partner',
            'CFBundleVersion': '0.1.0',
            'CFBundleShortVersionString': '0.1.0',
            'NSHighResolutionCapable': True,
            'LSBackgroundOnly': False,
        },
    )
else:
    # Windows/Linux: onefile 模式
    exe = EXE(
        pyz,
        a.scripts,
        a.binaries,
        a.datas,
        [],
        name='ClaudePartner',
        debug=False,
        bootloader_ignore_signals=False,
        strip=False,
        upx=True,
        upx_exclude=[],
        runtime_tmpdir=None,
        console=False,
        disable_windowed_traceback=False,
        argv_emulation=False,
        target_arch=None,
        codesign_identity=None,
        entitlements_file=None,
        icon=icon_file,
    )
