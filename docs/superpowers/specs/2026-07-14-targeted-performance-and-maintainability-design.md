# Targeted Performance and Maintainability 设计

- 日期：2026-07-14
- 状态：已确认
- 基线：main gzip 174,249 B、mobile 148,429 B、max lazy 263,829 B、runtime JS 934,108 B；SQLite pool=1 保持

## 1. 问题

当前 bundle budget 总体健康，但 Workbench 为一个运行时间文本每秒重渲染 1194 行页面及 controllers；CodeMirror 静态导入全部语言，编辑器 chunk 约 756 KiB raw。Claude session 首次索引在 async command 中同步扫描 JSONL 并缓存文本，watcher 不完整处理 delete/rename。peer client 所有请求共用 10 秒 timeout，多设备同步重复 health 且串行。多个 TS/Rust 文件超过模块基线，两个 hard exception 于 2026-10-12 到期。

## 2. 目标

1. 隔离 Workbench 1 Hz 时钟，只更新可见运行时文本。
2. CodeMirror 按语言动态加载并缓存，不回退未知/纯文本体验。
3. Claude session 索引移入 blocking worker，限制单轮文件数、单文件大小、总读取字节与缓存文本。
4. watcher 正确处理 create/modify/delete/rename。
5. peer timeout 按请求类别配置，多设备同步有限并发并复用单次 health。
6. 只拆当前触达的大模块并收紧 ratchet，不改变行为。

## 3. 非目标

- 不做框架重写、数据库连接池扩容或无证据微优化。
- 不卸载隐藏的 xterm DOM，不破坏 terminal buffer/provider 生命周期。
- 不一次性拆完所有大文件，不创建新的万能 controller。
- 不降低现有 bundle/module/test 门禁。

## 4. 前端性能

### 4.1 RuntimeText

`Workbench.tsx` 不再持有 `runtimeNow`。新增小型 `SessionRuntimeText({startedAt,endedAt,running,visible,emptyValue})`：`visible` 由现有 active inspector/workspace 状态提供；仅当 session running、所属表面可见且 document visible 时启动 interval。停止后使用 `endedAt` 冻结最终时长。使用 `useSyncExternalStore` 或局部 state，只重渲染文本子树。

验收以 React Profiler/测试计数为准：5 次 tick 不应重新执行 Workbench controller harness 或 terminal pane render。

### 4.2 CodeMirror language loader

```ts
export type WorkbenchLanguageLoader = () => Promise<Extension>

export function loadWorkbenchLanguage(language: string): Promise<Extension | null>
```

按语言 map 使用 dynamic import，非 `async` wrapper 直接返回按 canonical language cache 的 Promise，失败时清 cache 以允许重试。切换文件时 seq guard 丢弃旧 loader；加载中先显示 plain text，不阻塞编辑。常见语言可按真实使用数据决定是否合并，不凭直觉全部预加载。

## 5. 后端性能

### 5.1 Claude session index budgets

- `spawn_blocking` 执行目录遍历、JSONL 读取和解析。
- 单轮最多 10,000 文件、单文件 64 MiB、单 JSONL 行 1 MiB、总读取 512 MiB；每 session 最多索引 1,000,000 个 Unicode scalar value，并只在 UTF-8 char boundary 截断。使用 bounded `read_until/take`，不能先分配超长行。文件先按 mtime desc + canonical path 稳定排序再截断；达到预算返回稳定 truncated diagnostics 并保留已索引结果。
- display cache 不保存完整 transcript，只保存搜索/列表所需摘要；正文按需读取并有大小上限。
- 本机与远端 API/前端 DTO 返回 `{items,truncated,diagnostics}`，Session Search 明确展示截断原因/预算，不把 partial 列表伪装完整；新 capability 之前的 peer 保留 legacy Vec decode，并显示 diagnostics unavailable，而不是解码失败。
- watcher delete 删除对应 index，rename 视为 remove old + index new。

### 5.2 Timeout classes

```rust
pub enum PeerTimeoutClass { Health, Metadata, Mutation, LongRunning }
```

- Health 3s；metadata 10s；普通 mutation 30s；长操作由显式预算控制。event stream 已由 N1 owner-managed bridge 与 N3 heartbeat/watchdog 管理，不进入普通 PeerClient total-timeout enum。
- 多设备全局同步并发上限 4；每个 device 一次 typed health/protocol info 复用于所有领域。
- 并发不得绕过 N1 owner singleflight 或 N2 batch 限制。

## 6. 模块治理

优先边界：

- `Workbench.tsx` 抽出运行时展示和空态组合，但保持七个 controller 规则。
- `useSettingsController.ts` 按资源加载、表单保存和 update/permissions 分离纯 hooks；views 仍不 import API。
- `transfer/receiver.rs` 按 request validation、chunk IO、resume/finalize 和 route adapter 分模块。
- `workbench/dependencies.rs` 与 `sessions.rs` 仅在本轮触达部分有 characterization 后拆。

每次拆分先添加 characterization，迁移后降低 `scripts/module-boundary-baseline.json` 上限；禁止提高 baseline 掩盖回归。2026-10-12 exceptions 到期前必须关闭或用新证据申请一次明确、短期延长。

## 7. 指标与预算

- 保持当前 main/mobile/total hard ceilings；checker 新增 editor-entry loaded gzip 指标，CodeEditor initial editor chunk 目标下降至少 20%。`web/scripts` 与根 `scripts` 的 baseline 由现有 `--write-baseline` 同步写入，禁止只改单份。
- Workbench active idle CPU 在无 terminal output 时不因页面级 1 Hz render 持续上升。
- Claude index 达到预算时 UI 可用且显示 truncated，不 OOM/阻塞 Tokio worker。
- peer sync 同设备不重复 health；全局并发峰值 ≤4。
- SQLite pool 继续为 1，除非独立 benchmark 同时证明吞吐改善且锁等待/事务语义无回退。

## 8. 测试与验收

1. fake timers 证明 RuntimeText tick 不重渲染 Workbench harness。
2. language loader 测试 cache、未知语言、快速切换 stale guard 和 import failure fallback。
3. bundle analyzer/budget 证明语言 chunk 按需且主/mobile 不回退。
4. Rust 测试覆盖 index budgets、spawn_blocking 调用 seam、delete/rename watcher。
5. timeout class、health reuse、并发上限在 deterministic fake peer 测试中通过。
6. module checker 只下降不升高，所有拆分有 characterization 与 focused test。

## 9. 持久文档

实现时更新 `web/CLAUDE.md`、`src-tauri/CLAUDE.md`、bundle/module baseline、质量矩阵；只有持久预算或模块所有权变化才更新根 AGENTS。

## 10. Spec 自审

- 优化均来自已测热点，没有无证据全仓重写。
- xterm 生命周期、pool=1、七 controller 和现有预算均保留。
- 每个性能结果都有可重复测量面，不以文件变小冒充运行时改善。
