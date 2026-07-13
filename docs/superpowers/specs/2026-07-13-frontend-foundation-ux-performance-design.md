# cc-partner 前端基础、交互与性能设计

- 日期：2026-07-13
- 状态：方案已确认，可进入 implementation plan
- 范围：design token、模态交互、键盘/动效、错误隔离、拆包、终端缓存、信息架构、i18n 与巨型文件渐进拆分

## 1. 目标

本方案建立一组可自动验证的前端底座，使视觉 token 不再静默失效、模态与 tab 对键盘用户可靠、移动端首载不携带桌面重型依赖、终端高频输出不反复复制大字符串，并让后续拆分大文件有稳定边界。

目标按顺序为：先建立 contract，再修复当前违规，再迁移交互，最后做拆包和结构拆分。不得为追求行数或 chunk 数牺牲现有 Workbench terminal 常驻、replay、dirty guard、Attention 导航-only 等产品合同。

## 2. Design token contract

当前 `--bg-2`、`--bg-primary`、`--bg-elevated`、`--fg-muted`、`--border-subtle`、`--border-strong`、`--warning` 等引用没有定义。它们不新增兼容别名，而映射到已有 canonical token：

| 未定义引用 | canonical |
|---|---|
| `--bg-1`、`--bg-elevated` | `--surface` |
| `--bg-2`、`--bg-primary` | `--bg` |
| `--fg-3`、`--fg-muted` | `--meta` 或按对比度语义使用 `--muted` |
| `--border-subtle` | `--border-soft` |
| `--border-strong` | `--border` |
| `--warning` | `--warn` |

CI 新增 `check:css-tokens`：解析全部 CSS 的 `var(--name)`，语义 token 必须在 `tokens.css` 定义；颜色、字体、间距、圆角、阴影、动效、z-index token 在浅/深主题都必须满足现有规则。由 TSX inline style 注入的结构变量仅允许明确 allowlist：`--prompt-panel-left`、`--prompt-panel-top`、`--git-graph-color`。禁止宽泛 baseline 或忽略未知变量。

## 3. Dialog 与 Drawer

新增无业务语义的 `Dialog`、`Drawer` primitives，共享 `useModalLayer`：

```ts
interface DialogProps {
  open: boolean;
  titleId: string;
  children: ReactNode;
  initialFocusRef?: RefObject<HTMLElement | null>;
  closeOnEscape?: boolean;
  closeOnBackdrop?: boolean;
  onClose: () => void;
}

interface DrawerProps extends DialogProps {
  side?: 'left' | 'right';
}
```

两者 portal 到 `document.body`，打开后保存触发元素、将初始焦点放入 surface、Tab/Shift+Tab 在 surface 循环、Escape 按配置关闭、背景 siblings 设置 `inert` 与 `aria-hidden` 并在嵌套层计数恢复、关闭后恢复原触发元素焦点。Backdrop 不承载 dialog role；surface 使用 `role="dialog" aria-modal="true" aria-labelledby`。滚动锁同样引用计数，嵌套 modal 不提前解除。

迁移所有现有业务模态，不在业务页面重复 focus trap。移动导航抽屉仅在窄屏作为 modal Drawer；宽屏 rail 不具备 modal 语义。抽屉打开后焦点进关闭按钮，关闭/选择导航/Escape 后回到 menu button。

## 4. 单一交互与键盘语义

- Attention：每个条目只保留一个 `<button>` 导航目标；整行可点击，右侧动作文案作为同一按钮内部视觉元素，不再出现 role=button 内嵌 button。桌面与移动端 DOM 语义一致。
- Transfer dropzone：Enter/Space 与点击相同，触发原生 file dialog。
- Workbench terminal tabs：选中项 `tabIndex=0`，其余 `-1`；ArrowLeft/Right 循环、Home/End 跳转并激活；close button 不嵌入 tab，关闭选中项后焦点落到相邻项或“新建终端”。
- Workbench inspector tabs 同样使用 roving focus、`aria-controls`/`tabpanel`。文件 tabs 已有方向键实现，只补齐共享 contract test，不重写。
- `prefers-reduced-motion: reduce` 时全局 transition/animation duration 归零，保留必要的状态变化但不做位移、闪烁或 skeleton shimmer。

## 5. 路由错误隔离

`RouteErrorBoundary` 包裹 AppShell 内每个 lazy route，保留侧栏和 providers。错误页展示本地化摘要、重试当前 route 和返回首页；开发环境可显示 error message，生产不显示 stack。`resetKey` 随 pathname 改变，导航后自动复位。Screenshot/Health overlay 使用独立 boundary，不能因主路由错误白屏。

## 6. 拆包与 source map

所有 App routes 使用 `React.lazy`；DesignSystem 只在 dev 动态 import。Workbench 文件编辑器按类型动态加载 CodeMirror/Tiptap/HTML preview；Health route 独占 Recharts。移动端默认 Projects shell 同步加载，terminal/files/automation/browser 等重面板按激活面板 lazy load，使 mobile initial graph 不包含 xterm、CodeMirror、Tiptap、Recharts。

Vite 构建输出可机器读取的 chunk graph。预算以 gzip 后的静态 initial import closure 计算：

- desktop `main` initial graph ≤ 320 KiB gzip；
- `mobile` initial graph ≤ 280 KiB gzip；
- mobile initial graph 禁止包含 `@xterm/*`、`@tiptap/*`、`@uiw/react-codemirror`、`codemirror`、`recharts` 模块。

