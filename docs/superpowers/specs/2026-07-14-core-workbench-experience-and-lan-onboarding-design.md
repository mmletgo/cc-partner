# Core Workbench Experience and LAN Onboarding 设计

- 日期：2026-07-14
- 状态：已确认
- 设计约束：复用现有 token、primitives、AppShell、WorkbenchProjectRail、Dialog/Drawer；实现阶段使用 `huashu-design` 做现有设计语言内的验证，不创造新品牌方向

## 1. 问题

产品核心价值是跨设备 Workbench 与自动化，但 `/` 默认展示 GitHub Trending，侧栏有 12 个同级入口。短窗口下内容区没有独立滚动，项目/权限内容与 footer 重叠。未选择项目时，Workbench 仍展示大量禁用 toolbar、空终端与空 inspector，主 CTA 藏在侧栏。GUI 启动即确保 sidecar，但 Welcome 没有在 LAN listener 启动前获得一次性知情确认。移动端十个扁平 panel 增加导航负担，meta 小字对比度低于普通正文要求。

## 2. 目标

1. `/` 成为“继续工作”控制台，直接回答用户下一步要处理什么。
2. 一级导航按任务域分组，Trending 下沉为 Discover，不删除现有能力。
3. 侧栏在 720px 等短窗口中可滚动且 footer 不被覆盖。
4. Workbench 无项目时只呈现聚焦空态和明确 CTA。
5. GUI 第一次启动 LAN listener 前展示本机地址候选、首选端口/递增规则、无身份校验风险并要求确认；启动后再展示实际监听地址。
6. 移动导航按任务分组，适配竖屏、横屏、软键盘和 safe-area；窄屏 Workbench inspector 与 Settings tab 保持可发现。
7. 普通小字号文本对比度达到 WCAG 2.2 4.5:1。

## 3. 非目标

- 不重做品牌、字体、色板或所有页面视觉。
- 不新增配对、token、可信设备、只读 LAN 模式或路由权限矩阵。
- 不删除 GitHub Trending；只调整入口和默认首页。
- 不把 Home 变成新的 mutation/决策真值；sidecar 只提供有界只读 dashboard read model，各 section 保留独立 loading/error/stale。

## 4. 信息架构

### 4.1 导航分组

```text
工作 Work
  首页 / 待处理 / 文件传输 / 项目 rail
知识 Knowledge
  Prompt库 / Claude历史 / 速记本 / Prompt优化 / CLAUDE.MD / Claude Plugin
连接 Connect
  设备 / 发现（GitHub Trending）
系统 System
  健康提醒 / 设置
```

分组标题不是可聚焦元素，但每组用 `section aria-labelledby` 与可感知标题关联；现有 NavItem 路由、badge 和键盘行为保持不变。项目 rail 位于 Work 分组中，不在所有系统入口之后。

### 4.2 Continue Working 首页

首页按优先级展示：

1. 最近项目与最近 active session，“继续”直接进入 Workbench deep link。
2. Attention 摘要：Human Review、Blocked、环境阻断数量，只导航。
3. 正在运行/等待审核的 Orchestrator 任务。
4. 活跃/失败 Transfer。
5. 在线设备与同步最近结果。

sidecar 新增一个只读 `HomeDashboardSummary`，内部并发构建 recent projects/sessions、Orchestrator、Transfer、Devices 五个最多 5 条的独立 section outcome；它不触发 mutation，不按项目 N+1 拉 session，单 section failure 不拖垮其余结果。Attention 继续用 provider。Home controller 仅在 document visible 时以 15 秒间隔或既有事件失效刷新，所有请求可 abort，stale cache 标时间。空数据使用真实 CTA，不编造统计。Trending 通过 Discover 入口访问。

## 5. Workbench 空态

无项目时隐藏 worktree bar、terminal tabs、终端 pane、文件入口和 inspector，仅展示：

- 主动作“添加本机项目”；
- 主动作“连接远端项目”；
- 次级“检查 tmux 依赖”；
- 一句解释“项目添加后可管理终端、文件、Git 与自动化”。

已有项目但未选中时展示项目选择空态；项目离线沿用现有 remote offline 合同，不回退成无项目。

## 6. LAN 首次确认

### 6.1 时序

```text
GUI 首次启动 → build/manage state + 创建窗口/读取本地设置 → App-level LAN 风险 gate →
用户确认 → 写入 GUI launcher-owned bootstrap store → ensure sidecar → start GUI backend services → 展示实际 listener
```

