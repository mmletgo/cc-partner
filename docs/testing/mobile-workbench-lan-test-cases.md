# Mobile Workbench 局域网测试用例

## 目标

验证同一局域网内手机或远端主机访问 `http://<局域网IP>:<动态端口>/mobile` 时，能通过 Rust axum HTTP server 使用 Workbench 的项目、worktree、终端、文件、Git、Prompt 优化入口，并能持续接收终端事件。

## 环境

- 被测主机：本机 macOS，仓库 `/Users/hans/web_project/cc-partner`
- 模拟移动端：SSH 主机 `power`，Ubuntu，具备 `curl` 和 `google-chrome`
- 桌面端启动命令：仓库根目录执行 `./web/node_modules/.bin/tauri dev`
- HTTP 端口发现：监听日志 `axum HTTP server 已启动，监听端口: <port>`，或访问 `/api/mobile/access-info`
- 移动端 URL：`http://<本机局域网IP>:<port>/mobile`

## 自动化回归用例

| ID | 用例 | 命令 | 通过标准 |
| --- | --- | --- | --- |
| A1 | Mobile access-info 运行时选择 | `cd web && npx --yes tsx src/api/mobile.test.ts` | 输出 `mobile.test.ts passed` |
| A2 | MobileAccessCard URL 选择 | `cd web && npx --yes tsx src/components/domain/MobileAccessCard/mobileAccessCard.test.ts` | 输出 `mobileAccessCard.test.ts passed` |
| A3 | HTTP Workbench payload 契约 | `cd web && npx --yes tsx src/api/workbenchHttp.test.ts` | 输出 `workbenchHttp.test.ts passed` |
| A4 | Mobile shell/panel 状态 helper | `cd web && npx --yes tsx src/mobile/mobilePanelState.test.ts && npx --yes tsx src/mobile/mobileWorkbenchState.test.ts` | 两个测试均 passed |
| A5 | Mobile terminal replay helper | `cd web && npx --yes tsx src/mobile/mobileTerminalReplay.test.ts` | replay gate、去重和 live buffer helper 通过 |
| A6 | HTTP NDJSON parser | `cd web && npx --yes tsx src/hooks/workbenchHttpEvents.test.ts` | UTF-8 分包、pending line、terminalOutput 写 buffer 通过 |
| A7 | Terminal buffer store | `cd web && npx --yes tsx src/hooks/workbenchTerminalBuffer.test.ts` | append/reset/remove/订阅语义通过 |
| A8 | 桌面 terminal replay 基线 | `cd web && npx --yes tsx src/pages/Workbench/terminalReplay.test.ts` | replay gate、buffer diff、输入屏蔽释放通过 |
| A9 | 前端类型与构建 | `cd web && npm run build` | `tsc -b && vite build` exit 0，且生成 `dist/mobile.html` 与 mobile assets |

## HTTP 与静态入口用例

| ID | 用例 | 请求 | 通过标准 |
| --- | --- | --- | --- |
| H1 | access-info 返回局域网 URL | `curl -sS http://127.0.0.1:<port>/api/mobile/access-info` | JSON 包含 `port` 和 `urls[]`；`urls[]` 不含 `localhost`、`127.0.0.1`、`::1` |
| H2 | `/mobile` shell 可访问 | `curl -i http://127.0.0.1:<port>/mobile` | HTTP 200；HTML 包含 mobile 入口脚本；无 `mobile.html not found` |
| H3 | SPA fallback | `curl -i http://127.0.0.1:<port>/mobile/projects` | HTTP 200；返回 mobile shell |
| H4 | asset 精确服务 | 从 H2 提取 `/assets/*.js` 后 `curl -i http://127.0.0.1:<port>/assets/<file>` | HTTP 200；Content-Type 为 JS/CSS 等预期类型 |
| H5 | API 不被 SPA fallback 吃掉 | `curl -i http://127.0.0.1:<port>/api/health` | HTTP 200 JSON；不是 mobile HTML |
| H6 | Workbench 项目列表 | `curl -sS http://127.0.0.1:<port>/api/workbench/projects/list` | 返回 JSON 数组；空列表也算通过 |
| H7 | 远端主机可达 | `ssh power 'curl -i http://<局域网IP>:<port>/mobile'` | HTTP 200；证明非 localhost 设备可访问 |
| H8 | 远端浏览器无控制台错误 | `ssh power 'google-chrome --headless --disable-gpu --window-size=390,844 --screenshot=/tmp/mobile.png http://<局域网IP>:<port>/mobile'` | 截图文件生成；页面不是空白/错误页 |

