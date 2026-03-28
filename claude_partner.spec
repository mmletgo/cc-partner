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
        # 第三方库隐藏导入
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

pyz = PYZ(a.pure, a.zipped_data, cipher=block_cipher)

# 所有平台统一 onefile 模式，生成单个可执行文件
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
