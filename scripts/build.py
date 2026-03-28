#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
跨平台构建脚本：使用 PyInstaller 打包 Claude Partner 应用。

用法:
    python scripts/build.py

自动检测当前平台（Mac/Windows/Ubuntu），构建对应的可执行文件，
输出到 release/ 目录。
"""

import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path


def get_platform_name() -> str:
    """
    Business Logic（为什么需要这个函数）:
        构建产物需要按平台命名，方便用户识别下载对应版本。

    Code Logic（这个函数做什么）:
        检测当前操作系统，返回 'ubuntu'、'macos' 或 'windows'。
    """
    system: str = platform.system().lower()
    if system == "linux":
        return "ubuntu"
    elif system == "darwin":
        return "macos"
    elif system == "windows":
        return "windows"
    else:
        return system


def get_arch() -> str:
    """
    Business Logic（为什么需要这个函数）:
        同一操作系统可能有不同架构（x86_64/arm64），构建产物名应包含架构信息。

    Code Logic（这个函数做什么）:
        返回当前机器架构，如 'x86_64' 或 'arm64'。
    """
    machine: str = platform.machine().lower()
    if machine in ("x86_64", "amd64"):
        return "x86_64"
    elif machine in ("aarch64", "arm64"):
        return "arm64"
    return machine


def create_dmg(app_path: Path, dmg_path: Path) -> None:
    """
    Business Logic（为什么需要这个函数）:
        macOS 用户习惯通过 DMG 安装应用，将 .app 拖入 Applications 文件夹。

    Code Logic（这个函数做什么）:
        使用 hdiutil 创建 DMG 安装镜像，内含 .app 和 Applications 符号链接。
        1. 创建临时目录，放入 .app 和 Applications 快捷方式
        2. 用 hdiutil 打包为 DMG
    """
    staging_dir: Path = app_path.parent / "_dmg_staging"
    if staging_dir.exists():
        shutil.rmtree(staging_dir)
    staging_dir.mkdir()

    # 复制 .app 到临时目录（保留符号链接，否则会破坏 bundle 结构）
    staged_app: Path = staging_dir / app_path.name
    shutil.copytree(app_path, staged_app, symlinks=True)

    # 创建 Applications 符号链接
    os.symlink("/Applications", staging_dir / "Applications")

    # 删除已有 DMG
    if dmg_path.exists():
        dmg_path.unlink()

    # 创建 DMG
    subprocess.run(
        [
            "hdiutil", "create",
            "-volname", "Claude Partner",
            "-srcfolder", str(staging_dir),
            "-ov",
            "-format", "UDZO",  # 压缩格式
            str(dmg_path),
        ],
        check=True,
    )

    # 清理临时目录
    shutil.rmtree(staging_dir)
    print(f"DMG: {dmg_path}")


def main() -> None:
    """
    Business Logic（为什么需要这个函数）:
        用户需要一键执行构建，自动处理图标生成、PyInstaller 打包和产物整理。

    Code Logic（这个函数做什么）:
        1. 生成图标（如果不存在）
        2. 调用 PyInstaller 执行 spec 文件
        3. 将产物移动到 release/ 并按平台+架构重命名
        4. macOS 额外生成 DMG 安装镜像
    """
    project_dir: Path = Path(__file__).parent.parent
    spec_file: Path = project_dir / "claude_partner.spec"
    release_dir: Path = project_dir / "release"
    scripts_dir: Path = project_dir / "scripts"

    # 确保 release 目录存在
    release_dir.mkdir(exist_ok=True)

    # 1. 生成图标（如果还没有）
    icon_png: Path = scripts_dir / "icon.png"
    if not icon_png.exists():
        print("生成图标...")
        subprocess.run(
            [sys.executable, str(scripts_dir / "generate_icon.py")],
            check=True,
        )

    # 2. 调用 PyInstaller
    print(f"开始构建 ({get_platform_name()} {get_arch()})...")
    dist_dir: Path = project_dir / "dist"
    build_dir: Path = project_dir / "build"

    cmd: list[str] = [
        sys.executable, "-m", "PyInstaller",
        str(spec_file),
        "--distpath", str(dist_dir),
        "--workpath", str(build_dir),
        "--noconfirm",
    ]

    result = subprocess.run(cmd, cwd=str(project_dir))
    if result.returncode != 0:
        print("构建失败！")
        sys.exit(1)

    # 3. 移动产物到 release/
    platform_name: str = get_platform_name()
    arch: str = get_arch()
    system: str = platform.system().lower()

    if system == "darwin":
        # macOS: .app bundle + DMG 安装镜像
        app_src: Path = dist_dir / "ClaudePartner.app"
        if app_src.exists():
            dmg_name: str = f"ClaudePartner-{platform_name}-{arch}.dmg"
            dmg_path: Path = release_dir / dmg_name
            create_dmg(app_src, dmg_path)

    elif system == "windows":
        exe_src: Path = dist_dir / "ClaudePartner.exe"
        if exe_src.exists():
            exe_dst: Path = release_dir / f"ClaudePartner-{platform_name}-{arch}.exe"
            shutil.copy2(exe_src, exe_dst)
            print(f"产物: {exe_dst}")

    else:
        # Linux
        exe_src: Path = dist_dir / "ClaudePartner"
        if exe_src.exists():
            exe_dst: Path = release_dir / f"ClaudePartner-{platform_name}-{arch}"
            shutil.copy2(exe_src, exe_dst)
            os.chmod(exe_dst, 0o755)
            print(f"产物: {exe_dst}")

    # 4. 清理临时目录
    if build_dir.exists():
        shutil.rmtree(build_dir)
    if dist_dir.exists():
        shutil.rmtree(dist_dir)

    print("构建完成！")


if __name__ == "__main__":
    main()
