# Sync Integrity, Conflict and Recovery 设计

- 日期：2026-07-14
- 状态：已确认
- 依赖：N1 owner/control plane；复用现有 vector clock、CC History bounded sync 与 cloud snapshot

## 1. 问题

Prompt、SSH target 与 Scratchpad 的 peer client 会把网络、HTTP 或 JSON 错误折叠为空列表。同步引擎随后把“空”解释为远端不存在，重复推送全部本地数据；单条 push 失败只记录 warning，外层仍可能增加成功计数。批量仓储写入不是统一事务，LWW 会让用户看不到被覆盖版本，软删除 tombstone 也没有安全回收协议。用户缺少完整、可验证的导出与恢复入口。

## 2. 目标

1. 三个领域使用 typed sync result，严格区分成功空集、不可达、协议失败、资源超限与部分失败。
2. 引入无状态、有界、按 id 稳定排序的 manifest-page/items/push-batch 交换；client 完整流式比较双方 manifest，精确相等时零正文 push。
3. 单批数据库合并事务化，partial failure 不计成功。
4. 对并发版本保留 conflict copy；Scratchpad/Prompt 提供有限版本历史。
5. 使用 per-device acknowledgement watermark 安全回收 tombstone。
6. 提供不包含项目源码和凭据的导出包、恢复预览、校验与事务恢复。

## 3. 非目标

- 不改写 CC History 已有分页协议。
- 不实现实时协同编辑、CRDT 文本合并或云端账号服务。
- 不导出 Workbench 项目文件、终端 transcript、SSH 私钥、token 或 lifecycle control token。
- 不在网络错误时自动选择本地或远端版本覆盖另一方。

## 4. 协议与领域结果

### 4.1 Typed result

```rust
pub enum SyncDomainOutcome {
    Succeeded { pulled: u32, pushed: u32, unchanged: u32 },
    Partial { applied: u32, failed: Vec<SyncItemFailure> },
    Unreachable { class: TransportClass },
    ProtocolError { code: String },
    ResourceLimit { limit: String },
}
```

只有 `Succeeded` 计入 synced device/domain；`Partial` 必须在 UI 和日志显示，不得转成 `Ok(())`。transport/HTTP/JSON 错误不得返回空 manifest。

### 4.2 无状态 Manifest page、items 与 batch

- capability：`sync.manifest.v2`；旧 peer 继续 legacy 路径，但仍返回 typed success/error。
- manifest item 只包含 id、vector clock、updated/deleted metadata、content hash 与 size，并按 id 稳定排序。
- server 只提供 `manifest-page(cursor,limit)`、`items(ids)` 与 `push-batch(items,clientRequestId)` 三类无状态操作；不根据 caller 的单页 manifest 推断 caller 缺失项。
- client 用 opaque keyset cursor 拉完/流式 merge 全部 remote pages，再与完整 local manifest 比较；page 最大 500 item、序列化体最大 1 MiB，正文 batch 最大 100 item 或 4 MiB，先达到者为准。
- exact manifest equality 不请求正文、不 push。
- batch 使用稳定 `clientRequestId`；server 在写事务中以 `UNIQUE(claimedDeviceId,domain,clientRequestId)` 保存 payload hash + outcome ledger。相同 key/hash 返回原 outcome，不重复写 conflict；相同 key/不同 hash 返回 conflict。`claimedDeviceId` 只是收敛标签，不是认证身份。

### 4.3 合并、冲突与历史

- 向量时钟可排序时沿用当前胜者。
- 并发且正文不同，保留 winner，同时写入 `content_versions` conflict copy，记录 domain/item/sourceDevice/hash/createdAt。
- Prompt 与 Scratchpad 每项保留最近 20 个版本或 30 天，先达到者为准；冲突副本至少保留 30 天。
- UI 允许查看版本摘要、恢复为新版本和复制冲突内容，不提供逐行三方合并编辑器。

### 4.4 Tombstone GC

每个 domain 有持久单调 `deleteEpoch`；本地删除或首次接纳远端 tombstone 时在同一事务中递增并写入该 tombstone。manifest page 携带 tombstone/floor 的 `deleteEpoch`，client 只有在拉完一个完整 manifest、应用全部 delete/floor 且正文 batch 成功后，才回传该 domain 的最高连续 `ackedDeleteEpoch`；server 按 peer/domain 持久化。只有 tombstone 已超过 30 天且所有最近 90 天内活跃 peer 的 ack ≥ tombstone epoch，才可在同一事务中删除完整 tombstone 并写入 lightweight `sync_deletion_floors(domain,itemId,deleteVectorClock,deleteEpoch,hash)`。deletion floor 不因普通 GC 删除；旧 live row 被 floor 支配时拒绝复活并回送 delete，clock 并发时保留冲突副本但 active row 仍 delete-wins。只有未来显式的 dataset reset/peer retirement 方案才能清 floor，本轮不实现。长期离线 peer 再上线时完整 manifest 对账也必须应用 floor。

