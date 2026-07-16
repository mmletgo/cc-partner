# Agent-first CLI 设计

- 日期：2026-07-15
- 状态：已批准
- 依赖：A1–A6 稳定领域合同
- 对应计划：`docs/superpowers/plans/2026-07-15-agent-first-cli.md`
- 共享边界：继承 A0 owning-device 与固定 LAN 合同；remote business API 无调用者身份鉴权，本机 control token 不得发给 peer。

## 1. 问题

`cc-partner-backend` 当前只负责 `start/serve/stop/status/doctor` 生命周期与诊断。Agent 若要列出project/worktree/session、读取terminal、创建task、等待runtime或执行browser验证，只能了解内部Tauri/P2P DTO，缺少稳定selector、JSON envelope、exit code和retry合同。

把这些能力继续塞进backend生命周期CLI会混淆运维和Agent控制面；让Agent操作GUI又会增加脆弱交互。

## 2. 目标

1. 新增独立`cc-partner`二进制，保留`cc-partner-backend`现有语义。
2. 所有命令使用稳定ID或显式精确selector，不做模糊全局搜索。
3. 提供稳定JSON/JSONL、错误code、exit code、cursor和outcomeUnknown合同。
4. 本机复用sidecar control descriptor/control API；remote必须显式`--device id:<deviceId>`并复用P2P协议。
5. mutation不盲重放；有稳定request ID的领域按协议对账。
6. CLI只编排现有领域helper，不直接读写SQLite或复制业务逻辑。

## 3. 非目标

- 不修改`cc-partner-backend`为通用控制CLI。
- 不提供全局Quick Open、fuzzy picker、Command Recipe或shell模板。
- 不在argv中传Prompt、terminal input、token或secret；正文通过stdin。
- 不绕过provider sandbox/approval、Orchestrator claim或delivery状态机。
- 不把mDNS设备称为认证/可信设备。
- 不自动选择remote device。

## 4. 命令面

```text
cc-partner [--device local|id:<deviceId>] [--json] <resource> <action>

project list
project inspect --project id:<id>|path:<canonicalPath>
worktree list --project ...
worktree create --project ... --input-json -
session list --project ... [--worktree id:<id>|branch:<exact>]
session read --session id:<id> [--after-sequence N]
session send --session id:<id> --input-json -
agent list --project ...
agent inspect --agent id:<id>
agent wait --agent id:<id> --phase <phase> --timeout-ms N
task list --project ...
task create --project ... --input-json -
task cancel|retry --task id:<id> --client-request-id <uuid>
experiment create --project ... --input-json -
experiment inspect|cancel --experiment id:<id>
attention list
fleet snapshot
browser discover --project ...
browser verify --project ... --input-json -
browser inspect --run id:<id>
event follow [--after-owner <id> --after-sequence N]
```

v1不提供`active/current/recent/name`等依赖GUI局部状态的selector。branch/path只做规范化后的精确匹配；多命中返回conflict。

## 5. 输出与exit code

成功：

```json
{"schemaVersion":1,"ok":true,"data":{}}
```

失败：

```json
{
  "schemaVersion":1,
  "ok":false,
  "error":{
    "code":"snake_case",
    "message":"bounded generic message",
    "retryable":false,
    "requestId":null,
    "outcomeUnknown":false
  }
}
```

exit code：

| code | 语义 |
|---:|---|
| 0 | success |
| 1 | internal/unclassified failure |
| 2 | usage或input validation |
| 3 | not found |
| 4 | conflict/ambiguous selector |
| 5 | unavailable/timeout/backend offline |
| 6 | unsupported protocol/capability |
| 7 | partial result |

`--json`时stdout只允许一个JSON；`event follow`输出一行一个JSON。日志与诊断只到stderr，正文不回显。

## 6. 本机control plane

新增typed control endpoints：

- `POST /api/backend/control/agent/query`
- `POST /api/backend/control/agent/mutate`

仍为loopback+现有control token，只供本机GUI/CLI。query body≤256KiB、response≤1MiB；browser artifact复用专用stream route。

query可以在连接失败后重新读取control file并重试一次；mutation遵循：

- naturally idempotent或具有领域request ID：先对账再决定；
- terminal send、尚无服务端dedupe的worktree create：只发送一次，连接丢失返回`outcomeUnknown=true`；
- CLI不直接打开SQLite，也不通过localhost LAN业务API绕过control plane。

## 7. Remote transport

- `--device id:<id>`从当前owner的mDNS device表解析实际address/port。
- project/worktree/session复用Workbench P2P；task/experiment复用Orchestrator P2P；Attention/Fleet/Browser按各自capability。
- business API继续没有调用者身份校验，不发送control token。
- mutation严格读取route retry classification；terminal send/click/fill等non-replayable操作hit count恒1。
- remote error从结构化error envelope映射，不解析本地化message。
- remote entity ID由既有`remote_ids`helper映射，CLI不自行拼接/剥离。

## 8. 隐私与输入

- Prompt、goal、terminal bytes、fill value只允许`--input-json -`或stdin。
- shell history和process list中不得出现正文。
- JSON错误不包含stdin、path credential、env、terminal output或browser fill value。
- `agent inspect`只返回稳定cc-partner Agent ID与投影metadata，不返回provider-native session ID、transcript path或launch环境。
- `session read`和artifact是用户显式请求的内容读取，不写日志、不进入Ledger/Fleet。
- 最大stdin 1MiB；terminal send最大256KiB；超限在传输前失败。

## 9. 打包、兼容与回滚

- Cargo新增`cc-partner`bin；Tauri externalBin仍只指向`cc-partner-backend`。
- release workflow单独构建/上传`cc-partner`，不替换GUI sidecar。
- 新CLI对旧owner做health/capability gate；unsupported明确exit 6。
- rollback删除新二进制/control agent routes不影响backend lifecycle和GUI。
- JSON schemaVersion变更只能新增可选字段；破坏性变更提升version。

## 10. 测试与验收

1. args/selector/JSON/exit code/stdout-stderr隔离有单测。
2.本机query side-effect-free；session inspect不得隐式spawn/restore terminal。
3. mutation uncertainty、request ID对账和hit count有fake transport测试。
4. remote v0/v1/offline/conflict/requestId/capability有mock peer测试。
5. CLI smoke启动隔离backend，执行project/session/task/agent/browser查询与幂等create。
6. macOS/Windows/Ubuntu构建与基本smoke分别记录证据；未执行保持NOT VERIFIED。

## 11. Spec自审

- CLI没有新的业务状态机，只是typed transport和呈现层。
- 没有fuzzy/global入口、命令配方或自动设备选择。
- 敏感正文不进入argv、日志或错误envelope。
