# -*- coding: utf-8 -*-
"""更新下载模块：异步下载新版本安装包。"""

import aiohttp
import logging
from pathlib import Path
from PyQt6.QtCore import QObject, pyqtSignal

from claude_partner.config import CONFIG_DIR

logger: logging.Logger = logging.getLogger(__name__)

# 更新文件存放目录
UPDATES_DIR: Path = CONFIG_DIR / "updates"

# 下载 chunk 大小（字节）
CHUNK_SIZE: int = 64 * 1024  # 64KB

# 下载超时配置（秒）
TOTAL_TIMEOUT: int = 600  # 总超时 10 分钟
SOCK_READ_TIMEOUT: int = 120  # 单次读取超时 2 分钟


class UpdateDownloader(QObject):
    """
    异步更新下载器，支持进度报告和取消操作。

    Business Logic（为什么需要这个类）:
        新版本安装包可能较大（数十到数百 MB），需要流式下载并实时报告进度，
        同时支持用户中途取消下载。

    Code Logic（这个类做什么）:
        使用 aiohttp 流式读取，64KB chunk 分块写入临时文件（.downloading 后缀），
        下载完成后重命名为最终文件名。通过 Qt 信号实时报告下载进度百分比。
        session 超时 total=600, sock_read=120，适配大文件下载场景。
    """

    download_progress = pyqtSignal(float)  # 下载进度，0.0 ~ 1.0
    download_completed = pyqtSignal(str)  # 下载完成，传文件绝对路径
    download_failed = pyqtSignal(str)  # 下载失败，传错误信息

    def __init__(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化下载器，session 延迟到第一次下载时创建。

        Code Logic（这个函数做什么）:
            初始化 _session 和 _cancelled 状态标记。
        """
        super().__init__()
        self._session: aiohttp.ClientSession | None = None
        self._cancelled: bool = False

    async def _get_session(self) -> aiohttp.ClientSession:
        """
        Business Logic（为什么需要这个函数）:
            大文件下载需要较长的超时时间，与普通 API 请求的超时配置不同。

        Code Logic（这个函数做什么）:
            创建专用于下载的 ClientSession，超时配置为 total=600, sock_read=120。
            懒初始化模式，复用 session。
        """
        if self._session is None or self._session.closed:
            timeout: aiohttp.ClientTimeout = aiohttp.ClientTimeout(
                total=TOTAL_TIMEOUT, sock_read=SOCK_READ_TIMEOUT
            )
            self._session = aiohttp.ClientSession(timeout=timeout)
        return self._session

    async def download(self, url: str, filename: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户确认更新后，需要将新版本安装包下载到本地以供安装。

        Code Logic（这个函数做什么）:
            1. 创建 updates 目录
            2. 以 .downloading 后缀创建临时文件
            3. 流式读取远程文件，64KB chunk 写入
            4. 实时报告进度百分比（已下载 / 总大小）
            5. 下载完成后重命名为最终文件名
            6. 支持 cancel() 中途取消
            7. emit download_completed(file_path) 或 download_failed(error)
        """
        self._cancelled = False

        # 确保更新目录存在
        UPDATES_DIR.mkdir(parents=True, exist_ok=True)

        temp_path: Path = UPDATES_DIR / f"{filename}.downloading"
        final_path: Path = UPDATES_DIR / filename

        try:
            session: aiohttp.ClientSession = await self._get_session()
            async with session.get(url) as resp:
                if resp.status != 200:
                    error_msg: str = f"下载失败: HTTP {resp.status}"
                    logger.error(error_msg)
                    self.download_failed.emit(error_msg)
                    return

                total_size: int = int(resp.headers.get("Content-Length", "0"))
                downloaded: int = 0

                with open(temp_path, "wb") as f:
                    async for chunk in resp.content.iter_chunked(CHUNK_SIZE):
                        if self._cancelled:
                            logger.info("下载已取消: %s", filename)
                            # 清理临时文件
                            if temp_path.exists():
                                temp_path.unlink()
                            self.download_failed.emit("下载已被用户取消")
                            return

                        f.write(chunk)
                        downloaded += len(chunk)

                        if total_size > 0:
                            progress: float = downloaded / total_size
                            self.download_progress.emit(progress)

                # 下载完成，重命名为最终文件名
                if temp_path.exists():
                    # 如果已有旧文件，先删除
                    if final_path.exists():
                        final_path.unlink()
                    temp_path.rename(final_path)

                logger.info(
                    "下载完成: %s (%d bytes)", final_path, downloaded
                )
                self.download_completed.emit(str(final_path))

        except aiohttp.ClientError as e:
            error_msg: str = f"下载网络错误: {e}"
            logger.error("下载失败: %s", e, exc_info=True)
            # 清理临时文件
            if temp_path.exists():
                temp_path.unlink()
            self.download_failed.emit(error_msg)
        except OSError as e:
            error_msg: str = f"下载文件写入失败: {e}"
            logger.error("下载失败: %s", e, exc_info=True)
            # 清理临时文件
            if temp_path.exists():
                temp_path.unlink()
            self.download_failed.emit(error_msg)
        except Exception as e:
            error_msg: str = f"下载失败: {e}"
            logger.error("下载失败: %s", e, exc_info=True)
            # 清理临时文件
            if temp_path.exists():
                temp_path.unlink()
            self.download_failed.emit(error_msg)

    def cancel(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户可能改变主意不想更新，需要支持中途取消下载。

        Code Logic（这个函数做什么）:
            设置 _cancelled 标记为 True，download 方法在下一个 chunk
            读取时检测到标记后会停止下载并清理临时文件。
        """
        self._cancelled = True
        logger.info("用户请求取消下载")

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
            logger.info("UpdateDownloader session 已关闭")
