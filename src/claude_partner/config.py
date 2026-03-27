# -*- coding: utf-8 -*-
"""配置管理模块：负责应用配置的加载、保存和默认值生成。"""

from dataclasses import dataclass, field
import json
import uuid
import socket
from pathlib import Path


# 配置文件和数据文件的根目录
CONFIG_DIR: Path = Path.home() / ".claude-partner"
CONFIG_FILE: Path = CONFIG_DIR / "config.json"
DEFAULT_DB_PATH: str = str(CONFIG_DIR / "data.db")
DEFAULT_RECEIVE_DIR: str = str(Path.home() / "ClaudePartnerFiles")


@dataclass
class AppConfig:
    """
    应用全局配置数据类。

    Business Logic（为什么需要这个类）:
        应用需要在多次运行间保持一致的设备标识和用户偏好设置，
        例如设备 ID、设备名称、端口号、文件接收目录等。

    Code Logic（这个类做什么）:
        封装所有配置字段，提供从 JSON 文件加载和保存的能力。
        首次运行时自动生成默认配置并持久化。
    """

    device_id: str
    device_name: str
    http_port: int  # 0 = 系统自动分配
    receive_dir: str
    db_path: str
    screenshot_hotkey: str = "<ctrl>+<shift>+s"

    @classmethod
    def load(cls) -> "AppConfig":
        """
        Business Logic（为什么需要这个函数）:
            应用启动时需要读取上次保存的配置，保证设备 ID 等信息跨会话一致。
            如果配置文件不存在（首次运行），则创建默认配置并写入磁盘。

        Code Logic（这个函数做什么）:
            从 ~/.claude-partner/config.json 读取配置并反序列化为 AppConfig。
            文件不存在时生成默认值（UUID 作为设备 ID、hostname 作为设备名称）并保存。
        """
        if CONFIG_FILE.exists():
            data: dict = json.loads(CONFIG_FILE.read_text(encoding="utf-8"))
            config = cls(
                device_id=data["device_id"],
                device_name=data["device_name"],
                http_port=data["http_port"],
                receive_dir=data["receive_dir"],
                db_path=data["db_path"],
                screenshot_hotkey=data.get("screenshot_hotkey", "<ctrl>+<shift>+s"),
            )
            return config

        # 首次运行，生成默认配置
        config = cls(
            device_id=str(uuid.uuid4()),
            device_name=socket.gethostname(),
            http_port=0,
            receive_dir=DEFAULT_RECEIVE_DIR,
            db_path=DEFAULT_DB_PATH,
        )
        config.save()
        return config

    def save(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户修改配置后需要持久化到磁盘，下次启动时能读取到最新设置。

        Code Logic（这个函数做什么）:
            将当前配置序列化为 JSON 并写入 ~/.claude-partner/config.json。
            如果目录不存在则自动创建。
        """
        CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        data: dict = {
            "device_id": self.device_id,
            "device_name": self.device_name,
            "http_port": self.http_port,
            "receive_dir": self.receive_dir,
            "db_path": self.db_path,
            "screenshot_hotkey": self.screenshot_hotkey,
        }
        CONFIG_FILE.write_text(
            json.dumps(data, ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