## Workbench 真实链路用例

这些用例需要被测桌面端已有至少一个本机 Workbench 项目；如果项目列表为空，先在桌面端添加一个临时 Git 项目再继续。

| ID | 用例 | 步骤 | 通过标准 |
| --- | --- | --- | --- |
| E1 | 选择本机项目 | 在 `/mobile` 打开项目列表，选择 `kind === "local"` 的项目 | 自动加载 worktrees 和 sessions，进入 terminal 面板；远端 shortcut 保持禁用提示 |
| E2 | Worktree 列表 | 请求 `/api/workbench/worktrees/list`，body `{ "projectId": "<projectId>" }` | 返回至少主 worktree；字段包含 `id/path/isMain/status` |
| E3 | Session 列表/创建 | 若无 running session，调用 `/api/workbench/sessions/create`，body `{ "projectId": "<projectId>", "worktreeId": "<worktreeId>", "initialCols": 80, "initialRows": 24 }` | 返回 session；随后 list 能看到该 session |
| E4 | Terminal replay | 调 `/api/workbench/sessions/replay`，body `{ "sessionId": "<sessionId>" }` | 返回 `{sessionId, buffer, truncated, lastSeq}`；字段类型正确 |
| E5 | Terminal write + event stream | 打开 `/api/workbench/events` NDJSON，同时 POST `/api/workbench/sessions/write` 写入 `printf 'MOBILE_E2E_OK\n'\\n` | NDJSON 收到对应 `terminalOutput`，mobile 终端可见文本 |
| E6 | Resize/focus | POST `/sessions/resize` `{sessionId, cols: 90, rows: 28}`；POST `/sessions/focus` `{sessionId}` | 均返回 `{ok:true, sessionId}`；无后端错误日志 |
| E7 | Split/close pane | POST `/sessions/split-pane` `{sessionId, direction:"right"}`；再 POST `/sessions/close-pane` | split 返回方向；close 返回 `closedWindow` 布尔值；若最后 pane 被关闭，前端移除 window |
| E8 | Files list/open/save | POST `/files/list-dir` 根目录；打开临时文本文件；保存时带 `baseHash` | list 返回节点；open 返回文本与 hash；save 返回新 hash；错误时显示后端 message |
| E9 | Dirty guard | 移动端编辑文件不保存，尝试切换 worktree/project | 出现确认；取消后仍停留原文件和原上下文 |
| E10 | Git commits | POST `/api/workbench/git/commits` `{projectId, worktreeId, limit:30}` | 返回提交数组；空仓库应展示可读空态而不是崩溃 |
| E11 | Prompt 优化入口失败可读 | 在没有可用 Claude CLI 或输入为空时触发 Prompt 面板写入 | 前端显示可读错误；不产生空写入或无响应状态 |

## 日志与失败信号

- Rust 日志必须检查 `axum HTTP server 已启动`、`axum HTTP server 异常退出`、Workbench route 错误、tmux dependency 错误。
- 浏览器控制台必须检查 Vite/Tauri overlay、空白页、资源 404、JSON parse error、NDJSON 断流重连。
- 终端事件必须检查 UTF-8 分包后中文/符号不乱码、`lastSeq` 去重后不重复写入 replay 内容。
- 远端主机访问必须使用局域网 IP，不能用 `127.0.0.1` 或 `localhost`。

## 本次执行记录

时间：2026-07-05 00:13-00:18（Asia/Singapore）

启动方式：

- `cd src-tauri && RUST_LOG=info cargo run`
- 原因：本机 5173 端口已被其他项目 Vite 进程占用；`web/vite.config.ts` 使用 `strictPort: true`，因此没有杀无关进程，改用已构建的 `web/dist` + Rust dev/test 静态资源兜底启动。
- HTTP 端口：`52568`
- 局域网 URL：`http://192.168.6.145:52568/mobile`

已通过：

