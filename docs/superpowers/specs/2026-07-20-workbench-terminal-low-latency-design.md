# Workbench 终端低延迟设计

## 1. 背景与根因

cc-partner 的桌面 Workbench 终端使用 xterm 展示 sidecar owner 持有的 PTY/tmux。字符和退格不做前端本地回显，而是依赖真实 PTY 回显；这一点对 password、raw mode、tmux、Claude TUI 和任意全屏终端程序都是必要的。

当前桌面链路却把真实回显放进了轮询：xterm `onData` 经 Tauri invoke 和 loopback control HTTP 写入 sidecar，PTY 输出写入 owner event bus 后，GUI 的 `run_gui_owner_event_relay` 每轮调用 `events/catch-up`，结束后固定休眠 250ms。事件若刚好错过上一轮，必须等待下一轮，因此仅该环节就增加 0–250ms、平均约 125ms 的视觉延迟。

同一 control API 已提供 `/api/backend/control/events/stream` NDJSON catch-up + live 路由，但 GUI 尚未消费。输入侧还会为每个 `onData` 重新读取 control file、构造 `reqwest::Client` 并发 HTTP 请求；输出侧会等待 `requestAnimationFrame` 后通过 React effect 写 xterm。长会话中，Rust replay buffer 每个 chunk 对最多 120,000 个字符计数/重建，前端又会对最多 200,000 个 UTF-16 unit 物化完整字符串并做前缀/KMP 比较，使延迟随历史长度增长。

根因按优先级为：

1. GUI owner event relay 的固定 250ms 轮询。
2. 每键 control client/file/HTTP 成本及并发请求缺少显式顺序合同。
3. live 输出经过 rAF、React render、完整历史物化与 diff。
4. 前后端 replay buffer 在长会话中的 O(历史长度) append 热路径。
5. 光标移动时不必要的同步布局读取，以及隐藏 pane 仍参与热路径。

## 2. 目标

1. 桌面本机终端在 release 构建中达到键盘事件到可见真实 PTY 回显 p95 ≤ 50ms、p99 ≤ 100ms；任何单次正常回显不得因应用内固定调度等待超过 100ms。
2. owner 发布 `workbench:terminal-output` 到 GUI Tauri listener 的本机 relay p95 ≤ 20ms、p99 ≤ 50ms。
3. 保持 sidecar `HeadlessOwner` 唯一拥有 PTY/tmux、remote bridge 和 event bus；GUI 仍为纯 `GuiClient`。
4. 保持输入字节精确、有序、零重复；传输结果不确定时绝不自动重放同一批输入。
5. 保持 cursor、owner sequence、Gap resync、terminal replay、跨路由缓存、tmux window/pane、Prompt 优化流式写入和移动端终端行为。
6. live append 的 CPU/分配成本只与新增 chunk 大小相关，不再随 120k/200k 历史线性增长。
7. 通过确定性单测、真实 PTY 集成测试和 release GUI 性能证据共同验收；不以“代码路径看起来更短”代替测量。

## 3. 非目标

- 不做前端乐观本地回显。
- 不新增 WebSocket、第二套 event bus、第二套 terminal session/window/pane 模型或新的 LAN capability。
- 不改变 LAN 无身份鉴权边界、control loopback + token 边界或 expected-device request binding。
- 不把 terminal 输入内容、输出内容、Prompt、路径、token 或远端 URL 写入指标、日志和测试产物。
- 不在本次引入 WebGL addon；高吞吐绘制若在主链路修复后仍不足，另行以独立性能证据评估。
- 不卸载隐藏 xterm DOM，不改变“切换 workspace view 时保留终端实例”的既有产品合同。
- 不修改数据库 schema，不需要迁移或回滚脚本。

## 4. 方案比较

### 4.1 方案 A：缩短 catch-up 轮询间隔

把 250ms 调成 16ms 或 10ms 可以缓解手感，但仍保留固定等待、持续 control file 读取、HTTP 请求和空轮询；后台不可见时也会继续消耗 CPU。它还掩盖了服务端已有 live stream 未被使用的问题。

结论：拒绝。只允许作为旧 sidecar 不支持 stream 时的兼容降级，不作为正常路径。

### 4.2 方案 B：现有 control stream + 有序输入泵 + 增量输出

