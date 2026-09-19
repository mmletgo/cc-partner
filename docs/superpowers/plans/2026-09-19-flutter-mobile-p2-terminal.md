# Flutter Mobile P2 — Terminal channel

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Terminal input WS + NDJSON events state machine: `gap` forces replay; unacked input is never auto-replayed; never fall back to `/sessions/write`; album paste uses `paste-image`.

**Architecture:** Pure Dart `TerminalController`. IO (WS/HTTP) injected.

**Tech Stack:** Dart 3, `package:test`.

---

## File map

- Create: `mobile/lib/terminal/controller.dart`
- Test: `mobile/test/terminal_controller_test.dart`
