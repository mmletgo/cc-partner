# -*- coding: utf-8 -*-
"""版本检查模块：从 GitHub Releases API 检测新版本。"""

import aiohttp
import logging
import platform
import re
import sys
from dataclasses import dataclass
from PyQt6.QtCore import QObject, pyqtSignal

logger: logging.Logger = logging.getLogger(__name__)

# GitHub Releases API 地址
RELEASE_API_URL: str = (
    "https://api.github.com/repos/mmletgo/claude-partner/releases/latest"
)

# 默认请求超时（秒）
DEFAULT_TIMEOUT: int = 15

# 自动更新检查间隔（秒）：4 小时
UPDATE_CHECK_INTERVAL: int = 4 * 3600


@dataclass
class SemanticVersion:
    """
    语义化版本号，用于版本比较。

    Business Logic（为什么需要这个类）:
        自动更新需要判断远程版本是否比当前版本更新，
        需要解析和比较语义化版本号（major.minor.patch）。

    Code Logic（这个类做什么）:
        解析形如 "1.2.3" 的版本字符串，支持 major/minor/patch 三段比较。
        不依赖 packaging 库，纯标准库实现。
    """

    major: int
    minor: int
    patch: int

    @classmethod
    def parse(cls, version_str: str) -> "SemanticVersion":
        """
        Business Logic（为什么需要这个函数）:
            从版本字符串（如 "v1.2.3" 或 "1.2.3"）解析出结构化的版本号。

        Code Logic（这个函数做什么）:
            去除前缀 'v'，按 '.' 分割，解析 major/minor/patch。
            缺失的段默认为 0。无法解析时返回 0.0.0。
        """
        cleaned: str = version_str.strip().lstrip("v")
        match: re.Match[str] | None = re.match(
            r"^(\d+)(?:\.(\d+))?(?:\.(\d+))?", cleaned
        )
        if match is None:
            logger.warning("无法解析版本号: %s", version_str)
            return cls(major=0, minor=0, patch=0)
        return cls(
            major=int(match.group(1)),
            minor=int(match.group(2) or "0"),
            patch=int(match.group(3) or "0"),
        )

    def __gt__(self, other: "SemanticVersion") -> bool:
        """
        Business Logic（为什么需要这个函数）:
            判断远程版本是否比当前版本更新，决定是否提示用户更新。

        Code Logic（这个函数做什么）:
            依次比较 major、minor、patch，任意一段更大即返回 True。
        """
        if self.major != other.major:
            return self.major > other.major
        if self.minor != other.minor:
            return self.minor > other.minor
        return self.patch > other.patch

    def __eq__(self, other: object) -> bool:
        """
        Business Logic（为什么需要这个函数）:
            判断两个版本号是否完全相同，避免重复提示已是最新版。

        Code Logic（这个函数做什么）:
            比较 major、minor、patch 三段是否全部相等。
        """
        if not isinstance(other, SemanticVersion):
            return NotImplemented
        return (
            self.major == other.major
            and self.minor == other.minor
            and self.patch == other.patch
        )

    def __repr__(self) -> str:
        """返回版本号的字符串表示。"""
        return f"{self.major}.{self.minor}.{self.patch}"


@dataclass
class UpdateInfo:
    """
    更新信息，封装从 GitHub Release 获取的新版本详情。

    Business Logic（为什么需要这个类）:
        UI 层需要展示新版本的信息（版本号、更新说明），
        下载器需要下载地址和文件大小来执行下载。

    Code Logic（这个类做什么）:
        纯数据容器，包含 version、html_url、body、download_url、
        download_filename、download_size 六个字段。
    """

    version: str
    html_url: str
    body: str
    download_url: str
    download_filename: str
    download_size: int