GUI 通过已有 `/events/stream` 按当前 cursor 建立长连接，流本身先 catch-up 再 live；断线后用最后已交付 cursor 重连，收到 Gap 时继续执行现有 terminal replay/runtime snapshot 恢复。输入仍走现有 Tauri/control mutation，但复用 GUI control client，并在前端按 session 建立 leading-edge 有序输入泵。前端 ring buffer 同时保留 replay snapshot 与 live delta 订阅，已挂载 xterm 不再经过完整历史 diff。

优点：复用现有路由、event bus、owner/sequence 和恢复语义；改动可分阶段验证；无需新协议或数据库；能够同时消除固定延迟和长会话热点。

结论：采用。

### 4.3 方案 C：新建双向 WebSocket terminal multiplex

一个长连接同时承载输入和输出，理论上开销最低，但需要新增 framing、鉴权、session multiplex、背压、重连、输入不重放、mixed-version 和 LAN/remote relay 合同。仓库已有 control NDJSON 输出流和 HTTP mutation，当前问题不需要新的传输体系才能解决。

结论：拒绝，YAGNI。

## 5. 总体架构

```text
Desktop GUI (GuiClient)
  xterm onData
    -> per-session ordered input pump
    -> cached BackendControlClient
    -> existing control HTTP sessions.write
    -> HeadlessOwner PTY writer

HeadlessOwner PTY reader
  -> bounded replay chunk ring
  -> RuntimeEventBus(owner, sequence)
  -> existing /api/backend/control/events/stream
  -> GUI relay state (dedupe / Gap / resync)
  -> Tauri event
  -> frontend terminal buffer store
       |- bounded replay snapshot
       `- live delta subscription -> mounted xterm.write
