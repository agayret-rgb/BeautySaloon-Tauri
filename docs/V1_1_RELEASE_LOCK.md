# BeautySaloon V1.1 Release Lock

- Version: `1.1.0`
- Release candidate: `BeautySaloon_1.1.0_x64-setup.exe`
- SHA-256: `2E05BF69D7C855A0AEE6FBD0821A0AE9CF4212481CEEFAB7D1D104539D9FB04A`
- Installer: unsigned NSIS, `currentUser`; normal install location is
  `%LOCALAPPDATA%\\BeautySaloon`.

Acceptance completed:

- Offline real-v13 migration rehearsal passed with the explicit-path, test-only
  migration harness.
- Real live v13-to-v18 migration passed on first start; second start was
  idempotent.
- Business data, IDs, and appointment relationships were preserved.
- Integrity and foreign-key checks passed.
- The pre-migration preservation snapshot remains retained.
- A standalone v18 SQLite backup made through SQLite's backup API reopened
  independently and passed the same integrity and preservation checks.

No installer signing or push was performed for this release lock.
