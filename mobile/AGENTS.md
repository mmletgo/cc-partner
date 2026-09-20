# mobile/ — Flutter / Dart 原生客户端

局域网工作台客户端。业务逻辑在 `lib/`，Flutter 壳在 `lib/app.dart` 与 `lib/ui/`。不要修改 `web/src/mobile/`。

iOS 真机访问 PC 必须在 `ios/Runner/Info.plist` 保留 `NSLocalNetworkUsageDescription` 与 `NSBonjourServices` `_cc-partner._tcp`；地址簿在打开/回前台/下拉时探测 `GET /api/health`。

设计文档：[`docs/superpowers/specs/2026-09-19-flutter-mobile-app-design.md`](../docs/superpowers/specs/2026-09-19-flutter-mobile-app-design.md)。工作台面板与交互以网页 `web/src/mobile/` 为对齐基准（App 端 Dart 自绘实现同语义）。

```bash
export PATH="$HOME/flutter/bin:$PATH"
export DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
export ANDROID_HOME="$HOME/Library/Android/sdk"

cd mobile
flutter test
flutter test integration_test/app_test.dart -d emulator-5554
flutter test integration_test/app_test.dart -d <ios-simulator-id>
flutter run -d emulator-5554
flutter run -d <ios-simulator-id>
```

## 领域地图（lib/）

| 目录 | 职责 |
|------|------|
| `address_book/` | 多 PC 地址簿：CRUD、health 探测（有界并发 3、失败原因）、切服务器（拆 HTTP/WS）、`lastLocation` 持久化、推送登记意图 |
| `core/` | `lan_http`（原生 HTTP/WS 客户端，剥离 Origin）、`health_probe`（4s 超时 + 中文失败原因）、`server_url`（URL 归一化，剥 `/mobile` 路径）、`qr_payload`（扫码解析） |
| `workbench/` | 导航壳：`nav.dart` 双模式分组（全局/项目内，分组标题中文映射）+ 实验开关过滤（`GET /api/orchestrator/config` 读 `experimentalFeatures`，失败/缺字段 fail-closed 全关 automation/browser，当前面板被关自动回落 terminal/projects）+ 徽章 + lastLocation 回落纯函数；`worktree.dart` 激活 worktree 解析 |
| `terminal/` | 终端输入策略：unacked 队列、断线丢弃不重放、gap→replay 状态机、sticky 修饰键 3s 超时、切换会话重置；`touch_scroll.dart`（触控滚动纯函数：像素→行带余量、SGR wheel 编码 `CSI < 64/65;col;rowM` 单次 ≤8 帧、滚动模式判定 mouse-tracking/alt-screen→转发、惰性 history hydration 触发）；`git_actions.dart`（终端内 commit/merge 的 envelope 宽容解析 + hook 失败卡 + `canShowTerminalMergeFab`/`worktreeMergeConfirmText` 门控与文案纯函数 + commit/merge unknown 对账接入 GitMutationTracker） |
| `sessions/` | 会话 API：list/create（带实测 cols/rows clamp 20x6..u16）/replay（refreshHistory）/focus/paste-image/close/resize/split-pane(down)/switch-pane/close-pane/zoom-pane；SessionSummary 宽容解析 supportsPanes/paneCount |
| `files/` | 文件工作区：目录浏览（列表行带 目录/大小 元信息，空目录空态，列表上方当前路径 crumb——root 显示「/」，对齐 web mobilePathCrumb 纯文本语义）、open/save-text（baseHash 乐观锁，保存成功用返回的新 baseHash 回写基线——同会话再次保存不误报 409；open 失败 SnackBar 上屏停留列表）、目录加载失败错误卡（重试 + 可下拉刷新）、dirty guard、HTML 沙箱预览（JS off + 资源 data URL）、图片/CSV/SQLite/Markdown 预览、open 元信息行（类型 · 大小 B/KB/MB · 本地修改时间）与 notice/truncated 提示条；保存失败 inline 错误条（409 冲突提示「文件已在磁盘上变化，请刷新后再试」）且保持 dirty、成功才清 dirty；`capabilities.canEdit` 非 true（含缺字段）一律只读隐藏编辑器与保存（fail-closed，对齐 web `text && capabilities.canEdit` 的 falsy 语义） |
| `git/` | worktree 列表（`includeGitStatus:true`）、commit/pull/push/merge/create/remove/repairHook（clientOperationId 幂等）、提交历史 `commits`；`mutation.dart`（mutation 相位机 idle/busy/reconciling/unknown + ledger `worktrees/mutation-operation` 同 id 对账矩阵，unknown 锁动作不盲重放）；`project_sync.dart`（主分支 push 后兄弟设备逐个 pull 的同步规划纯函数）；`listAllProjects`、分支前缀常量 `composeWorktreeBranchName`；顶层 `reconcileWorktreeMutation()`（同 id 查 ledger + authority 列表/主分支提交集 → 对账矩阵，统一 Git 页/终端/strip/worktrees 页四条对账通道）；`createWorktreeWithTerminalSession` 共享创建 helper（创建 + 自动开绑定终端窗口，窗口失败保留 worktree） |
| `automation/` | 编排器：泳道任务视图（8 泳道）+ 任务块（组渲染/成员上移下移 `reorder-block-members`/末尾追加 `append-block-member`/创建 `create-block`，`orchestrator.task-blocks.v1` 能力门控 fail-closed）、创建对话框（任务/任务块双模式 + AI 完善 + Backlog/Todo/Start + 表单指纹幂等）、AI 完善、Evidence、Outbox retry/discard、experiments 采纳推荐/取消（成功后 onExternalMutation）、runtime 快照条全字段（slots/running/retrying/latestError/recentEvents + 四态徽章 + warm offline「缓存于」）、Attention focusTaskId/focusOutboxId 聚焦（缺失回调 onFocusMissing）、「打开执行现场」onFocusSession（worktreeId 或 sessionId 任一非空即可用，单边原样传 null 由壳层回落）；刷新带请求序号守卫 |
| `browser/` | dev server 自动发现与 live preview（代理 URL，JS on）；一键验证当前预览（`browser-verification/create|get|artifact` 默认 smoke + 有界轮询 250ms×60，摘要卡展示状态/路径/console 错误数/断言失败数/截图，超时与失败可重试、busy 防重） |
| `transfer/` | 主机中转传输：init/chunk/complete（禁盲重试 chunk）、failed 恢复（retry/resume + `transfer.resume.v1` 能力判定 + logical transfer 互斥锁）、uncertain 后 `get-operation` 有界对账（12 次 × 1.5s，成功确认/超时报错）、可见性轮询（任务 3s / 设备 5s，仅 resumed 时跑、回前台立即补拉）、下载落盘、任务分组 |
| `attention/` | 待处理 Inbox：v2/v1 回退、移动端隐藏口径（workbenchDependency 来源 / settings 目标）、已读未读、今天/更早分桶、未读徽章口径、summary/缓存时间展示、freshness 徽章（实时/远端缓存）、刷新失败 stale banner（保留旧数据）、decision/blocked/environment 分组（未知 category 归「其他」尾随）、orchestratorTask/remoteOutbox 跳自动化时带 focusTaskId/focusOutboxId |
| `prompts/` | 收藏 Prompt 列表与优化流式写入会话 |
| `provider/` | cc-switch provider 查询/切换（`provider-manager.v1` 能力探测；手机不装 CLI）；summary `cli.available=false` 时警示卡 + 全部切换禁用（缺 cli 字段按旧后端宽容放行）；空列表时「未找到已配置的 provider」引导提示（对齐 web noProviders） |
| `push/` | APNs/FCM token 获取、向全地址簿在线 PC 登记（仅 `mobile.push.v1` 能力）、payload 白名单 |