- 未确认时 GUI 同时跳过 `ensure_backend_process_for_gui` 与要求 sidecar ready 的 `start_gui_backend_services`；这不是运行时开关，确认后产品仍只有一种固定 LAN 行为。
- acknowledgement 不写 sidecar-owned `AppConfig`，而写独立的 `gui-bootstrap.json`（仅 `lanDisclosureVersion/acknowledgedAt`），由 GUI launcher 在 sidecar 出生前原子读写；sidecar 运行配置仍完全遵守 N1 单一 owner。
- 已运行的独立 backend CLI 不受 GUI onboarding 阻塞，但启动日志继续输出固定风险说明。
- disclosure 版本发生实质变化时可要求重新确认；普通文案修改不重复打扰。
- 确认页展示首选端口 62116、端口可能递增、mDNS UDP 5353，以及“同一可达网络任意设备均可读写执行，系统不验证调用者身份”。
- launcher startup coordinator 使用一个 async mutex/once gate；确认与重复点击在同一 gate 内依次 ensure + start services，启动后再展示实际 IP/port。bootstrap 读取/写入或启动失败均 fail-closed，状态为 loading/required/starting/error/pass，并提供重试与打开诊断，不把确认回滚。
- GUI 检测到独立 CLI 已在运行但本机尚未确认时，守卫仍显示风险页并回显已运行 listener；确认只允许 GUI 进入产品，不声称它阻止了既有 CLI listener。

## 7. 响应式与可访问性

- Sidebar `.content` 使用 `min-height:0; overflow-y:auto`，footer 固定在 flex 流底部，不覆盖内容。
- 移动 panel 映射固定为 Projects=`projects/worktrees`、Attention=`attention`、Work=`terminal/browser/files/git/prompt`、Automation=`automation`、More=`settings`；不新增第二套路由状态，仍映射现有 `MobileWorkbenchPanel`。
- 横屏终端优先可视高度；当前顶部菜单 + Drawer 使用 `visualViewport`/safe-area 适配软键盘，不引入不存在的 bottom nav，且不覆盖终端输入区。
- `--meta` 提升对比度，或拆为 `--fg-muted-readable` 与只用于 disabled/decorative 的 token；浅深主题同时定义。
- 1024×768、1280×720、390×844、844×390 为固定布局回归 viewport；Playwright 断言 bounding-box 不重叠、无横向滚动、键盘可达，并保存命名截图供 `huashu-design` 人工评审，本轮不宣称已有像素 diff baseline。
- Workbench 在 1024×768 与 1280×720 下，inspector 排列后仍应让首屏终端与 inspector 入口可见；Settings ≤680px 时，深链选中的 tab 必须由 tablist 自身的 `scrollTo` 显示在视口内，不移动页面主滚动容器。

## 8. 数据与错误

- Home 通过 N1 sidecar control 读取有界 dashboard summary，Attention 复用现有 provider；各 section 保留 stale snapshot，首页不触发业务 mutation。
- deep link 失效时进入目标页面并显示可恢复空态，不静默跳回首页。
- sidecar 启动失败保留风险确认结果，用户可以打开 Settings/Doctor。
- App-level `LanDisclosureGate` + `useLanDisclosureStartup` 独立于 permission onboarding，覆盖新安装与已有 `cp-onboarding-complete=1` 的升级用户；它不放在无业务的 layout 层，Welcome 的跳过/完成路径不得绕过该守卫。
- LAN 文案不得使用“认证”“可信设备”“安全设备”。

## 9. 测试与验收

1. 首页独立资源加载：一个 API reject 其余卡片仍可用。
2. 最近项目/Attention/任务/Transfer CTA 使用现有 deep link 和 authority 页面。
3. 720px 高侧栏无 footer overlap，键盘可到达所有 nav/project 动作。
4. 无项目 Workbench 不渲染禁用工具栏和空 inspector，只显示三个动作。
5. 首次未确认不由 GUI ensure sidecar；确认后只启动一次；升级 disclosureVersion 才重新提示；升级用户和 Welcome 跳过路径同样受守卫约束。
6. 固定四个 viewport 通过全页 layout assertions 与命名截图评审；移动软键盘/safe-area 在 N8 真机验证。
7. token 对比度自动检查普通正文组合不低于 4.5:1。

## 10. 持久文档

实现时更新 `docs/prd.md`、README 首次启动说明、根 `AGENTS.md` 组件清单（若新增 domain 组件）、`web/CLAUDE.md` 与 mobile 测试用例。

## 11. Spec 自审

- UX 变化建立在现有设计系统和真实页面上，不发明新视觉语言。
- LAN 确认是启动前的一次性 disclosure，不是可切换模式或身份机制。
- Home、侧栏、空态、移动布局和对比度均有可测完成条件。
