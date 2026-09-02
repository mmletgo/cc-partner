# PC → 移动端文件传输（本机邮箱 / 零拷贝）

- 日期：2026-09-02
- 状态：已实现
- 依赖：现有 `send_transfer` 幂等合同、Transfer 任务模型、`GET /api/mobile/transfer/download/:taskId`、mobile Transfer 动作矩阵

## 1. 问题

手机可以把文件经主机中转发给本机或局域网对端；电脑只能把文件发给其他 P2P 电脑。手机浏览器不是 mDNS/P2P 节点，不能当 `send_transfer` 的对端。结果是：人在电脑前选好文件，没法发给正在（或稍后）使用本机 `/mobile` 的手机。

## 2. 目标

1. 电脑传输页可以把本机文件发给「手机」：发送时不要求手机在线。
2. 发出去后任务立刻完成，挂在本机传输历史上；任意稍后打开本机 `/mobile` 传输页的人可以点下载。
3. 不拷贝快照，下载时流式读取发送时登记的原路径。
4. 手机 JSON / 错误信封永不返回主机路径。
5. 不把「手机」混进全局设备发现列表（Home / Agent Hub 等仍只看到局域网电脑）。

## 3. 非目标

- 不拷贝到 `receive_dir` 或 staging；不在发送时计算整文件 SHA256。
- 不跟踪 `/mobile` 会话在线、不自动开始下载、不弹系统分享表。
- 不支持从电脑 A 发给连在电脑 B 上的手机。
- 不把手机变成 P2P 节点，不新增分块协议。
- 不给桌面这条任务 Open/Reveal；不把方向伪装成 Receive。
- 不改 LAN 无身份鉴权边界：能打开本机 `/mobile` 的人都能下载已挂出的文件。
- 不新增下载 Range/断点；与现有 Receive 下载同一条流式响应。

## 4. 产品行为

### 4.1 电脑

- 传输页目标下拉框**最上方**固定一项「手机」，id 为稳定常量 `cc-partner-mobile-inbox`，始终可发送，不依赖 mDNS。首次进入页面默认选中「手机」。
- 选文件、点发送的流程与发给电脑相同（同一发送按钮、同一 `clientOperationId` 门闩）。
- 选中「手机」时给一句提示：文件仍留在原位置，若之后移动或删除，手机将无法下载。
- 成功后任务为 `direction=Send`、`status=completed`、`peerDeviceId=cc-partner-mobile-inbox`，对端展示名「手机」。无 Cancel / Resume；失败才可能 Retry。
- 无局域网对端时仍可发给手机（不再因为设备列表为空而无法发送）。

### 4.2 手机

- 传输任务列表就是主机任务列表（现有合同）。发给手机的任务方向仍为 Send，不改成 Receive。
- Download 按钮在「Receive+completed」之外，对「Send+completed+peer=手机」同样显示。
- 点下载走现有 `GET /api/mobile/transfer/download/:taskId`，浏览器按 `Content-Disposition` 保存。
- 手机目标下拉**不出现**「手机」；继续只列「这台电脑」+ 局域网对端。

### 4.3 邮箱语义

- 同一文件可被多次下载，直到源文件不可用。
- 同一 `clientOperationId` + 同一 payload（源路径 + 手机目标）回放已有任务，不重复挂出。
- 历史保留与其它传输任务相同，不做单独过期 GC。

## 5. 架构

```
电脑 Transfer
  下拉框注入虚拟目标「手机」（仅本页）
  → send_transfer(deviceId=cc-partner-mobile-inbox, filePath, clientOperationId)
  → sidecar start_sending 命中 inbox 分支（不 lookup devices 表、不 spawn P2P）
  → 校验源文件为普通文件 → claim 幂等 → 记 Send+completed → emit transfer:completed

手机 /mobile 传输页
  → 列表看到该任务（无 path）
  → GET /api/mobile/transfer/download/:taskId
  → 仅当 Receive+completed 或 inbox offer 且源仍是普通文件、size 仍匹配
  → octet-stream；任何失败 404 泛化文案，不含 path
```

权威任务只有一条，存在主机 transfer registry/history。电脑 DTO 含 path（现有桌面合同）；mobile DTO 继续剥离 path。

虚拟目标**不写入** `AppState.devices`，不出现在 `list_devices` / mDNS。仅：

- 前端 Transfer 页注入选项（与后端常量同一 id）
- `start_sending` / retry 以该 id 走 offer 分支
- download / 前端 Download 判定认这个 peer id

## 6. 接口与实现边界

### 6.1 常量

```
MOBILE_INBOX_DEVICE_ID = "cc-partner-mobile-inbox"
```

Rust 与 `web/` 各声明一次，测试锁定字符串相等。禁止用本机 `device_id` 冒充（那是手机侧的「这台电脑」）。

### 6.2 `start_sending`

在 peer lookup 之前：

1. `device_id == MOBILE_INBOX_DEVICE_ID` → `register_mobile_inbox_offer`。
2. 否则保持现有 P2P 发送。

`register_mobile_inbox_offer`：

1. 拒绝空 `clientOperationId`、空路径；路径按不透明 UTF-8，不做分隔符改写。
2. `symlink_metadata`：必须是绝对路径、普通文件、非符号链接、非目录；否则 typed 校验错误（桌面 invoke 可含 basename，不得把完整 path 写进会到 mobile JSON 的字段）。
3. **不**计算 SHA256（大文件发送必须立刻返回）。`sha256` 存空字符串；`size` 为当时 `len()`。
4. `canonical_send_payload_hash(sourcePath, peerDeviceId)` 与现网发送相同（kind=send，不含随机 UUID）。
5. `claim_sender_operation`；Fresh：写入 `Send + completed + transferred_bytes=size + phase=Completed`，registry/history 记录，emit `transfer:completed`，返回 task id。Replay 终态直接返回 id；inbox offer 不会处于非终态，若 Replay 到非终态视为实现错误并 fail-closed。Conflict → `operationIdConflict`。
6. 不调用 `spawn_claimed_send`、`peer_client`、`receive_dir`。

