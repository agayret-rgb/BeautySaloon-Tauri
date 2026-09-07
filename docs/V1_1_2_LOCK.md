# BeautySaloon V1.1.2 Release Lock

- Release: `BeautySaloon V1.1.2`
- Status: `LOCKED`
- Installer: `BeautySaloon_1.1.2_x64-setup.exe`
- Installer SHA-256: `51D87BE96BE1198B3509F8021F26D3F6D009BEBB1FDBA44A3C98753257378331`
- Desktop accepted commits: `e90b8a3`, `20093c2`, `affaed8`
- Cloud/server accepted commits: `1a35f74`, `2da876e`

BeautySaloon remains local-first; local business data is authoritative. The
desktop release gates passed: Rust 77 passed, 0 failed, 7 explicit live tests
ignored; frontend 5 passed, 0 failed; formatting, typecheck, lint, and diff
checks passed. The targeted cloud reminder suite passed: 5 files and 28 tests.
The legacy cloud repository's broad test suite is not represented by this lock.

Windows installation acceptance passed with V1.1.2, no console window, and
preserved business data. Google OAuth persisted across restart. Google outbox
state remains intentionally untouched: 8 pending CREATE rows for active
appointments and 1 synced row. Those pending rows are deferred for later
controlled validation and were not processed during this release lock.

WhatsApp provider acceptance passed with one controlled live delivery. OTP and
cloud-session persistence passed, including refresh of expired access tokens;
only an authoritatively invalid refresh/session clears the desktop session.
Provider metadata and credentials remain server-side only. No provider secrets
are stored in desktop SQLite or frontend configuration.

Automatic reminders are enabled. Final dispatcher state is RUNNING, with no
pause reason, no pending reminders, no processing reminders, and no currently
due reminders. The desktop readiness state is: `Hazır / Hatırlatmalar aktif`.

The dispatcher fix addresses PostgreSQL `claim_next_due_reminder(...)`, which
returns `RETURNS public.cloud_reminders`. A no-result `RETURN NULL` crosses the
RPC boundary as an all-null composite object. The prior dispatcher treated it
as a reminder and failed with `INVALID_TEMPLATE_PARAMETERS` at claim time,
then paused. Cloud commit `1a35f74` treats that all-null composite as no due
reminder. Cloud commit `2da876e` also fixes diagnostic classification precedence
so `INVALID_TEMPLATE_PARAMETERS` is not shortened to `INVALID_TEMPLATE`.

No production data was deleted, no Google outbox row was processed, no live
reminder was created or sent during the lock, and no push was performed.

V1.1.2 functionality is frozen by this lock. Future functional changes require
a new version or follow-up phase. Known post-lock follow-ups: Google OAuth is
still in Testing mode and may be promoted to Production after several days of
real use; the intentionally deferred Google pending CREATE rows require a later
controlled validation.