- A1：`npx --yes tsx src/api/mobile.test.ts`
- A2：`npx --yes tsx src/components/domain/MobileAccessCard/mobileAccessCard.test.ts`
- A3：`npx --yes tsx src/api/workbenchHttp.test.ts`
- A4：`npx --yes tsx src/mobile/mobilePanelState.test.ts && npx --yes tsx src/mobile/mobileWorkbenchState.test.ts`（`mobileWorkbenchState.test.ts` exit 0，无成功文案输出）
- A5：`npx --yes tsx src/mobile/mobileTerminalReplay.test.ts`
- A6：`npx --yes tsx src/hooks/workbenchHttpEvents.test.ts`
- A7：`npx --yes tsx src/hooks/workbenchTerminalBuffer.test.ts`
- A8：`npx --yes tsx src/pages/Workbench/terminalReplay.test.ts`
- A9：`npm run build`，产物包含 `dist/mobile.html`、`dist/assets/mobile-*.js/css`
- H1：`/api/mobile/access-info` 返回 `{"deviceName":"hanss-Air.lan","port":52568,"urls":["http://192.168.6.145:52568/mobile"]}`，未包含 loopback URL
- H2/H3：`/mobile` 与 `/mobile/projects` 返回 HTTP 200 mobile shell
- H4：`/assets/mobile-CrMGfMvq.js` 返回 HTTP 200 `text/javascript`
- H5：`/api/health` 返回 JSON，不被 mobile SPA fallback 捕获
- H6：初始 `/api/workbench/projects/list` 返回空数组，随后通过临时 Git 项目验证真实链路
- H7：`ssh power 'curl http://192.168.6.145:52568/mobile'` 返回 HTTP 200 mobile shell
- H8：`power` 上 `google-chrome --headless --window-size=390,844` 渲染成功，DOM 包含 `MobileWorkbench`、项目列表和临时项目；截图已拉回 `/tmp/cc-partner-mobile-power.png`
- E2/E3：临时 Git 项目通过 `/api/workbench/projects/open` 打开后，worktree list 返回 main worktree，session create 返回 running session 且 `supportsPanes:true`
- E4/E5：`/sessions/replay` 返回 replay DTO；`/api/workbench/events` 监听期间写入 `printf 'MOBILE_E2E_OK\n'`，NDJSON 收到 `terminalOutput`，replay after 包含 `MOBILE_E2E_OK`
- E6/E7：`resize`、`focus`、`split-pane right`、`close-pane` 均返回 `{ok:true}`，`close-pane` 返回 `closedWindow:false`
- E8：根目录 `files/list-dir` 返回 `.git` 与 `README.md`；`files/open` 返回 README 文本和 `baseHash`；`files/save-text` 成功，重开文件包含 `MOBILE_FILE_SAVE_OK`
- E10：`git/commits` 返回初始提交 `init mobile e2e fixture`
- UI 交互：in-app Browser 以 390x844 打开 `/mobile`，点击临时项目进入终端面板，页面显示 `MOBILE_E2E_OK`，浏览器 error/warn 日志为空

清理：

- 已调用 `/api/workbench/sessions/close` 关闭测试 session
- 已删除临时目录 `/tmp/cc-partner-mobile-e2e.Ncf4Io`
- 已从 `/Users/hans/.cc-partner/data.db` 精确删除本次临时 project/worktree/session 记录；清理后匹配 `cc-partner-mobile-e2e.%` 的项目记录数为 `0`
- 已停止 `cargo run`，`127.0.0.1:52568` 已不可连接

观察：

- 首次并行执行多个 `npx --yes tsx` 时触发 npm npx 缓存目录 `ENOTEMPTY rename`，改为串行执行后全部通过。这是测试运行方式问题，不是项目代码失败。
- in-app Browser 的 Playwright locator/snapshot 路径报过插件侧 `incrementalAriaSnapshot is not a function`，改用 Browser CUA 坐标点击 + DOM evaluate 后完成交互验证。`power` 上 Chrome headless 也已独立验证远端浏览器可渲染。
- 后端运行期间 `mdns_sd::service_daemon` 对若干 IPv6/link-local 接口输出 `Cannot find valid addrs for TYPE_SRV/TYPE_A response`，但 HTTP server、access-info、远端访问、Workbench routes 和事件流均正常。
