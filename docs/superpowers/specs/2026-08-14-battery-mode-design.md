# 充电模式 / 无限模式设计

日期：2026-08-14  
状态：已批准，进入实现

## 1. 问题

工作台没有使用上限，用户希望用健康行为与记单词换工作时间，作为可随时退出的自我约束，而不是密码锁。

## 2. 范围

第一版只做本机账本 + 桌面 UI 门禁：

- 侧栏 footer 充电 / 无限切换（主题按钮前）
- 设置页「充电模式」tab
- 健康完成与闪卡答对入账
- 工作台 / Inbox 前台扣时（多窗只扣一份）
- 工作台 main 区耗尽遮罩；电池环与 `+Xm` 不被遮挡

不做：拦 HTTP / WS / CLI / 手机 API；杀或停已跑 Agent / Orchestrator；跨设备同步余额；Agent Hub 门禁；第三套 `data-theme`。

## 3. 模式

- `charging` | `unlimited`
- 一键切换，不设锁。切无限不抹余额。
- 入口：footer 26×26 圆按钮（图标表示目标态）+ 设置 tab 同一开关。
- 首次进入充电且从未赠送：+25 分钟，账本 `credit_welcome` 一次。

## 4. 额度（设置可改）

| 来源 | 默认 | 日上限 |
|------|------|--------|
| 喝水 `water` completed | +8 分 | 6 次 |
| 休息 `rest` completed | +20 分 | 8 次 |
| 提肛 `kegel` completed | +10 分 | 4 次 |
| 自定义习惯 completed | +10 分 | 6 次 |
| 闪卡答对 1 张 | +3 分 | 30 张 |

skip / snooze / 答错 = 0。余额上限默认 240 分钟，钳制 0，不扣负。

## 5. 消耗

- 仅 `charging` 且剩余 > 0。
- 权威时钟在 owner 后端。窗口上报 `visible && focused && pathname ∈ {/workbench,/attention}`。
- ≥1 个消耗窗计 1 倍墙钟，禁止按窗相加。
- 余额为 0 后冻结。失焦 / 其它路由 / 其它 App 不算消耗。

## 6. 入账挂钩

`habit_records` 写入 `kind=completed` 后 credit（含 acknowledge / 手动 +1 / record_water / record_rest）。  
`submit_wordgame_answer` 且 `correct` 后 credit。  
幂等：健康 `habit:<id>`；闪卡 `wordgame:<lemma>:<type>:<date>:<correct_today>`。

## 7. UI

- 主窗 footer：电池按钮在 ThemeToggle 前。充电态环形进度（满=上限）；无限态 ∞。
- 卫星窗：瘦 footer 只放电池按钮 + 剩余文案，保证环与 `+Xm` 在遮罩外。
- `+Xm` toast 挂 AppShell，不进 main。
- 遮罩只盖 `/workbench` 的 `<main>`。Inbox 不遮罩。截图/健康 overlay 旁路。
- Settings `?tab=battery` 在 health 后、activity 前。

## 8. 数据

- `config.json` `battery`：rewards / dailyCaps / maxBalanceMinutes / welcomeGrantMinutes。模式不进 localStorage。
- SQLite `battery_state`（单行）+ `battery_ledger`。禁止 `sqlx::migrate!`。

## 9. 非目标

见 §2。卫星窗算工作台；多窗只扣一份。
