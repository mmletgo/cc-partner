# Task 2 Report: Runtime UI Adapter for GUI and Headless Modes

## Status

Implemented and verified with focused tests.

## Files Changed

- `src-tauri/src/backend/ui.rs`
- `src-tauri/src/backend/mod.rs`
- `src-tauri/src/state.rs`
- `src-tauri/src/net/http_server.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/CLAUDE.md`

`src-tauri/src/lib.rs` was changed only to construct and inject the new `TauriBackendUi` into `AppState`.

## Implementation

- Added `backend::ui::BackendAsset`, `BackendUi`, `TauriBackendUi`, and `HeadlessBackendUi`.
- `TauriBackendUi` wraps `AppHandle`, emits events via `Emitter::emit`, and preserves the previous exact-match Tauri asset resolver behavior.
- `HeadlessBackendUi` emits no-op events and reads only normalized relative files from a dist directory.
- Headless asset lookup rejects empty paths, absolute paths, `..`, `.` components, Windows prefixes, and backslash-containing keys.
- Added `AppState::emit_event` and `AppState::mobile_asset`.
- Updated `/mobile` asset response to call `state.mobile_asset(&asset_key)` before existing dev/test filesystem fallback.
- Updated `src-tauri/CLAUDE.md` with the backend UI adapter and `/mobile` resource boundary.

## TDD Evidence

- RED: `cargo test backend::ui` failed because `HeadlessBackendUi` was undefined after adding the initial contract tests.
- GREEN: Implemented the adapter and confirmed `cargo test backend::ui` passed.
- Additional RED/GREEN: Added `headless_asset_rejects_windows_prefix_paths`; it failed against `C:mobile.html`, then passed after tightening Windows prefix detection.

## Verification

- `cargo test backend::ui` — passed: 3 tests.
- `cargo test net::http_server::tests` — passed: 7 tests.

## Self Review

- The new HTTP path keeps existing `mobile_asset_key`, MIME mapping, `/mobile` fallback behavior, and dev/test filesystem fallback.
- The Tauri exact-match guard moved from `http_server.rs` into `TauriBackendUi` without changing the resolver decision logic.
- The headless adapter uses synchronous `std::fs::read` because the `BackendUi` trait is synchronous and intended for small static assets.

## Concerns

- Strictly removing `AppState.app_handle` would currently break many non-task files (`commands/workbench.rs`, Orchestrator routes, transfer routes, etc.). To keep focused tests compiling without expanding scope, this task adds `ui` while retaining `app_handle` as a compatibility field for Task 3/4 to remove.