```

架构保持两个独立但共享顺序合同的热路径：

- 输入路径只负责把 xterm 产生的字节按 session 精确写入 owner，不等待 UI 回显，也不猜测终端模式。
- 输出路径只展示 owner PTY 的真实字节；live delta 直接进入已挂载 xterm，完整 snapshot 仅用于首次挂载、reset 和 Gap resync。

## 6. GUI owner 事件实时流

### 6.1 Control 客户端

`BackendControlClient` 新增 `open_events_stream(after)`，POST 到既有 `events/stream`，请求体继续使用 `controlToken + afterOwnerInstanceId + afterSequence`。

现有 client builder 的 15s 全局 timeout 必须移除；普通请求继续由 `send_once(path, body, timeout)` 设置逐请求 timeout。stream 只对“收到响应头”设置 3s connect/header timeout，成功后读取 body 不设 overall timeout，否则长连接会每 15s 被主动切断。

NDJSON decoder 必须：

- 按 byte buffer 和换行解析，允许 UTF-8 字符跨网络 chunk。
- 单行上限 1 MiB；超限清 pending 并返回 resource-limit 错误。
- 空行跳过；malformed JSON 终止当前 stream，不把未知 payload 交给 GUI。
- EOF 时若有非空未终结尾部，按 malformed/truncated stream 处理，不静默交付半条消息。

### 6.2 Relay 生命周期

`run_gui_owner_event_relay` 使用 stream 作为正常路径：

1. 从缓存 control descriptor 构造/取得 client。
2. 以 `GuiEventRelayState.cursor()` 打开 stream。
3. 每条 `RuntimeRelayMessage` 继续交给 `GuiEventRelayState::on_message`。
4. `Deliver` 保持现有 Tauri event enrichment 规则。
5. `DropDuplicate` 静默丢弃。
6. `RequestResync` 先执行现有 terminal replay + runtime gap，再 `attach_at(owner, latest)`。
7. EOF、network、decode 或 owner restart 后保留最后 cursor，按 50ms、100ms、250ms、500ms、1s 上限退避重连。

旧 sidecar 对 stream 返回 404/unsupported 时允许退回现有 `events/catch-up`；fallback 必须被诊断为 `pollFallback`，不得把它当作低延迟达标路径。fallback 以 250ms 维持旧版本功能，每 5s 再探测一次 stream，避免每轮制造 404，也避免 sidecar 升级后永久锁死在轮询。其它错误只重连，不自动切到无限轮询掩盖协议损坏。

### 6.3 取消与关闭

relay 的 `CancellationToken` 必须保存在 GUI runtime state，并在 shutdown 时 cancel。当前 setup 中创建后丢弃 token 的做法需要修正，避免窗口关闭后残留 stream task。

## 7. Control client 复用

GUI `AppState` 新增进程内 `BackendControlClientRuntime`，保存当前 owner descriptor 对应的 cloneable client。它只缓存 port、control token、owner id、schema 和 `reqwest::Client`；不持久化、不暴露 DTO、不写日志。

规则：

- 首次调用从 control file 加载并缓存。
- 同一 owner 的普通 control/Workbench 调用复用 client 和 HTTP connection pool。
- transport、401/403、stale descriptor 或 stream EOF 可使缓存失效；下一次调用重新读取 control file。
- 使缓存失效不等于重放。`sessions.write`、Git mutation 和其它副作用请求的当前调用只返回原结果/错误，绝不因刷新 descriptor 自动再次发送。
- query 是否允许一次刷新继续沿用现有 query 合同，不扩展 mutation 重试。
- server 端每请求 control file 鉴权本次保留，避免同时改变 token authority；其成本在 client/stream 修复后重新测量，再决定是否需要独立优化。

## 8. 有序输入泵

前端新增 session-scoped input pump，输入为 `(sessionId, data)`，输出仍调用现有 `workbenchApi.sessions.writeInput(sessionId, data)`。

每个 session 的 lane 保持：

- `inFlight`: 当前已经提交、尚未 settle 的批次。
- `pending`: in-flight 期间后来到达的字节，按到达顺序拼接。
- `generation`: close/remove/dispose 后阻止旧完成回调驱动新 lane。

行为：

1. idle lane 的第一批数据立即发送，不使用 debounce、rAF 或固定 timer。
2. 请求在途时只累积后续数据；settle 后把完整 pending 作为下一批发送。
3. 每个 session 最多一个 in-flight write，不同 session 可独立推进。
4. 失败批次不重放；当前失败保持与现状一样由 session status/后续动作暴露。已经排队但尚未发送的数据可按原顺序继续发送，除非 lane 已 dispose 或 remoteWriteDisabled。
5. 空字符串不入队；paste、IME、ESC、方向键、Ctrl+C、Ctrl+D 和任意二进制控制序列在 JS string/UTF-8 边界上原样拼接。
6. session close、controller unmount 或 remote offline 时清理尚未提交的 pending；已经提交的请求不伪取消、不重发。

该泵既减少快速输入的 HTTP 次数，也把当前“多个异步 invoke 竞争 owner session mutex”的隐式顺序改为显式 FIFO。

## 9. 前端增量输出与 replay 握手

### 9.1 Store 数据模型

每个 session buffer 除 bounded chunk ring 外，维护进程内 cursor：

```text
TerminalBufferCursor { generation, appendId }
TerminalBufferSnapshot { buffer, cursor, revision }
TerminalBufferDelta { sessionId, generation, appendId, chunk }
```

- `appendId` 是 store 内单调递增编号，不替代 owner event sequence；它只解决 React/xterm 订阅握手竞态。
- `generation` 在 reset/resync/remove 时递增。
- buffer 仍限制为 200,000 UTF-16 unit，继续供跨路由恢复和首屏 replay。
- append 只 push chunk、维护长度、发布 delta；不得物化完整 buffer。现有 `chunks.shift()` 热路径改为 `headIndex` 推进，只有累计废弃前缀达到阈值时才摊销 compact，避免满 200k 后每个小 chunk 都移动整个数组。
- snapshot 仅在首次挂载、显式 replay/resync 或测试读取时物化。

### 9.2 Mounted xterm 握手

TerminalPane 创建 xterm 后：

1. 先订阅该 session 的 live delta，将到达的 delta 暂存到 pane queue。
2. 再读取 `TerminalBufferSnapshot`。
3. 用现有 replay gate 写 snapshot.buffer。
4. replay callback 完成后，丢弃 cursor ≤ snapshot.cursor 的重复 delta。
5. 严格按 `(generation, appendId)` drain 后续 delta 到 `terminal.write`。
6. generation 变化时停止旧 drain，clear + replay 最新 snapshot，再继续新 generation。

这样可以同时保证：订阅与 snapshot 之间无丢失；snapshot 已包含的数据不重复；reset/Gaps 不把旧 generation 追加到新屏幕。

### 9.3 背压

xterm `write` 本身异步解析。每个 pane 维护一个 write-in-flight 队列：首个 live delta 立即调用 `terminal.write`，回调前的新 delta只拼接到 next buffer；回调后继续下一批。不得为首个 echo 等待 animation frame。

现有 rAF revision 通知继续服务 React snapshot/非 live 消费者，但不再位于 active xterm 的 live 显示关键路径。

## 10. 后端 replay buffer

`SessionReplayBuffer` 改为 `VecDeque<ReplayChunk>`：

```text
ReplayChunk { text: String, charCount: usize }
SessionReplayBuffer { chunks, charCount, maxChars, truncated, lastSeq }
```

append 只对新增 chunk 做一次 `chars().count()`。超限时先 O(1) pop 完整头 chunk；只在 overflow 落入头 chunk 中部时扫描该头 chunk 找 UTF-8 char boundary 并切一次。`snapshot()` 才按当前 chunks 总字节数预分配并拼接 String。

必须保持：

- 容量仍按 Unicode scalar 数量 120,000 计算。
- 中文、emoji 和组合字符不会被切坏 UTF-8。
- `truncated` 一旦发生保持 true。
- `lastSeq` 与最后成功 append 的 terminal event seq 一致。
- `maxChars=0`、单 chunk 大于容量和大量单字符 chunk 均有确定性测试。

## 11. 布局热路径

- `onCursorMove` 在 `onCursorAnchorChange` 不存在时必须在读取 DOM 前返回。
- Prompt 优化浮层关闭时必须在 `terminalArea.getBoundingClientRect()` 前返回。
- viewport rect、cell width/height 由 ResizeObserver/fit 或 Prompt 浮层刚打开时更新；普通光标移动只读取缓存。
- 本次仍保持隐藏 pane 的 xterm 实例和输出同步，不引入“隐藏期间暂停解析”这一更高风险行为。完成主链路后若多 Agent 输出仍产生 long task，再单独设计隐藏 pane catch-up。

## 12. 错误处理与恢复

| 场景 | 行为 |
| --- | --- |
| stream connect 失败 | 保留 cursor，有限退避后重连；不清终端 |
| stream malformed/超限 | 终止该连接，记录稳定错误类，按 cursor 重连；服务端 Gap 决定是否 replay |
| owner id 变化 | `GuiEventRelayState` 按既有规则重置 owner，必要时 resync |
| event ring Gap | 先 terminal replay/runtime snapshot，再 attach latest |
| input write 明确失败 | 不重放；清该 in-flight batch，继续或由 offline/close 清 pending |
| input write 响应不确定 | 不重放；缓存 client 可失效，下一批使用新 descriptor |
| frontend delta generation 变化 | 丢弃旧 generation queue，clear + replay 新 snapshot |
| xterm dispose | 取消 live subscription，清 pane queue；不删除全局 replay buffer |

生产日志和指标只记录 stage、耗时、字节数、event count、错误类和 opaque session hash；不得记录 `data`、`chunk`、Prompt、路径、token、URL 或命令文本。

## 13. 测试与性能证据

### 13.1 Rust L0

- NDJSON decoder 覆盖跨 chunk UTF-8、同 chunk 多行、空行、malformed、1 MiB 超限和 EOF 半行。
- event stream client 覆盖 cursor body、非成功状态、connect/header timeout，以及 body 读取不受 15s overall timeout。
- relay state 覆盖 live Deliver、duplicate、owner change、Gap→真实 resync→attach latest、cancel 和重连 cursor。
- control client runtime 覆盖同 owner 复用、invalidate 后重新加载、mutation error 不重放。
- replay deque 覆盖 Unicode、trim、lastSeq、truncated、零容量和单字符满容量增量。

### 13.2 前端 L0/L1

- input pump 覆盖首批同步开始、单 session 最多一个 in-flight、pending 合并、跨 session 隔离、失败不重放、dispose/offline 清理和精确字节顺序。
- terminal buffer store 覆盖 subscribe-before-snapshot 竞态、appendId 去重、generation reset、trim 和 live append 不 materialize 完整 buffer。
- TerminalPane 覆盖 replay 期间 delta queue、replay 后 drain、generation change、xterm write 背压和 workspace view 切换不重建 Terminal。
- cursor 测试断言 callback/浮层关闭时 `getBoundingClientRect` 调用数为 0。

### 13.3 Rust L2

真实 sidecar control server + GUI relay fake UI：

1. 创建 native PTY session。
2. 以 10ms 间隔写入 ASCII、Backspace、ESC 和 paste fixture。
3. 通过 `/events/stream` 实时接收 output。
4. 断言字节顺序、零丢失、零重复。
5. 中断 stream 后按 cursor 重连，验证 catch-up；制造 ring lag，验证 Gap/replay。

测试内容使用固定非敏感 token，不保存用户终端正文。

### 13.4 L3 GUI 性能

macOS release 构建至少执行：

- 空历史单终端 100 次字符/退格。
- 120k 后端 replay + 200k 前端 buffer 后 100 次字符/退格。
- 4 个常驻 pane，其中 3 个后台持续输出。
- 本机与局域网远端项目各一轮。
- password/raw mode、tmux 分屏、Claude TUI、中文 IME、paste、Ctrl+C、Ctrl+D。

记录阶段时间：`onData → control ACK → PTY emit ts → GUI relay receive → Tauri listener → xterm write callback → next painted frame`。其中 xterm write callback 只作为 JS 写入完成点，`key-to-visible` 以其后的首个已绘制帧为终点，并用 release GUI 的帧时间线或高速视频抽样校验；不得把 callback 本身冒充视觉完成。证据不得包含输入/输出正文。

性能门槛：

- 本机 key-to-visible p50 ≤ 20ms、p95 ≤ 50ms、p99 ≤ 100ms。
- owner event publish → GUI listener p95 ≤ 20ms、p99 ≤ 50ms。
- 200k buffer 后 active terminal live 增量处理 p95 ≤ 2ms，且不得调用完整 materialize/KMP。
- 1000 个混合输入字节严格有序，零丢失、零重复。
- 正常输入期间不得出现由终端热路径产生的 >50ms main-thread long task。

L3 未执行时只能声称自动合同通过，不得声称真实 GUI 手感达标。

## 14. 文档与兼容

- `docs/prd.md` 增加桌面终端实时回显、真实 PTY 回显和断线恢复合同。
- `src-tauri/CLAUDE.md` 记录 control stream 为 GUI 正常事件路径、catch-up fallback、client cache 和输入不重放。
- `web/CLAUDE.md` 记录 input pump、live delta/replay snapshot 双路径及 xterm 生命周期合同。
- 不新增 P2P capability；`/api/backend/control/events/stream` 已是 loopback control 路由。
- mixed-version 只在 stream endpoint 不可用时进入有诊断的 poll fallback；同版本不得使用 fallback。

## 15. 回滚

- stream relay 可回滚到 catch-up fallback，终端功能仍可用但不满足低延迟门槛。
- client cache 可单独回滚为每次加载 descriptor，不改变 control API。
- input pump 可回滚为逐次 invoke，不改变后端 mutation。
- frontend live delta 可回滚为现有 revision/rAF/full snapshot 路径；全局 replay buffer仍保留。
- replay deque 的外部 DTO 不变，可回滚内部结构，无数据迁移。

任何阶段回滚都不得启用本地乐观回显、自动重放不确定输入或让 GUI 直接拥有 PTY。

## 16. 完成标准

1. 同版本桌面 GUI 正常路径不再周期性调用 `events/catch-up`，而是持续消费 `events/stream`。
2. stream 断线、owner change 和 Gap 继续通过 cursor/replay 收敛，无事件重复或静默丢失。
3. 快速输入时每个 session 最多一个 in-flight write，输入精确有序，不确定结果不重放。
4. 已挂载 xterm 的 live 输出不经过 rAF、React render、完整 200k materialize/KMP 才显示。
5. Rust replay append 不再每 chunk 扫描/重建完整 120k 历史。
6. cursor 热路径在无需定位时不触发同步布局读取。
7. Rust、前端、L2 测试通过；L3 release GUI 证据满足第 13.4 节全部门槛。
8. PRD、前后端分层指令和测试事实同步，且无终端正文或凭据进入日志/证据。
