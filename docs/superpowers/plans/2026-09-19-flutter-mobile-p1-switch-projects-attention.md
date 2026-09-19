# Flutter Mobile P1 — Switch, Projects, Attention, Settings

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Active-server switch restores `lastLocation`; projects list/open/remove on the current PC; Attention is navigate-only and hides tmux/dependency items; settings show the fixed LAN risk copy.

**Architecture:** Pure Dart view-models on `LanHttpClient` + `AddressBook`. No Flutter widgets in this phase.

**Tech Stack:** Dart 3, `package:test`.

---

## File map

- Create: `mobile/lib/settings/risk_copy.dart`
- Create: `mobile/lib/attention/filter.dart`
- Create: `mobile/lib/projects/client.dart`
- Create: `mobile/lib/nav/location.dart`
- Test: `mobile/test/attention_filter_test.dart`
- Test: `mobile/test/risk_copy_test.dart`
- Test: `mobile/test/projects_client_test.dart`

Switch/`lastLocation` already covered in P0 `address_book_test.dart`.

---

### Task 1: Attention filter

Hide `sourceKind == workbenchDependency` and `target.kind == settings` (tmux/deps). Navigate-only: no execute actions in the model.

### Task 2: Risk copy

Exact sentence: `同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。`

### Task 3: Projects client

`GET/POST` current PC `/api/mobile/workbench/projects/*` via `LanHttpClient`. Tests use local HttpServer.
