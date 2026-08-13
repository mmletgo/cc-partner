# 健康提醒模板化设计

日期：2026-08-14

## 问题

饮水与休息原先是两套硬编码产品线（间隔打卡 vs 久坐状态机 + 全屏倒计时）。用户需要在设置里直接配置，并出厂预置饮水 / 休息 / 提肛。

## 决策

- 每条模板自选触发：`sedentary` 或 `interval`
- 多条久坐共用一条键鼠活跃时钟，各自独立阈值；完成/跳过一条不重置其它久坐计时
- 每条模板自选完成：`instant` 或 `session`
- 可新增自定义模板；`water` / `rest` / `kegel` 不可删除，只能关；上限 12
- 提肛出厂：间隔 2 小时 + 30 秒倒计时；文案克制，不写医学/解剖细节
- 全屏遮罩互斥 FIFO 排队（同 id 去重）；系统通知可同时出；应用内 toast 保持停用
- 健康数据不进 P2P/云同步
- 旧标量 `workWindowSeconds` / `waterIntervalSeconds` / `waterEnabled` / `reminderFullscreen` 作兼容镜像
- 新表 `habit_records` 双写，旧 `water_records` / `rest_records` 保留以便回滚
- `skip_reminder` 只处理 rest，不再重置整机

## 出厂三项

| id | 触发 | 完成 | 默认参数 |
|---|---|---|---|
| water | interval | instant | 3600s |
| rest | sedentary | session | 2700s / 300s |
| kegel | interval | session | 7200s / 30s |

## 运行时

`HealthStateMachine` 只负责 Idle/Working/Resting 与有效休息关窗。每模板 `TemplateRuntime { pending, last_completed_ts, reminded_this_window, snooze_until }`。session 到点不重置共享时钟——默认休息 5 分钟会自然关窗，提肛 30 秒不会。

## 前端

设置页 `HealthPanel`：总开关 + 有效休息 + 模板列表 + 添加提醒 + DND/通知。Overlay 走 `/health-overlay?display=&template=`，旧 `type=` 兼容。Listener 用事件载荷 title/body。