默认生产 `sourcemap=false`。只有 `CC_PARTNER_SOURCEMAP=1` 时生成 `hidden` sourcemap，供受控 CI artifact/Sentry 类上传流程使用；map 不进入 Tauri release bundle。CI build 后执行 bundle contract，超预算或依赖泄漏即失败。

## 7. Terminal ring buffer 与帧批处理

外部 store API 保持 `getBuffer/getRevision/subscribe/append/reset/remove`，内部每 session 改为 chunk deque：追加仅 push chunk，超过 200,000 UTF-16 code units 时从头裁剪；只有 `getBuffer` 需要 snapshot 时才 join，并缓存 materialized string。多个 append 在同一 animation frame 只 bump revision/notify 一次；reset/remove 立即使旧 scheduled notification 失效。

Node/Vitest 注入 deterministic frame scheduler。桌面 Tauri event 与移动 NDJSON 继续写同一 store；xterm replay/diff 语义、200,000 上限和 session 隔离不变。

## 8. Workbench 信息架构

侧栏保留现有一级路由，但项目 rail 增加明确分区标题“工作台项目”、空态说明和“添加本机项目/选择局域网项目”入口。选中项目保持现有 deep link 行为；不把项目伪装成普通全局导航项。窄侧栏/键盘下分区标题与项目名称可读，状态不只靠颜色。

本轮不重做 Workbench 页面视觉语言，不变更自动化、terminal、files、Git 的层级；只提升旗舰功能可发现性。

## 9. i18n contract

新增 AST 检查脚本扫描生产 `.tsx`：包含中文或拉丁字母的 JSXText，以及 `title/aria-label/placeholder/alt` 字符串属性必须来自 `t()` 或允许的品牌/技术词 allowlist。测试、DesignSystem、代码示例和纯符号排除。脚本不得使用无限增长 baseline；现有违规先全部迁移到对应 zh/en namespace，再启用 CI。

运行时错误必须通过已有格式化函数嵌入本地化模板；后端错误原文可作为 `{error}` 参数，不作为静态 UI 文案。新增 key 两种语言同提交，locale parity test 必须通过。

## 10. 巨型文件渐进拆分

按职责拆分，禁止重复已经完成的 Workbench controller extraction：

- `Settings.tsx`：页面只持有 tab/layout，资源加载和保存进入 `useSettingsController`，业务 panel 各自文件化。
- `Orchestrator.tsx`：board/filters、task detail drawer、create dialog、outbox 分为 view；请求与 action orchestration 进入 controller，但任务状态机仍由后端权威决定。
- `MobileAutomationPanel.tsx`：task list/detail/create/outbox 分 view，transport/action controller 独立。
- `lib/types.ts`：按 `core/settings/workbench/orchestrator/attention` 域拆文件，保留 `@/lib/types` barrel，避免一次改全仓 import。
- `Workbench.tsx`：只修正当前 1,222 行超限的终端 tab/布局叶子，目标 ≤1,200；不新建第八个总 controller，不迁移已抽出的领域逻辑。

每个拆分先加 characterization test，单次只拆一个边界；禁止同时改产品行为。Settings 的局部容错功能先由核心完整性计划完成，随后本计划做行为保持型抽离。

## 11. UX 专家评审门禁

本轮沿用 cc-partner 现有 design system、品牌气质与真实业务内容，不做脱离产品上下文的风格探索，也不添加装饰性渐变、无信息图标、虚构统计或新的临时色板。Workbench 属于高密度生产力工具，信息密度必须来自项目、任务、终端、文件和运行状态等真实上下文，而不是额外卡片层级。

实施收口时用 Playwright 固定截图覆盖浅/深主题、桌面/手机宽度，以及 normal、empty、loading、partial-error、offline、modal/drawer、dense terminal/task 状态。按 App UI 的五维评审框架检查：功能性与细节执行为最高优先级，视觉层级次之，哲学一致性用于确认仍属于现有产品，创新性仅用于排除模板化与 AI slop，不以追求新奇扩大重设计范围。

每个发现必须落到可验证的具体动作：交互或可访问性问题进入自动化测试；对齐、间距、层级和响应式问题进入截图对比与人工 smoke；纯个人审美偏好不进入实现清单。评审结果记录在实施 PR checklist/测试证据中，不新建一次性总结 Markdown。

## 12. 验收

必须验证：token contract 能对故意写入的未知 token 失败；Dialog/Drawer 键盘循环、Escape、inert、焦点恢复；Attention 单 tab stop；terminal/inspector roving focus；reduced motion；route error fallback；desktop/mobile chunk 预算与 forbidden module；ring buffer 高频 append 只通知一次且内容完整；i18n AST contract；各大文件拆分前后的 characterization。

最终运行 `npm run lint && npm run build && npm test && npm run test:e2e`。视觉 QA 覆盖浅/深主题、桌面键盘、手机窄屏 drawer 和 reduced-motion。真实 VoiceOver/NVDA、三平台 WebView 焦点行为列为人工 smoke。

## 13. 非目标与依赖

- 不引入 Redux/Zustand、CSS framework、第三方 modal 或完整 design-system 库。
- 不改变 Attention source、Orchestrator 状态机或 Workbench controller ownership。
- 不承诺一次 PR 完成全部拆分；每个 task 保持可运行、可独立回滚。
- 本计划依赖核心完整性计划先完成 Settings loader 语义；token、Dialog、拆包、ring buffer 可在不冲突 worktree 中先行。

实现后同步根 `AGENTS.md` 组件清单、`web/CLAUDE.md` 的 modal/bundle/i18n/文件边界、`docs/prd.md` 的 Workbench 项目 rail 和可访问交互持久行为。
