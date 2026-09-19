# Flutter Mobile P3 — File workspace

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** File workspace types (code/Markdown/HTML/image/CSV/SQLite) with dirty-guard on context switch; HTML preview scripts off, relative assets as data URLs.

**Architecture:** Pure Dart preview policy + dirty guard.

---

## File map

- Create: `mobile/lib/files/workspace.dart`
- Create: `mobile/lib/files/html_preview.dart`
- Test: `mobile/test/file_workspace_test.dart`
- Test: `mobile/test/html_preview_test.dart`
