# sync/ - Prompt 同步引擎

## 模块概述

基于向量时钟的跨设备 Prompt 数据同步，实现最终一致性。

## 文件说明

### vector_clock.py - 向量时钟
- `VectorClock`: 提供三个静态方法
  - `increment(clock, device_id)`: 递增指定设备计数器（不修改原始）
  - `merge(clock_a, clock_b)`: 合并两个时钟，每个 key 取最大值
  - `compare(clock_a, clock_b)`: 比较偏序关系
    - `greater`: a 每个分量 >= b 且至少一个 >
    - `less`: b 每个分量 >= a 且至少一个 >
    - `equal`: 完全相同
    - `concurrent`: 互有领先（冲突）

### merger.py - 冲突合并
- `PromptMerger`: 提供三个静态方法
  - `should_update(local, remote)`: 判断是否用 remote 覆盖 local
    - greater -> True, less -> False, concurrent -> LWW (比较 updated_at), equal -> False
  - `merge_prompt(local, remote)`: 合并两个版本，胜出方内容 + 合并后时钟
  - `diff_summaries(local_summary, remote_summary)`: 比较摘要列表，返回需要互相同步的 ID

### engine.py - 同步引擎
- `SyncEngine(QObject)`: 协调同步流程
  - `sync_with_peer(device)`: 与单个设备双向同步（pull + merge + push）
  - `sync_all(devices)`: 与所有在线设备同步
  - `stop()`: 关闭时调用的占位钩子（无异步任务需停止）
  - 信号: `sync_completed()`, `sync_error(str)`

## 同步策略
1. 每个 Prompt 携带向量时钟 `{device_id: counter}`
2. 本地修改时递增本设备计数器
3. 同步时比较向量时钟：严格领先则覆盖，并发则 LWW（Last-Writer-Wins）
4. 合并后的 Prompt 始终包含双方时钟的合并结果
5. 触发时机：UI 层手动调用 sync_with_peer / sync_all（由用户在 Prompt 管理面板点击"同步"按钮触发）

## 依赖关系
- 依赖: `models.prompt`, `config`, `storage.prompt_repo`, `network.client`
- 被依赖: `app.py`（启动 SyncEngine）, `network.protocol`（使用 VectorClock）
