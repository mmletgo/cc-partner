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
| `workbench/` | 导航壳：`nav.dart` 双模式分组（全局/项目内）+ 徽章 + lastLocation 回落纯函数；`worktree.dart` 激活 worktree 解析 |
| `terminal/` | 终端输入策略：unacked 队列、断线丢弃不重放、gap→replay 状态机、sticky 修饰键 3s 超时、切换会话重置 |
| `sessions/` | 会话 API：list/create/replay/focus/paste-image |
| `files/` | 文件工作区：目录浏览、open/save-text（baseHash 乐观锁）、dirty guard、HTML 沙箱预览（JS off + 资源 data URL）、图片/CSV/SQLite/Markdown 预览 |
| `git/` | worktree 列表（可选 git status）、commit/pull/push/merge/create/remove/repairHook（clientOperationId 幂等）、提交历史 `commits` |
| `automation/` | 编排器：泳道任务视图（8 泳道）、创建任务（三动作 + clientRequestId 幂等）、AI 完善、Evidence、Outbox retry/discard |
| `browser/` | dev server 自动发现与 live preview（代理 URL，JS on） |
| `transfer/` | 主机中转传输：init/chunk/complete（禁盲重试 chunk）、下载落盘、任务分组 |
| `attention/` | 待处理 Inbox：v2/v1 回退、移动端隐藏口径（workbenchDependency 来源 / settings 目标）、已读未读、今天/更早分桶、未读徽章口径 |
| `prompts/` | 收藏 Prompt 列表与优化流式写入会话 |
| `provider/` | cc-switch provider 查询/切换（`provider-manager.v1` 能力探测；手机不装 CLI） |
| `push/` | APNs/FCM token 获取、向全地址簿在线 PC 登记（仅 `mobile.push.v1` 能力）、payload 白名单 |

## 工作台交互语义（与 /mobile 对齐）

- **导航**：`WorkbenchShell` 左侧 Drawer，全局（项目/待处理/传输/设置/Provider）与项目内（终端/浏览器/文件/Git/worktrees/自动化 + 快捷组）双模式；「待处理」项带今天未读徽章（0 不显示，>99 显示 99+），数字与列表同一过滤口径。无项目时选项目绑定面板弹回项目页。
- **终端**：会话 chip 条（状态点 + 名称 + 新建）；切换会话 = 清屏 + replay + 事件基线归零（修串台）；NDJSON events 断线指数退避重连（1s→15s 封顶，重连带 `afterOwnerInstanceId/afterSequence`，45s 无帧主动重连，回前台立即重连）；输入 WS 断线退避重建（未 ACK 输入永不重放）；长按选区复制、粘贴文本按钮、相册贴图；extra keys（sticky Ctrl/Alt 3s 超时、方向键 400ms 前置 + 80ms 连发、`/` 长按滑出菜单跟随按键定位）。
- **项目**：目录浏览选择器（本机 fs/roots 起步逐级浏览；局域网先选设备再远端浏览；可新建一层文件夹）替代手填路径；删除项目需确认。
- **Git/worktrees**：状态卡（分支/dirty/ahead-behind，`includeGitStatus:true`）+ 最近 30 条提交；提交/拉取/推送/合并前确认；失败必须上屏；worktree 删除确认；两页下拉刷新。
- **自动化**：泳道分组列表、任务详情（goal/验收/Evidence 时间线）、创建对话框（AI 完善 + Backlog/Todo/Start）、Outbox 重试/丢弃（丢弃确认）；刷新带请求序号守卫。
- **浏览器**：进入自动 discover 候选 chips、预览内刷新（不新建 previewId）、加载进度；传输：分块上传确定进度 + 进行中/需注意/已完成分组；Provider：刷新 + 重新检测。
- **Attention**：单条已读/未读 toggle、全部已读、今天/更早切换；点击未读先 markRead 再导航（只导航不执行动作）；项目不在列表时提示。
- **lastLocation**：离开工作台写入 `{projectId, panel, worktreeId, sessionId}`；再次进入自动恢复（逐级回落：项目不在列表放弃、面板无效回终端、worktree 无效回主树），恢复成功提示。
- **确认与反馈**：删除 PC/项目/worktree、丢弃 outbox 等破坏性操作一律确认框；成功用 SnackBar，失败上屏不留静默；文件 dirty 切上下文三选（取消/丢弃/保存）。

## 测试

`mobile/test/` 覆盖：地址簿、URL 解析、终端 controller（含重连退避/sticky/连发/切会话）、extra keys、文件工作区、Git client/page、worktree 解析、项目页（选择器/确认）、Attention（filter/client/page/徽章）、自动化（client/泳道/page）、浏览器、传输（进度/分组）、Provider、推送 payload/fanout、主题、风险文案。widget 测试用 fake client 注入，不发真实网络。