### 6.3 retry / resume

- `resume_transfer`：inbox 任务（`peer_device_id == MOBILE_INBOX_DEVICE_ID`）一律 unsupported / 不可续传（无 chunk checkpoint）。
- `retry_transfer`：仅当该 inbox 任务 `failed + retryable` 时，对同一源路径重新走 `register_mobile_inbox_offer`（可 mint 新 attempt id，payload hash 仍是 path+inbox id）。completed 的 inbox 任务不提供 retry。
- 源文件在 completed 之后消失：**不**把已完成任务改成 failed；下载失败即可。

### 6.4 下载

扩展 `open_completed_receive_download`（或并列 predicate，保持 404 泛化）：

允许：

- 现有：`direction=Receive AND status=completed`
- 新增：`direction=Send AND status=completed AND peer_device_id=cc-partner-mobile-inbox`

仍必须：绝对路径、无 `..`、非 symlink、普通文件、可打开。新增：当前文件 `len()` 必须等于任务 `size`，否则当不可用（源已被替换）。

**禁止**对「发给其它电脑的 Send 任务」开放下载，即使 path 仍在。否则局域网 `/mobile` 能把任意已发送源文件读走。

不新增路由，不新增 capability token（仍是本机 `/api/mobile/transfer/download/:taskId`）。更新 `docs/p2p-protocol.md` 该行的资格说明。

### 6.5 前端

- `web/src/lib/` 或 Transfer 模块导出 `MOBILE_INBOX_DEVICE_ID` 与 `isMobileInboxDevice` / `isMobileInboxOffer`。
- `Transfer.tsx`：本地合成 Device 置顶；展示名用 i18n，不显示假 IP/端口。`list_devices` 失败时若尚无列表，仍保留「手机」一项。
- 桌面 `canOpenRevealTransfer` 不变（仍仅 Receive+completed）。不要给 Send 任务传 `onDownload`。
- `MobileTransferView`：`canDownload = canOpenRevealTransfer(task) || isMobileInboxOffer(task)`。
- 手机设备列表不注入 inbox。
- i18n：`transfer:mobileInbox`、`transfer:mobileInboxHint`（及 locale 全套）。

### 6.6 GUI / sidecar

inbox 分支跑在 HeadlessOwner 的 `start_sending`。GuiClient 继续经现有 `send_transfer` control 代理，不在 GUI 进程读文件或持 registry。

## 7. 失败

| 情况 | 行为 |
|------|------|
| 发送时文件不存在 / 不是普通文件 | invoke 失败，尽量不落 completed 任务；用户改选后可再发 |
| 同 clientOperationId 不同路径 | `operationIdConflict`，前端复用对账，禁止 mint 新 id |
| 下载时文件没了、变成目录/链接、size 变了 | 404「下载不可用」，无 path |
| 用其它 Send 任务 id 撞下载 | 同上 404 |
| 对 inbox 调 resume | 明确 unsupported，不开始 P2P |
| 原文件被改但 size 不变 | 零拷贝固有限制：下到的是当前字节。提示文案已说明文件留在原处 |

桌面错误可以给本机用户看 basename；任何 mobile HTTP 响应不得含绝对路径。

## 8. 测试与文档

### 8.1 Rust

- inbox id 走 offer：completed Send、peer 正确、不碰 devices 表、不 spawn chunk。
- 同 clientOperationId 回放同一 task id；不同路径 Conflict。
- 目录 / symlink / 相对路径拒绝。
- download：inbox completed + size 匹配 → 流；Receive completed 仍可用；普通 Send completed → 404；size 变化 / 删除 → 404 且 body 无 path。
- `list_devices` / `list_mobile_devices` 都不返回 inbox id。
- resume(inbox) 拒绝；retry 仅 failed inbox。

### 8.2 前端

- Transfer 下拉置顶「手机」；发送参数 `deviceId=cc-partner-mobile-inbox`。
- 无对端时仍能点发送（有选中文件）。
- 桌面 completed inbox 无 Open/Reveal/Download。
- 手机 `isMobileInboxOffer` 显示 Download；设备下拉无 inbox。

### 8.3 E2E

- 桌面：选手机 + 绝对路径 mock → 一次 `send_transfer` 带 inbox id → 列表出现 completed Send。
- 手机 harness：inbox offer 任务出现 Download；点下载打到 `/api/mobile/transfer/download/:id`。

### 8.4 文档

- `docs/prd.md` §2.1：补充电脑发给本机 `/mobile` 的零拷贝邮箱。
- `docs/p2p-protocol.md`：download 资格扩展。
- `web/AGENTS.md` Transfer 旅程：虚拟目标仅传输页注入。
- `src-tauri/AGENTS.md`：`start_sending` inbox 分支与 download predicate。
- 不新增独立 capability；不改 quality-matrix 的 L3 真机项（仍 NOT VERIFIED）。

## 9. 实现顺序

1. 后端常量 + offer 分支 + download predicate + 单测。
2. 前端常量 / 判定 helper + Transfer 下拉与 hint + mobile Download + 单测。
3. i18n、PRD、协议文档、AGENTS。
4. 桌面 + mobile E2E。

## 10. 已确认决策

- 范围：这台电脑 → 访问这台电脑 `/mobile` 的手机（不做跨主机）。
- 时机：邮箱；发送不要求手机在线。
- 入口：传输页设备下拉增加「手机」，发送按钮不变。
- 存储：零拷贝读原路径（不快照）。
