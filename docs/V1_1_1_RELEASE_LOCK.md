# BeautySaloon V1.1.1 Release Lock

- Maintenance release based on locked V1.1.
- Installer: `BeautySaloon_1.1.1_x64-setup.exe`
- SHA-256: `59F3DA3C7645D3AD4F6BA011E242396D1C77FAD5F61C08B4A0977F2B0C89B207`
- Identifier: `com.beautysaloon.desktop`
- NSIS install mode: `currentUser`
- Installer is unsigned.

Real Windows installation and smoke testing passed with installed version 1.1.1.
Live business data was preserved. Customer name selection and search, root
navigation, the unsaved Kal / Cik guard, appointment rescheduling, and conflict
prevention with its user-facing message all passed. Automated release gates
passed. The database schema remains v18.

Known accepted limitation: New Appointment phone-number customer filtering is
not fully reliable in real use. Phone remains optional; name search is the
accepted V1.1.1 selection path.

No push was performed.
