# BeautySaloon V1.1.5 Release Lock

- Release: `BeautySaloon V1.1.5`
- Status: `LOCKED`
- Baseline: `V1.1.4`
- Lock date: `2026-09-15`
- Scope: `WhatsApp Customer Consent Fix`
- Release EXE: `beautysaloon-tauri.exe`
- Release EXE SHA-256: `136C9DCC8D320374ABE67E149CF841843DAF85CB99E3BB35244A321885A3F877`
- Installer: `BeautySaloon_1.1.5_x64-setup.exe`
- Installer SHA-256: `45330FD50F2F8C6A198C531693BCB0FEDE9C58AAD03FA8630FF595C825C7BC1C`

V1.1.5 makes new customer WhatsApp reminder and consent defaults enabled and
confirmed. The customer create and edit form exposes both controls, loads the
stored values for existing customers, and lets the user change them. Rust creation
defaults missing values to `true` / `true`; Rust updates preserve existing values
when their optional input fields are absent.

Booking's inline customer creation uses the same enabled and confirmed defaults.
The appointment WhatsApp consent eligibility gate remains in place. No migration
was applied for existing customers.

Local acceptance passed for new-customer defaults, existing customer edit behavior,
and appointment WhatsApp eligibility. No real WhatsApp message was sent. Acceptance
test reminders were cancelled, and the dispatcher remained `PAUSED` with pause
reason `PERMANENT_TOKEN_ACCEPTANCE_COMPLETE`.

Quality gates passed: Rust `101 passed / 0 failed / 7 ignored`; frontend
`5 passed / 0 failed`; fmt; clippy with `-D warnings`; frontend typecheck, lint,
and production build; diff check; secret scan; customer consent regression;
reminder eligibility regression; and React consent regression.

No production/cloud data was changed, no migration or deployment was performed,
and no token or secret value is recorded here.