class UpdateChecker(QObject):
    """
    版本检查器，从 GitHub Releases API 检测新版本。

    Business Logic（为什么需要这个类）:
        用户需要知道是否有新版本可用，以便及时获取功能改进和缺陷修复。
        通过查询 GitHub Releases API 获取最新发布版本信息。

    Code Logic（这个类做什么）:
        异步调用 GitHub API 获取最新 release 信息，
        解析版本号并与当前版本比较，通过 Qt 信号通知 UI 层。
        aiohttp session 采用懒初始化模式（复用 PeerClient 的设计）。
    """

    update_available = pyqtSignal(object)  # 有新版本，传 UpdateInfo 对象
    update_not_available = pyqtSignal()  # 已是最新版本
    check_failed = pyqtSignal(str)  # 检查失败，传错误信息

    def __init__(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化检查器，session 延迟到第一次请求时创建。

        Code Logic（这个函数做什么）:
            初始化 _session 为 None，实际的 ClientSession 在
            _get_session 中懒创建。
        """
        super().__init__()
        self._session: aiohttp.ClientSession | None = None

    async def _get_session(self) -> aiohttp.ClientSession:
        """
        Business Logic（为什么需要这个函数）:
            aiohttp.ClientSession 需要在运行中的事件循环内创建，
            且应复用同一个 session 以提升连接效率。

        Code Logic（这个函数做什么）:
            如果 _session 为 None 或已关闭，则创建新的 ClientSession
            （带 15 秒超时配置）。否则返回现有的。
        """
        if self._session is None or self._session.closed:
            timeout: aiohttp.ClientTimeout = aiohttp.ClientTimeout(
                total=DEFAULT_TIMEOUT
            )
            self._session = aiohttp.ClientSession(timeout=timeout)
        return self._session

    async def check_for_update(self, current_version: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用启动或用户手动触发时，检查是否有新版本可用，
            以便提示用户升级。

        Code Logic（这个函数做什么）:
            1. GET GitHub Releases API 获取最新 release
            2. 解析 tag_name 为 SemanticVersion，与 current_version 比较
            3. 如果有新版本，从 assets 中匹配当前平台的下载文件
            4. emit update_available(update_info) 或 update_not_available()
            5. 失败时 emit check_failed(error_msg)
        """
        try:
            session: aiohttp.ClientSession = await self._get_session()
            async with session.get(RELEASE_API_URL) as resp:
                if resp.status != 200:
                    error_msg: str = f"GitHub API 返回 HTTP {resp.status}"
                    logger.warning("版本检查失败: %s", error_msg)
                    self.check_failed.emit(error_msg)
                    return

                data: dict = await resp.json()

            # 解析版本号
            tag_name: str = data.get("tag_name", "")
            remote_version: SemanticVersion = SemanticVersion.parse(tag_name)
            local_version: SemanticVersion = SemanticVersion.parse(
                current_version
            )

            logger.info(
                "版本检查: 本地=%s, 远程=%s", local_version, remote_version
            )

            if not (remote_version > local_version):
                self.update_not_available.emit()
                return

            # 匹配当前平台的下载资源
            platform_suffix: str = self._get_platform_suffix()
            assets: list[dict] = data.get("assets", [])

            download_url: str = ""
            download_filename: str = ""
            download_size: int = 0

            for asset in assets:
                name: str = asset.get("name", "")
                if platform_suffix in name:
                    download_url = asset.get("browser_download_url", "")
                    download_filename = name
                    download_size = asset.get("size", 0)
                    break

            if not download_url:
                error_msg: str = (
                    f"在 release 资产中未找到匹配平台 '{platform_suffix}' 的文件"
                )
                logger.warning(error_msg)
                self.check_failed.emit(error_msg)
                return

            update_info: UpdateInfo = UpdateInfo(
                version=tag_name,
                html_url=data.get("html_url", ""),
                body=data.get("body", ""),
                download_url=download_url,
                download_filename=download_filename,
                download_size=download_size,
            )

            logger.info("发现新版本: %s (%s)", tag_name, download_filename)
            self.update_available.emit(update_info)

        except aiohttp.ClientError as e:
            error_msg: str = f"网络请求失败: {e}"
            logger.error("版本检查异常: %s", e, exc_info=True)
            self.check_failed.emit(error_msg)
        except Exception as e:
            error_msg: str = f"版本检查失败: {e}"
            logger.error("版本检查异常: %s", e, exc_info=True)
            self.check_failed.emit(error_msg)

    @staticmethod
    def _get_platform_suffix() -> str:
        """
        Business Logic（为什么需要这个函数）:
            GitHub Release 包含多平台的安装包，需要自动匹配当前平台
            的下载文件，避免用户手动选择。

        Code Logic（这个函数做什么）:
            根据当前操作系统和 CPU 架构，返回与 build.py 命名一致的
            平台匹配关键字：
            - macOS arm64: "macos-arm64"
            - macOS x86_64: "macos-x86_64"
            - Windows: "windows-x86_64"
            - Linux: "ubuntu-x86_64" 或 "ubuntu-aarch64"
        """
        system: str = sys.platform
        machine: str = platform.machine().lower()

        if system == "darwin":
            arch: str = "arm64" if machine == "arm64" else "x86_64"
            return f"macos-{arch}"
        elif system == "win32":
            return "windows-x86_64"
        else:
            # Linux
            if machine in ("aarch64", "arm64"):
                return "ubuntu-aarch64"
            return "ubuntu-x86_64"

    async def close(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用关闭时需要释放 HTTP 连接资源。

        Code Logic（这个函数做什么）:
            关闭 aiohttp.ClientSession，释放底层 TCP 连接。
        """
        if self._session is not None and not self._session.closed:
            await self._session.close()
            self._session = None
            logger.info("UpdateChecker session 已关闭")
