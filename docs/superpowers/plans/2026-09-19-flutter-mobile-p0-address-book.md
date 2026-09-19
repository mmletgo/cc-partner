# Flutter Mobile P0 — Address Book + LAN HTTP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a top-level `mobile/` Dart package that persists multiple PC `http://host:port` entries, parses desktop `/mobile` URLs, and talks to a PC without a browser Origin header.

**Architecture:** Pure Dart (no Flutter SDK import in P0) so `dart test` runs without Xcode. Address book, URL parse, and `dart:io` HTTP live in `mobile/lib`. Flutter widgets arrive in later phases. Persistence is a JSON file injected via `AddressBookStore`.

**Tech Stack:** Dart 3, `package:test`, `dart:io` HttpClient / HttpServer.

**Spec:** `docs/superpowers/specs/2026-09-19-flutter-mobile-app-design.md` §6, §5 LAN client.

---

## File map

- Create: `mobile/pubspec.yaml`
- Create: `mobile/analysis_options.yaml`
- Create: `mobile/lib/core/server_url.dart` — parse host/port, default `62116`, strip `/mobile`
- Create: `mobile/lib/core/lan_http.dart` — GET/POST, omit Origin, Host from baseUrl
- Create: `mobile/lib/address_book/models.dart`
- Create: `mobile/lib/address_book/book.dart`
- Create: `mobile/test/server_url_test.dart`
- Create: `mobile/test/address_book_test.dart`
- Create: `mobile/test/lan_http_test.dart`
- Create: `mobile/AGENTS.md`
- Modify: `AGENTS.md` — add `mobile/` to the directory map
- Modify: `.gitignore` — `.dart_tool/`, `mobile/.dart_tool/`

Do not edit `web/src/mobile/`.

---

### Task 1: Server URL parse

**Files:**
- Create: `mobile/lib/core/server_url.dart`
- Test: `mobile/test/server_url_test.dart`

- [ ] **Step 1: Write failing tests** for: `http://192.168.1.8:62116/mobile` → host+port only; missing port → `62116`; `host:62117`; duplicate path `/mobile/foo` stripped; default scheme http.

- [ ] **Step 2: Implement `parseServerInput`** returning `{host, port, baseUrl}` with `baseUrl = http://host:port` (no trailing slash). Normalize host to lowercase. Reject empty host.

- [ ] **Step 3: Run `dart test test/server_url_test.dart`** — all pass.

---

### Task 2: Address book

**Files:**
- Create: `mobile/lib/address_book/models.dart`
- Create: `mobile/lib/address_book/book.dart`
- Test: `mobile/test/address_book_test.dart`

`ServerRecord` fields: `id, name, host, port, baseUrl, pcDeviceId, deviceName, protocolVersion, capabilities, lastHealth, lastUsedAt, lastLocation, pushIntent`.

`lastHealth`: `online | unreachable | unsupported`.

- [ ] **Step 1: Tests**
  - same normalized host:port updates existing row (no second entry)
  - health fail + `force: true` saves with `lastHealth=unreachable` (never `online`)
  - health fail + `force: false` does not save
  - `switchActive` calls disconnect for the previous active connection and does **not** clear other rows' `lastLocation` or `pushIntent`
  - `lastLocation` round-trip via JSON store

- [ ] **Step 2: Implement `AddressBook`** with in-memory list + `AddressBookStore` (`load`/`save` JSON). Inject `HealthProbe` so tests do not hit the network.

- [ ] **Step 3: `dart test test/address_book_test.dart`** — pass.

---

### Task 3: LAN HTTP client

**Files:**
- Create: `mobile/lib/core/lan_http.dart`
- Test: `mobile/test/lan_http_test.dart`

- [ ] **Step 1: Tests against a local `HttpServer`:** request has no `origin` header; `host` header equals `baseUrl` host:port; GET `/api/health` returns body.

- [ ] **Step 2: Implement `LanHttpClient.getJson/postJson(baseUrl, path)`** using `dart:io` `HttpClient`. Do not set Origin. Let Host come from the request URI (which is `{baseUrl}{path}`).

- [ ] **Step 3: `dart test test/lan_http_test.dart`** — pass.

---

### Task 4: Package + docs

- [ ] **Step 1:** `pubspec.yaml` name `cc_partner_mobile`, SDK `>=3.5.0 <4.0.0`, `dev_dependencies: test`.
- [ ] **Step 2:** Root `AGENTS.md` directory map line: `mobile/` Flutter/Dart 原生客户端（地址簿 + 局域网 HTTP；不改 `web/src/mobile/`）。
- [ ] **Step 3:** `mobile/AGENTS.md` — test command `cd mobile && dart test`.
- [ ] **Step 4:** Commit on `feat/flutter-mobile-app`.
