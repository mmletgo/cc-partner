# -*- coding: utf-8 -*-
"""更新安装模块：三平台（macOS/Windows/Linux）安装并重启应用。"""

import logging
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

logger: logging.Logger = logging.getLogger(__name__)


class UpdateInstaller:
    """
    三平台更新安装器，执行替换并重启应用。

    Business Logic（为什么需要这个类）:
        下载完新版本安装包后，需要用新版本替换当前运行的旧版本，
        然后重启应用。不同操作系统的安装流程完全不同（DMG/EXE/TAR.GZ）。

    Code Logic（这个类做什么）:
        提供静态方法 install_and_restart 作为入口，根据文件扩展名
        和操作系统自动选择对应的安装策略：
        - macOS (.dmg): 挂载 → 复制 .app → 卸载 → 启动
        - Windows (.exe): 写 CMD 脚本等待进程退出 → 覆盖 → 启动
        - Linux (.tar.gz): 解压 → 写 Shell 脚本等待进程退出 → 覆盖 → 启动
        所有策略在启动新进程后都调用 os._exit(0) 立即退出当前进程。
    """

    @staticmethod
    def install_and_restart(downloaded_file: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            下载完成后，用户确认安装，需要自动替换当前应用并重启。

        Code Logic（这个函数做什么）:
            入口方法，根据文件扩展名和操作系统分发到对应的安装方法：
            - .dmg → _install_macos
            - .exe → _install_windows
            - .tar.gz → _install_linux
            安装脚本启动后调用 os._exit(0) 退出当前进程。
        """
        file_path: Path = Path(downloaded_file)
        system: str = platform.system().lower()
        filename: str = file_path.name.lower()

        logger.info("开始安装更新: %s (系统: %s)", file_path, system)

        try:
            if system == "darwin" and filename.endswith(".dmg"):
                UpdateInstaller._install_macos(file_path)
            elif system == "windows" and filename.endswith(".exe"):
                UpdateInstaller._install_windows(file_path)
            elif system == "linux" and filename.endswith(".tar.gz"):
                UpdateInstaller._install_linux(file_path)
            else:
                logger.error(
                    "不支持的安装包格式或平台不匹配: %s (系统: %s)",
                    filename,
                    system,
                )
                return

            # 安装脚本已启动，退出当前进程
            logger.info("安装脚本已启动，退出当前进程")
            os._exit(0)

        except Exception as e:
            logger.error("安装更新失败: %s", e, exc_info=True)

    @staticmethod
    def _install_macos(dmg_path: Path) -> None:
        """
        Business Logic（为什么需要这个函数）:
            macOS 用户习惯通过 DMG 安装应用，需要自动挂载镜像并
            将 .app 复制到 /Applications 目录。

        Code Logic（这个函数做什么）:
            1. hdiutil attach -nobrowse 挂载 DMG（不弹出 Finder 窗口）
            2. 从挂载卷中找到 .app 并复制到 /Applications
            3. hdiutil detach 卸载 DMG
            4. open 命令启动新版本应用
        """
        # 挂载 DMG
        mount_cmd: list[str] = [
            "hdiutil", "attach", "-nobrowse", str(dmg_path)
        ]
        result: subprocess.CompletedProcess[str] = subprocess.run(
            mount_cmd, capture_output=True, text=True, timeout=60
        )
        if result.returncode != 0:
            logger.error("DMG 挂载失败: %s", result.stderr)
            return

        # 解析挂载点（hdiutil attach 输出最后一行的第一个字段）
        mount_point: str = ""
        for line in result.stdout.strip().split("\n"):
            parts: list[str] = line.split("\t")
            if len(parts) >= 3:
                mount_point = parts[2].strip()

        if not mount_point:
            logger.error("无法解析 DMG 挂载点")
            return

        logger.info("DMG 已挂载到: %s", mount_point)

        try:
            # 查找 .app
            mount_dir: Path = Path(mount_point)
            app_files: list[Path] = list(mount_dir.glob("*.app"))
            if not app_files:
                logger.error("DMG 中未找到 .app 文件")
                return

            app_src: Path = app_files[0]
            app_dst: Path = Path("/Applications") / app_src.name

            # 删除旧版本
            if app_dst.exists():
                shutil.rmtree(app_dst)

            # 复制新版本
            shutil.copytree(app_src, app_dst, symlinks=True)
            logger.info("已复制 %s 到 %s", app_src, app_dst)

        finally:
            # 卸载 DMG
            subprocess.run(
                ["hdiutil", "detach", mount_point],
                capture_output=True,
                timeout=30,
            )
            logger.info("DMG 已卸载")

        # 启动新版本
        subprocess.Popen(["open", str(app_dst)])
        logger.info("已启动新版本: %s", app_dst)

    @staticmethod
    def _install_windows(exe_path: Path) -> None:
        """
        Business Logic（为什么需要这个函数）:
            Windows 下正在运行的 EXE 文件无法被覆盖，
            需要先退出当前进程再用新文件替换。

        Code Logic（这个函数做什么）:
            写一个 .cmd 批处理脚本，逻辑为：
            1. tasklist 循环等待当前进程 PID 不再存在
            2. copy 新 EXE 覆盖旧 EXE
            3. start 启动新版本
            4. 删除自身脚本
            以 DETACHED_PROCESS 标志启动该脚本，确保不受父进程退出影响。
        """
        current_exe: str = sys.executable if getattr(sys, "frozen", False) else ""
        if not current_exe:
            # 开发模式下无法替换，直接用 exe_path 启动
            logger.warning("非打包模式，直接启动下载的 EXE")
            subprocess.Popen([str(exe_path)])
            return

        pid: int = os.getpid()
        exe_name: str = Path(current_exe).name

        # 写安装脚本到临时目录
        script_path: Path = Path(tempfile.gettempdir()) / "claude_partner_update.cmd"
        script_content: str = (
            "@echo off\n"
            "echo Claude Partner 正在更新...\n"
            f"set PID={pid}\n"
            f"set EXE_NAME={exe_name}\n"
            f"set NEW_EXE={exe_path}\n"
            f"set TARGET_EXE={current_exe}\n"
            "\n"
            ":wait\n"
            "tasklist /FI \"PID eq %PID%\" | find \"%PID%\" >nul\n"
            "if not errorlevel 1 (\n"
            "    timeout /t 1 /nobreak >nul\n"
            "    goto wait\n"
            ")\n"
            "\n"
            "copy /Y \"%NEW_EXE%\" \"%TARGET_EXE%\"\n"
            "start \"\" \"%TARGET_EXE%\"\n"
            "del \"%~f0\"\n"
        )
        script_path.write_text(script_content, encoding="utf-8")

        # DETACHED_PROCESS = 0x8，独立于父进程运行
        DETACHED_PROCESS: int = 0x8
        subprocess.Popen(
            [str(script_path)],
            creationflags=DETACHED_PROCESS,
            close_fds=True,
        )
        logger.info("Windows 更新脚本已启动: %s", script_path)

    @staticmethod
    def _install_linux(tar_path: Path) -> None:
        """
        Business Logic（为什么需要这个函数）:
            Linux 下打包为 tar.gz，需要解压后替换当前可执行文件。
            同样需要先等待当前进程退出。

        Code Logic（这个函数做什么）:
            1. 解压 tar.gz 到临时目录
            2. 找到可执行文件
            3. 写一个 shell 脚本：
               - kill -0 循环等待当前进程 PID 不再存在
               - cp 覆盖旧可执行文件
               - chmod +x 添加执行权限
               - nohup 后台启动新版本
               - 删除自身脚本
            4. 以 start_new_session 启动该脚本
        """
        current_exe: str = sys.executable if getattr(sys, "frozen", False) else ""
        if not current_exe:
            logger.warning("非打包模式，无法执行 Linux 安装")
            return

        pid: int = os.getpid()

        # 解压到临时目录
        extract_dir: Path = Path(tempfile.mkdtemp(prefix="claude_partner_update_"))
        with tarfile.open(tar_path, "r:gz") as tar:
            tar.extractall(extract_dir)

        # 查找可执行文件（通常在解压后的子目录中）
        new_exe: Path | None = None
        for root, dirs, files in os.walk(extract_dir):
            for fname in files:
                if fname == "ClaudePartner":
                    new_exe = Path(root) / fname
                    break
            if new_exe is not None:
                break

        if new_exe is None:
            logger.error("tar.gz 中未找到 ClaudePartner 可执行文件")
            shutil.rmtree(extract_dir, ignore_errors=True)
            return

        # 写安装脚本
        script_path: Path = Path(tempfile.gettempdir()) / "claude_partner_update.sh"
        script_content: str = (
            "#!/bin/bash\n"
            f"PID={pid}\n"
            f"NEW_EXE={new_exe}\n"
            f"TARGET_EXE={current_exe}\n"
            f"EXTRACT_DIR={extract_dir}\n"
            "\n"
            "# 等待当前进程退出\n"
            "while kill -0 $PID 2>/dev/null; do\n"
            "    sleep 1\n"
            "done\n"
            "\n"
            "# 替换可执行文件\n"
            "cp -f \"$NEW_EXE\" \"$TARGET_EXE\"\n"
            "chmod +x \"$TARGET_EXE\"\n"
            "\n"
            "# 清理解压目录\n"
            "rm -rf \"$EXTRACT_DIR\"\n"
            "\n"
            "# 启动新版本\n"
            "nohup \"$TARGET_EXE\" > /dev/null 2>&1 &\n"
            "\n"
            "# 删除自身\n"
            "rm -f \"$0\"\n"
        )
        script_path.write_text(script_content, encoding="utf-8")
        os.chmod(script_path, 0o755)

        # start_new_session 确保脚本不受父进程退出影响
        subprocess.Popen(
            [str(script_path)],
            start_new_session=True,
            close_fds=True,
        )
        logger.info("Linux 更新脚本已启动: %s", script_path)
