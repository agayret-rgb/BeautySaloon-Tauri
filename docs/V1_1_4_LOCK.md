# BeautySaloon V1.1.4 Release Lock

- Release: `BeautySaloon V1.1.4`
- Status: `LOCKED`
- Baseline: `V1.1.3`
- Lock date: `2026-09-15`
- Schema: `v19`
- Release EXE: `beautysaloon-tauri.exe`
- Release EXE SHA-256: `3D724BCBC663E56437372E353F23CADD7CE4D3A40CECFBAA387B5542BAEEB19A`
- Installer: `BeautySaloon_1.1.4_x64-setup.exe`
- Installer SHA-256: `9D7FA113DBC8D47081394A51AC8F8A6468631B6A0877554E5C30E428D13DFD17`

V1.1.4 makes reminder-cloud projection recover safely from terminal and retryable
cloud outcomes. It preserves safe cloud error codes, prevents poison-outbox
blocking, avoids unnecessary revision churn, and keeps sent, failed, or processing
reminders from returning to local pending state. Desktop-to-cloud projection remains
available while the dispatcher is paused; the desktop still has no provider-send
capability.

`REMINDER_TOO_LATE` entries can self-heal without poisoning ordered outbox work.
`REMINDER_TERMINAL` cancel entries become terminal blocked records and do not delay
later reminders. Authentication failure retry and reminder identity/idempotency
semantics are preserved.

Production acceptance used a permanent Meta System User token. The target cloud
reminder reached `sent` with no failure code, and physical WhatsApp delivery was
confirmed. No token or secret value is recorded here.

Final dispatcher state is `PAUSED` with pause reason
`PERMANENT_TOKEN_ACCEPTANCE_COMPLETE`.

Quality gates passed: Rust `100 passed / 0 failed / 7 ignored`; frontend
`5 passed / 0 failed`; fmt; clippy with `-D warnings`; frontend typecheck, lint,
and production build; diff check; secret scan; and the `REMINDER_TERMINAL` and
`REMINDER_TOO_LATE` regressions.

No production/live data was changed, no migration or deployment was performed, and
the dispatcher remained paused during final release locking.
