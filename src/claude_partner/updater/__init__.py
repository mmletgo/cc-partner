# -*- coding: utf-8 -*-
"""自动更新模块：提供版本检查、下载和安装功能。"""

from claude_partner.updater.checker import UpdateChecker, UpdateInfo
from claude_partner.updater.downloader import UpdateDownloader
from claude_partner.updater.installer import UpdateInstaller

__all__: list[str] = [
    "UpdateChecker",
    "UpdateInfo",
    "UpdateDownloader",
    "UpdateInstaller",
]
