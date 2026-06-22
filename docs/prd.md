# Claude Partner - 产品需求文档 (PRD)

## 1. 产品概述

Claude Partner 是一款支持 Mac/Windows/Ubuntu 三端的桌面工具，设计用于 Claude Code 用户在局域网环境下的多设备协作。

### 1.1 目标用户
使用 Claude Code 进行开发的程序员，拥有多台局域网设备。

### 1.2 核心价值
- 在多台设备间快速传输文件
- 一键区域截图并粘贴到 Claude Code
- 集中管理常用 Prompt，跨设备同步

## 2. 功能需求

### 2.1 局域网文件传输

**描述**：在局域网内的多个设备间互传文件。

**功能点**：
- 选择在线设备作为传输目标
- 支持任意大小文件传输
- 分块传输（1MB/块），显示传输进度
- 断点续传：传输中断后可从已完成的位置继续
- SHA256 校验确保文件完整性
- 支持拖拽文件到应用窗口发起传输
- 文件接收后保存到用户配置的目录

### 2.2 区域截图

**描述**：用户触发截图后，框选屏幕区域，截图自动保存到系统剪贴板。

**功能点**：
- 通过快捷键或托盘菜单触发
- 全屏半透明遮罩覆盖
- 鼠标拖拽框选区域，选区内显示原始画面
- 松开鼠标后截取区域图片
- 自动复制到系统剪贴板
- ESC 键取消截图
- 截图后用户可直接 Ctrl+V 粘贴到 Claude Code

### 2.3 Prompt 管理

**描述**：提供文本记录管理功能，支持标签分类和筛选。

**功能点**：
- 创建 Prompt：标题 + 内容 + 标签
- 编辑已有 Prompt
- 删除 Prompt（软删除，用于同步）
- 一键复制 Prompt 内容到剪贴板
- 标签管理：添加/移除标签
- 按标签筛选 Prompt 列表
- 文本搜索（搜索标题和内容）
- 按创建时间/更新时间排序

### 2.4 设备自动发现与互联

**描述**：局域网内的 Claude Partner 实例自动发现彼此并建立连接。

**功能点**：
- 启动时自动注册 mDNS 服务
- 自动发现同一局域网内的其他实例
- 显示在线设备列表（设备名、IP、在线状态）
- 设备上线/下线实时通知
- 每个实例同时作为 HTTP 服务端和客户端

### 2.5 Prompt 跨设备同步

**描述**：Prompt 数据在所有连接的设备间自动同步。

**功能点**：
- 新设备上线时自动拉取/推送 Prompt
- 本地修改后自动同步到对端（500ms 防抖）
- 定时同步（每 30 秒）
- 向量时钟追踪版本，避免丢失更新
- 并发冲突采用 Last-Writer-Wins 策略
- 仅同步 Prompt 数据，不同步文件

## 3. 非功能需求

### 3.1 跨平台
- 支持 macOS、Windows、Ubuntu
- 使用 PyInstaller 打包为各平台独立可执行文件

### 3.2 性能
- 文件传输速度应充分利用局域网带宽
- UI 操作不应因网络/IO 阻塞而卡顿（异步架构）
- 截图操作响应时间 < 200ms

### 3.3 可靠性
- 文件传输支持断点续传
- 数据库使用 SQLite，数据持久化可靠
- 设备离线后重新上线，同步应能恢复

## 4. 技术架构

### 4.1 技术栈
- 桌面宿主：Tauri 2（Rust 主进程）
- 语言：Rust（后端）+ TypeScript（前端）
- 网络：axum（HTTP 服务端，跨设备 P2P）+ reqwest（peer client）
- 发现：mdns-sd（mDNS）
- 存储：SQLite + sqlx
- 抓屏/剪贴板：xcap + arboard
- 通信：Tauri `invoke()` IPC（本地前端 ↔ Rust）
- 打包/更新：Tauri CLI + tauri-plugin-updater

### 4.2 数据模型

#### Prompt
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 主键 |
| title | str | 标题 |
| content | str | 内容 |
| tags | list[str] | 标签列表 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |
| device_id | str | 创建设备 ID |
| vector_clock | dict[str, int] | 向量时钟 |
| deleted | bool | 软删除标记 |

#### Device
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 设备唯一 ID |
| name | str | 设备名称 |
| host | str | IP 地址 |
| port | int | HTTP 端口 |
| last_seen | datetime | 最后在线时间 |
| online | bool | 是否在线 |

### 4.3 网络协议

#### mDNS 服务
- 类型：`_claude-partner._tcp.local.`
- TXT 记录：device_id, device_name

#### HTTP API
| 方法 | 路径 | 说明 |
|------|------|------|
| GET | /api/health | 健康检查 |
| POST | /api/sync/pull | 拉取 Prompt（含向量时钟摘要） |
| POST | /api/sync/push | 推送 Prompt |
| POST | /api/transfer/init | 发起文件传输 |
| POST | /api/transfer/chunk/{id} | 发送文件块 |
| GET | /api/transfer/status/{id} | 查询传输状态 |
