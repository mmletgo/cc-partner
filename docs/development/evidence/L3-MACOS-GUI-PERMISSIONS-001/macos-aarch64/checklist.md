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

## Checklist

| Step | Result | Notes |
|------|--------|-------|
| Install packaged candidate | PASS | DMG mounted; app copied to /tmp/cc-partner-rc-app |
| Package SHA matches RC inventory | PASS | DMG + app.tar.gz sha verified |
| Launch GUI process | PASS | process running (Contents/MacOS/app) |
| Doctor --json healthy paths | PASS | schemaVersion=1 status=healthy with isolated CC_PARTNER_DATA_DIR |
| Backend start/status/health | PASS | port 62116; /api/health ok; capabilities include protocol v1 set; no auth headers |
| Backend stop + restart | PASS | status stopped then running |
| Fixed unauth LAN model | PASS | health/capabilities; no pairing token introduced |
| LAN disclosure UI confirm flow | FAIL | GUI window not AX-inspectable; System Events saw 0 windows for process; universalAccessAuthWarn present — interactive disclosure not completed in this agent session |
| Accessibility deny→grant lifecycle | FAIL | Not completed (requires manual System Settings; plan forbids automating System Settings) |
| Screen Recording deny→grant | FAIL | Not completed (manual) |
| Input Monitoring deny→grant | FAIL | Not completed (manual) |
| Notification deny→grant | FAIL | Not completed (manual) |
| Screenshot region capture | FAIL | Not completed without Screen Recording grant |
| Hotkey conflict/recovery | FAIL | Not completed |
| GUI close leave backend / stop backend | PARTIAL | Headless backend start/stop verified; GUI-owned close dialog paths not exercised |
| Updater harness N-1 → production | FAIL | Harness artifacts present in RC inventory but end-to-end install/upgrade UI not executed |

## Overall execution result
FAIL — package/RC/backend lifecycle proven; interactive GUI permission matrix and updater journey not completed this session.

## Honesty
No PASS claimed for incomplete interactive rows. Deferred platforms remain NOT VERIFIED.
