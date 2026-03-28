# transfer/ - 文件传输模块

## 模块职责
负责局域网内设备间的文件分块传输，支持断点续传、SHA256 完整性校验和传输取消。

## 文件结构

| 文件 | 职责 |
|------|------|
| `__init__.py` | 模块入口，导出 FileSender 和 FileReceiver |
| `sender.py` | 文件发送器，负责计算哈希、初始化握手、逐块发送 |
| `receiver.py` | 文件接收器，负责接收数据块、校验哈希、保存文件 |

## 核心类

### FileSender (`sender.py`)
- **依赖**: `PeerClient`（网络客户端，用于 HTTP 通信）
- **信号**: `progress_updated(transfer_id, progress)`, `transfer_completed(transfer_id)`, `transfer_failed(transfer_id, error)`
- **流程**: 计算 SHA256 -> transfer_init 握手 -> 逐块发送（支持 resume_offset）-> 完成
- **取消机制**: 调用 `cancel(transfer_id)` 将 ID 加入取消集合，发送循环中检查

### FileReceiver (`receiver.py`)
- **依赖**: `AppConfig`（获取 receive_dir 等配置）
- **信号**: `progress_updated(transfer_id, progress)`, `transfer_completed(transfer_id, saved_path)`, `transfer_failed(transfer_id, error)`
- **流程**: `init_transfer`(创建任务+检查断点) -> `receive_chunk`(写入临时文件) -> `finalize_transfer`(SHA256 校验+重命名)
- **临时文件**: `{receive_dir}/.{transfer_id}.tmp`
- **文件名冲突**: 自动添加 `(1)`, `(2)` 后缀

## 传输协议
- 分块大小: 960KB (CHUNK_SIZE = 960 * 1024)，低于 aiohttp 默认 client_max_size(1MB) 以兼容未自定义限制的对端
- SHA256 计算: 8KB 块读取，不一次性载入内存
- 断点续传: init_transfer 检查临时文件大小作为 resume_offset
- 完成判定: `transferred_bytes >= size` 时自动调用 finalize
