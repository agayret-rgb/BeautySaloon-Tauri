# BeautySaloon V1.1.3 Release Lock

- Release: `BeautySaloon V1.1.3`
- Status: `LOCKED / ACCEPTED`
- Baseline: `V1.1.2`
- Final HEAD: `e41b36e`
- Schema: `v19`
- Installer: `BeautySaloon_1.1.3_x64-setup.exe`
- Installer SHA-256: `9C3BFB78F1EDCBB2E19D2189C8B216769BEE7D3ED59187DF631B266455D5F709`

V1.1.3 adds a per-appointment WhatsApp reminder preference, defaulting to ON,
while retaining customer consent and valid-phone gates. Today shows the
preference indicator without changing reminder send eligibility.

The Services workflow supports category create, rename, filtering, and grouped
views. `Tümü` is a view-only filter. `Genel` is no longer displayed, created,
defaulted, or used by the normal workflow. A service belongs to exactly one
user-created category and requires an explicit category selection. The current
test/live database may retain one old unreferenced `Genel` category row with no
services; it was intentionally not mutated during release locking.

Calendar editing requires the explicit `Düzenle` action. Appointment status can
be corrected later, save errors are shown near Save, and no automatic appointment
completion was introduced. Customer creation is independent, customer edit
supports name, phone, and notes, and Week view covers Monday through Saturday.

SQLite remains the source of truth. Google one-way behavior and the WhatsApp
provider, cloud, and dispatcher architecture are unchanged. No provider secrets
were introduced into the desktop application.

Package A, B, and C targeted gates passed. The integrated RC gate passed after
a test-only audit ordering correction; production audit behavior was unchanged.
Final UX-correction and `Genel`-removal targeted tests, typecheck, lint, and
diff checks passed. Final manual acceptance passed all tests.

No production/live data changed, no external API was called, and no push was
performed during the final release lock.
