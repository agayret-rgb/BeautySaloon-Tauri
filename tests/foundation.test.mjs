import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

test("React shell keeps the required navigation and appointment action", async () => {
  const source = await readFile(new URL("../src/App.tsx", import.meta.url), "utf8");

  for (const label of ["Bugun", "Takvim", "Musteriler", "Hizmetler", "Personel", "Ayarlar"]) {
    assert.match(source, new RegExp(`"${label}"`));
  }
  assert.match(source, /\+ Yeni Randevu/);
  assert.match(source, /listAppointmentsByDate/);
});

test("Tauri Rust boundary exposes only named core data commands", async () => {
  const source = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  assert.match(source, /app_health/);
  for (const command of [
    "customer_create",
    "customer_search",
    "staff_list",
    "staff_set_services",
    "service_list",
    "appointment_create",
    "appointment_update",
    "appointment_list_by_date",
    "database_create_backup",
    "database_clean_start",
    "google_sync_pending_mock",
    "reminder_reconcile_all_mock",
    "whatsapp_list_candidates",
    "auth_mock_verify_otp",
    "auth_logout",
    "google_calendar_status",
    "cloud_connection_status",
    "cloud_request_otp",
    "cloud_verify_otp",
    "cloud_disconnect"
  ]) {
    assert.match(source, new RegExp(command));
  }
  assert.doesNotMatch(source, /read_file/);
  assert.doesNotMatch(source, /execute_sql/);
  assert.doesNotMatch(source, /executeSql/);
  assert.doesNotMatch(source, /arbitrary_http/i);
});

test("Frontend API exposes typed service commands without raw secrets or arbitrary transport", async () => {
  const source = await readFile(new URL("../src/tauriApi.ts", import.meta.url), "utf8");
  for (const name of ["getGoogleCalendarStatus", "getCloudConnectionStatus", "requestCloudOtp", "verifyCloudOtp", "disconnectCloud"]) {
    assert.match(source, new RegExp(`export async function ${name}`));
  }
  assert.doesNotMatch(source, /accessToken/);
  assert.doesNotMatch(source, /refreshToken/);
  assert.doesNotMatch(source, /clientSecret/);
  assert.doesNotMatch(source, /fetch\(/);
  assert.doesNotMatch(source, /httpRequest/);
});