## 工作台交互语义（与 /mobile 对齐）

- **导航**：`WorkbenchShell` 左侧 Drawer，全局（项目/待处理/传输/设置/Provider + 断开并返回地址簿）与项目内（终端/浏览器/文件/Git/worktrees/自动化 + 快捷组）双模式，分组标题中文；automation/browser 受实验开关控制（关闭时从 Drawer 过滤、当前面板自动回落、常驻面板卸载；拉取失败在 Drawer 打开时静默重试）；「待处理」项带今天未读徽章（0 不显示，>99 显示 99+），数字与列表同一过滤口径，壳层在前台每 10s 静默轮询徽章（attention 面板时暂停，页面自身回写）。无项目时选项目绑定面板弹回项目页；worktree 切换条（状态点 + 主/worktree 后缀 + 非主 X 删除确认）出现在文件/浏览器/Git/**终端**面板，终端全屏时隐藏。**面板常驻挂载**：仅 files/transfer/terminal 首次激活后进 `_persistedPanels` 以 Offstage 常驻（对齐 web 的 hidden 常驻策略）——Files 草稿上下文、终端会话流、传输进度不因切面板销毁；其余面板对齐 web「切走即卸载、进入即重挂重拉」语义（Git/worktrees/自动化/浏览器等每次进入都是权威新鲜数据）；切换项目/移除激活项目/退出项目或门控关闭时清空项目绑定面板的常驻记录并卸载；终端全屏状态离开终端面板时暂存、回来恢复；strip 删除/创建等 worktree 操作在途（mutation 非 idle）时切换 worktree 被拒绝并提示。
- **终端**：会话 chip 条（状态点 + 名称 + pane 数 + 关闭 X + 新建带实测尺寸）；关闭当前会话自动选下一个优先会话；窗格菜单（新增 split down/切换/关闭，`supportsPanes` 门控，close-pane 返回 closedWindow 整窗移除）；切换会话 = 清屏 + replay + 事件基线归零（修串台）+ zoom-pane 幂等；boot/初始化失败错误页带「重试」（可反复重试）；**replay 输入门闩**（对齐 web `shouldForwardMobileTerminalInput`）：切换会话后首次 replay 完成前丢弃全部输入（输入行/extra keys/SGR wheel），输入 WS 非 ready 时输入行禁用；全屏模式（隐藏 chip 条与工具行，回调壳层隐藏 strip）；NDJSON events 断线指数退避重连（1s→15s 封顶，重连带 `afterOwnerInstanceId/afterSequence`，45s 无帧主动重连，回前台立即重连；重连成功首帧与回前台各静默刷新一次会话列表保持 chip 状态/pane 数新鲜）；输入 WS 断线退避重建（未 ACK 输入永不重放）；长按选区复制、粘贴文本按钮、相册贴图（session 非 running 或输入流未就绪时禁用）；extra keys（sticky Ctrl/Alt 3s 超时、方向键 400ms 前置 + 80ms 连发、`/` 长按滑出菜单跟随按键定位）；触控滚动：mouse-tracking/alt-screen 拖动转发 SGR wheel（64/65，≤8 帧/次），normal buffer 首次上滑惰性 hydration（`replay refreshHistory:true` 替换 buffer 保持底部锚点）；工具行内建提交（message 可空=AI 生成）/合并快捷动作（**交叉互锁**：commit/merge 任一 tracker 处于 busy/reconciling/unknown 未决相位时两个动作都禁用，重对账统一走横幅入口）+ hook 失败卡（让 AI 修复 → 返回 terminalSessionId 则切会话聚焦）；收藏 Prompt sheet（搜索 + 标签 chip 过滤 + 三态 + 失败重试，选中写入不回车）；输入行启用要求会话权威状态为 running（缺 status 按 fail-closed 禁用）、replay 门闩放行与输入 WS ready；Prompt 优化 workingDirectory 优先 worktree.path、成功清空输入 + 「已发送」提示。
- **项目**：目录浏览选择器（本机 fs/roots 起步逐级浏览；局域网先选设备再远端浏览；可新建一层文件夹）替代手填路径；项目行带 kind 徽章（remote「远端」高亮/local「本机项目」中性）与远端设备名（宽容解析 `deviceName`，缺失不显示）；非 local/remote kind（含缺失）行禁点置灰并显示「不支持的项目类型」说明（fail-closed 对齐 web `canSelectMobileProject`）；删除项目需确认，删除**激活**项目前经 `confirmRemove` 接缝先过文件 dirty 预检（壳层复用 dirty 确认框，取消不调后端），成功后以已删除项目 id 回调 `onProjectRemoved`（壳层据此清理激活项目的工作台上下文与常驻面板）；顶部 agent fleet 摘要行（`/api/mobile/workbench/lan-fleet` 宽容解析：needsInput+failed 合计 + 离线设备数，数据不可得整行隐藏）；列表加载失败为错误卡 + 重试 + 可下拉刷新。
- **Git/worktrees**：状态卡（分支/dirty/ahead-behind/canPush，`includeGitStatus:true`）+ 工具行动作（提交/拉取/推送/合并/同步）+ 最近 30 条提交；提交/拉取/推送/合并前确认（合并文案区分功能 merge 与主工作区 collect-merge）；canPush=false 禁推送；提交成功提示 2.5s 自动消失；mutation 结果 unknown（传输异常）时锁定动作 + 「重新对账」（同 clientOperationId 查 ledger + worktrees.list authority 判定，不盲重放）；「同步」= push 主分支后对兄弟设备 sibling 项目逐个 pull——push 子步骤同样纳入 GitMutationTracker 相位机（unknown 自动对账，confirmedSucceeded 才继续 pull；失败/仍 unknown 走 unknown 横幅），兄弟 pull 失败仍走摘要 SnackBar；hook 失败卡复用终端 `HookFailureView`（stdout/stderr 可展开 + 空输出占位，不再打印原始 JSON）；worktrees 页卡片带主/linked、路径、状态/同步/可推送 badge，创建 = 前缀下拉 + `/` + 后缀并自动开绑定终端窗口后切终端面板；点 worktree 卡切换后自动进入终端面板；失败必须上屏；壳层 `_loadWorktrees` 带请求序号守卫（快速连点两个 chip 时旧响应晚到不覆盖新选中，对齐 web requestId 丢弃）；strip 空列表显示「暂无 worktree」占位；worktree 删除（strip 与列表）与终端 commit/merge/worktrees 卡合并全部接入 GitMutationTracker unknown 对账（同 id 查 ledger，unknown 锁动作 + 「重新对账」，不盲重放）；strip 尾部「+ 新建」内联创建（前缀下拉/后缀，成功自动开终端窗口并切终端面板）；终端合并按钮按 canShowTerminalMergeFab 门控（禁用含说明），确认文案用 worktree 显示名/collect 专用文案；worktrees 卡非主树带合并入口；删除激活项目后壳层清空工作台上下文回项目列表；两页下拉刷新。
- **自动化**：泳道分组列表 + 任务块组（展开成员、上移/下移、末尾追加、创建块模式，能力门控）、任务详情（goal/验收/workflow/attemptPhase/运行消息/Claude Session/Transcript/blockedReason/Evidence 时间线 + 「打开执行现场」）、创建对话框（任务/任务块 + AI 完善 + Backlog/Todo/Start）、runtime 快照条（slots/running/retrying/latestError/recentEvents/四态/缓存提示）、experiments（采纳推荐/取消）、Outbox 重试/丢弃（丢弃确认）、Attention 聚焦高亮（缺失回 Attention 提示）；刷新带请求序号守卫。
- **浏览器**：进入自动 discover 候选 chips、预览内刷新（不新建 previewId）、加载进度；传输：分块上传确定进度 + 进行中/需注意/已完成分组（进行中/传输中行内进度条 + 「已传 X / Y（Z%）」文本，复用 `formatTransferBytes`）+ failed 行内重试/续传（能力判定 + 行内错误）+ uncertain 对账 + 可见性轮询；设备/任务**首载失败**（无数据时）显示错误行 + 显式「重试」按钮（轮询失败保留旧数据路径不变）；Provider：刷新 + 重新检测；summary 空列表时显示「未找到已配置的 provider」引导提示。
- **Attention**：单条已读/未读 toggle、全部已读、头部刷新按钮（busy 禁用）、初始失败错误态带重试、可见时每 10s 静默轮询（复用 `VisibilityPoller`：resumed 边沿立即补拉、hidden 停表，single-flight + 请求序号守卫）；点击未读先 markRead 再导航（只导航不执行动作）；项目不在列表时提示；条目卡展示 summary 与 freshness 徽章；刷新失败有快照时显示 stale banner（含最后成功时间）并保留旧数据；按 decision/blocked/environment 分组渲染（未知归「其他」）；跳自动化面板携带 focusTaskId/focusOutboxId。
- **lastLocation**：离开工作台写入 `{projectId, panel, worktreeId, sessionId}`；再次进入自动恢复（逐级回落：项目不在列表放弃、面板无效回终端、worktree 无效回主树），恢复成功提示。
- **确认与反馈**：删除 PC/项目/worktree、丢弃 outbox 等破坏性操作一律确认框；删除**激活** worktree（strip 与列表一致）先做只读脏文件预检（对齐 web `runMobileWorktreeRemovalFlow`，取消不调后端，丢弃清 dirty 快照）；删除**激活**项目同样先过 dirty 预检（`confirmRemove` 接缝）；成功用 SnackBar，失败上屏不留静默；文件 dirty 切上下文三选（取消/丢弃/保存）。

## 测试

`mobile/test/` 覆盖：地址簿、URL 解析、终端 controller（含重连退避/sticky/连发/切会话）、终端触控滚动/SGR 转发/hydration、终端会话与窗格动作、终端页 widget（含 boot 重试/replay 输入门闩/输入 WS 断开禁用输入行/commit-merge 交叉互锁/非 running 禁输入行）、extra keys、文件工作区（含打开失败上屏/错误重试/空目录/行元信息/路径 crumb）、Git client/page/mutation 对账/project sync（含同步 push unknown 对账/hook 卡）、worktree 解析/strip（含空列表占位）、项目页（选择器/确认/重试/confirmRemove 接缝/kind 徽章/不支持类型禁选）、Attention（filter/client/page/徽章/分组/freshness/轮询）、自动化（client/泳道/块/能力门控/experiments/runtime 条/page）、浏览器、传输（进度/分组/恢复判定/对账/轮询/行内进度条/首载失败重试）、Provider（含空列表提示）、推送 payload/fanout、主题、风险文案、工作台壳（面板常驻挂载/断开入口/features 重试/徽章轮询/worktrees 请求序号守卫）。widget 测试用 fake client 注入，不发真实网络。
