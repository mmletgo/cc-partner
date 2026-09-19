# Flutter Mobile P4–P6 — Git, Transfer, Provider

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Worktree/Git mutations on the current PC; host-relay transfer with immediate upload and no host paths in task JSON; Provider on the current PC only (`provider-manager.v1` or unsupported; no CLI install).

**Architecture:** Three independent Dart clients on `LanHttpClient`.

---

## File map

- Create: `mobile/lib/git/client.dart`
- Create: `mobile/lib/transfer/client.dart`
- Create: `mobile/lib/provider/client.dart`
- Test: `mobile/test/git_client_test.dart`
- Test: `mobile/test/transfer_client_test.dart`
- Test: `mobile/test/provider_client_test.dart`