## 5. 导出与恢复

### 5.1 导出包

ZIP 包含 versioned manifest、Prompt、CC History metadata/content、Scratchpad、SSH target（当前模型仅 host/port/username/label，不含认证材料）、CLAUDE.md metadata、deletion floors、应用非敏感配置的只读 report 与每个文件 SHA-256。配置 report 用于预览，不参与 restore，避免跨 JSON 文件与 SQLite 伪装单事务。导出默认保存到用户选择位置，不上传网络。恢复器流式读取，限制 archive ≤2 GiB、entry ≤100,000、单 entry 解压后 ≤64 MiB、总解压量 ≤4 GiB，并拒绝 zip-slip、符号链接、绝对路径和超限压缩包。

### 5.2 恢复流程

```text
选择包 → 校验版本/哈希 → 生成只读预览 → 用户选择 merge/replace-domain →
进入 sidecar maintenance gate → 创建自动恢复前备份 → 单个 SQLite 事务导入 → 领域重建索引 → 展示结果
```

- `merge` 使用 vector clock/conflict copy 规则。
- `replace-domain` 仅替换用户明确勾选的领域，不覆盖运行时 owner、项目源码或凭据。
- backup inspect/restore/list/rollback 只在 N1 sidecar owner 上执行；GUI 只负责原生文件选择并通过 loopback control client 代理。`AppState` 共享全局 DB maintenance 读写屏障，并提供唯一生产写事务构造器：普通 writer 由 shared lease 生成 `DatabaseWritePermit`，restore 由已持有的 exclusive lease 生成 maintenance permit；`begin_write_with_permit` 接受任一 permit 且 exclusive 路径绝不重入 shared lock。Prompt、Scratchpad、SSH、CC History、CLAUDE.md、sync/cloud、Transfer、Workbench、Orchestrator、health/background、LAN 与未来新增的 SQLite writer 都必须从该入口开事务，permit 覆盖完整 commit/rollback；CI writer inventory 拒绝 raw write begin/execute。restore 从恢复前备份开始到 replace/merge commit 与索引重建结束持独占 lease。config report 永不写回。
- 任一事务失败回滚；恢复前备份写入应用数据目录并使用当前平台可提供的用户私有权限，保留 7 天且最多 3 份，只有新备份完整落盘后才清理旧备份，并可一键回退。

## 6. 用户表面

- Settings 同步 tab 展示每个 device/domain 的 `succeeded/partial/unreachable/protocol/resource-limit` 与 pulled/pushed/unchanged 数量。
- Prompt/Scratchpad 详情提供“版本历史”，冲突用非阻塞 Pill 标识。
- Settings 关于或同步 tab 提供“导出数据”“从备份恢复”；恢复必须使用 Dialog/Drawer 的现有焦点合同。
- 所有错误文案可重试且不显示 token、仓库凭据或正文。

## 7. 事务、兼容与回滚

- 新表通过 `backend/runtime.rs::init_db` 的幂等 runtime schema 与 repo helper 创建：`content_versions`、`sync_peer_watermarks`、`sync_domain_delete_sequences`、`sync_deletion_floors`、`sync_request_ledger`、`recovery_jobs`；`migrations/0001_init.sql` 仅同步作为 schema 文档，不启用 `sqlx::migrate!`。
- 新旧 peer 混合时不发送 v2 batch；legacy 失败仍不得假成功。
- 回滚停止读写新元数据表并保留表数据，不能在自动降级中删除用户历史；真正删除只允许显式维护版本。
- 导出 schema 带 `formatVersion`；未来版本不支持时在预览前拒绝，不做 best-effort 半恢复。

## 8. 测试与验收

1. network/HTTP/invalid JSON 分别返回 typed failure，push 数量保持 0。
2. exact equality 在三个领域均为 pulled=0/pushed=0/unchanged=N。
3. 413、batch 中途断开与数据库注入失败不产生半批次成功。
4. 并发向量时钟产生 conflict copy，恢复版本会生成新 vector clock 而非覆盖历史。
5. tombstone 仅在满足 age + active peer watermark 后压缩为 deletion floor；离线 180 天 peer 携带旧 live row 回归仍不会复活删除项。
6. 导出包哈希篡改、未知版本、zip-slip、体量超限与事务失败均在修改数据库前/中安全终止；crash 后 `recovery_jobs` 可判定/恢复，config report 不被写回。
7. 恢复后再导出可通过语义等价校验；secret scanner 确认无排除项。

## 9. 持久文档

实现时更新 `docs/prd.md` 的同步、版本与恢复行为，更新 `docs/p2p-protocol.md`、`src-tauri/CLAUDE.md`、测试矩阵和用户向恢复说明。

## 10. Spec 自审

- CC History v1 能力不重复实现；新协议只覆盖 Prompt/SSH/Scratchpad。
- 冲突、历史、GC、导出与恢复均有确定期限和上限。
- 没有把网络错误解释为成功空集，也没有导出敏感排除项。
