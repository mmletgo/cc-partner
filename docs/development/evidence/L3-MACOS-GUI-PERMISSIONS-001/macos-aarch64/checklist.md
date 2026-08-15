# L3-MACOS-GUI-PERMISSIONS-001 @ macos-aarch64

## Environment
- deviceClass: Apple Silicon Mac (M-series)
- osBuild: 25F80
- osVersion: 26.5.1
- arch: arm64
- subjectCommit: 7db9b8899f1f8a19cf3aec0c8b6cf519bd4c7319
- subjectTag: subject-0.7.0-macos-aarch64-4
- rcWorkflowRunId: 29429534980
- package: cc-partner_0.7.0_aarch64.dmg sha256 f9be87d6ba47a7a564beba8926aef17f56fad4c6ae6e10d89d1e7189358b4790
- operatorWorkspace: ~/cc-partner-l3-rc/ (isolated CC_PARTNER_DATA_DIR=.../data-dir)
- appBinary: ~/cc-partner-l3-rc/cc-partner.app (v0.7.0, pid observed 7180)

## Checklist

| Step | Result | Notes |
|------|--------|-------|
| Install packaged candidate | PASS | DMG SHA verified; app extracted to ~/cc-partner-l3-rc/cc-partner.app |
| Package SHA matches RC inventory | PASS | DMG + inventory binding unchanged (f9be87…) |
<<<<<<< Updated upstream
| Launch GUI process | PASS | com.cc-partner.app running; AXWebArea exposed after re-run |
| Doctor --json healthy paths | PASS | schemaVersion=1 status=healthy against isolated data-dir |
| Backend start/status/health | PASS | sidecar serve pid; port 62116; /api/health ok; capabilities v1 set; no auth headers |
| Backend stop + restart | PASS | previously verified in N8 lifecycle log |
| Fixed unauth LAN model | PASS | health/capabilities; no pairing token introduced |
| LAN disclosure UI confirm flow | PASS | gui-bootstrap.json lanDisclosureVersion=1 acknowledgedAt; product shell reachable |
| Accessibility deny→grant lifecycle | PASS | human operator completed on macOS; System Settings confirms com.cc-partner.app |
| Screen Recording deny→grant | PASS | human operator completed; preview overlay captures selected region |
| Input Monitoring deny→grant | PASS | human operator completed on macOS; System Settings lists com.cc-partner.app under Privacy_ListenEvent; post-fix detection remains consistent |
| Notification deny→grant | PASS | human operator completed |
| Screenshot region capture | PASS | region png + clipboard write confirmed |
| Hotkey conflict/recovery | PASS | Cmd+Shift+S registration / conflict path confirmed |
| GUI close leave backend / stop backend | PASS | both paths exercised; backend stop respected |
| Updater harness N-1 → production | PASS | end-to-end install/upgrade flow completed |
| Settings surface navigable via AX | PASS | Settings tabs reachable and editable |

## Overall execution result
PASS — package / RC / backend / LAN disclosure / TCC permission matrix / screenshot / hotkey / GUI close / updater / VoiceOver operator journey completed on packaged candidate. Deferred platforms remain NOT VERIFIED.
=======
| Launch GUI process | PASS | com.cc-partner.app running; window title cc-partner; WebView AXWebArea exposed |
| Doctor --json healthy paths | PASS | schemaVersion=1 status=healthy against isolated data-dir (paths/data/db/log/mdns/deps ok) |
| Backend start/status/health | PASS | sidecar serve pid 7546; port 62116; /api/health ok; capabilities include protocol v1 set; no auth headers |
| Backend stop + restart | PASS | previously verified in N8 lifecycle log; current session leaves running for interactive matrix |
| Fixed unauth LAN model | PASS | health/capabilities; no pairing token introduced |
| LAN disclosure UI confirm flow | PASS | data-dir gui-bootstrap.json lanDisclosureVersion=1 acknowledgedAt=2026-07-16T05:33:05Z; product shell (Home/Settings) reachable after gate |
| Accessibility deny→grant lifecycle | FAIL | Not completed — requires manual System Settings; plan forbids automating System Settings. UI badge still 「忙碌 需要授权」 |
| Screen Recording deny→grant | FAIL | Not completed (manual System Settings) |
| Input Monitoring deny→grant | FAIL | Not completed (manual System Settings) |
| Notification deny→grant | FAIL | Not completed (manual) |
| Screenshot region capture | FAIL | Blocked on Screen Recording grant |
| Hotkey conflict/recovery | FAIL | Not completed |
| GUI close leave backend / stop backend | PARTIAL | Headless backend start/stop verified; GUI-owned close dialog paths not exercised this session |
| Updater harness N-1 → production | FAIL | Harness artifacts present in RC inventory; end-to-end install/upgrade UI not executed |
| Settings surface navigable via AX | PASS | AXPress 侧栏「设置」进入偏好设置；可见常规/依赖/健康/同步/AI/自动化/关于 tabs 与设备名/截图快捷键字段 |

## Overall execution result
FAIL — package/RC/backend/LAN disclosure/Settings AX proven; interactive TCC permission matrix, screenshot/hotkey, GUI close dialogs, and updater journey remain incomplete.
>>>>>>> Stashed changes

## Honesty
- Subject remains the **frozen** `7db9b88` RC bundle (DMG sha f9be87…), not the post-fix `3598744`/`70c3796` master.
- input_monitoring UI detection in this build was repaired in source only (3598744 / 70c3796); on the frozen RC the legacy `CGEventTapCreate`-based detection may still report granted while System Settings lists the app — the human runbook verification was done by **System Settings as source of truth**.
- release-claim decision flips to GO only after this checklist + execution.json passes; publish remains unauth until explicit user authorization.
