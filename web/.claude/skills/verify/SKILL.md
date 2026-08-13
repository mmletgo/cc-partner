---
name: verify
description: Drive the Vite desktop SPA to verify AppShell / Workbench sidebar changes
---

# web/ verify

## Launch

- Dev server: `cd web && npm run dev` → `http://127.0.0.1:5173`
- Playwright already listed in `web/package.json`; reuse `:5173` when listening
- Desktop shell is a Vite SPA. Tauri `invoke` must be stubbed with `page.addInitScript` (see `tests/frontend-foundation.spec.ts` / `tests/support/appBootstrap.ts`)

## Drive sidebar / Workbench entry

1. Skip onboarding: `localStorage cp-permission-onboarded=1`, `cp-lang=zh`
2. Mock at least: `check_permissions`, `get_lan_disclosure_status`, `list_workbench_projects`, `list_attention_items`, `get_config`, `get_version`
3. Open `/` and read `navigation[name=主导航]`
4. Assert no `a[href="/workbench"]`; Workbench entry is `region[name=工作台项目]` + project buttons
5. Click a project → URL becomes `/workbench`

## Gotchas

- Opening `/workbench` without `list_workbench_git_commits` returning an array crashes `WorkbenchGitInspector` (`commits is not iterable`)
- `npm run lint` scans the whole `web/` tree; pre-existing AgentHub/mobile hook errors are unrelated
