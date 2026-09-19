# Flutter Mobile P7 — Push register / fan-out / skip-if-no-relay

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Advertise `mobile.push.v1` with register/unregister; upsert by `mobileDeviceId`; skip send when relay is unconfigured; payloads have no terminal/path/prompt.

**Architecture:** SQLite `mobile_push_registrations` + optional relay URL/token. LAN routes stay credential-free.

**Tech Stack:** Rust sqlx, axum, Dart payload builder (already in `mobile/lib/push/payload.dart`).
