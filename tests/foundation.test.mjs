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
  assert.match(source, /connectGoogleCalendar/);
  assert.match(source, /requestCloudOtp/);
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
    "google_calendar_connect",
    "google_calendar_disconnect",
    "google_calendar_sync",
    "cloud_connection_status",
    "cloud_status",
    "cloud_request_otp",
    "otp_request",
    "cloud_verify_otp",
    "otp_verify",
    "cloud_disconnect",
    "cloud_process_outbox"
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
  for (const name of [
    "getGoogleCalendarStatus",
    "connectGoogleCalendar",
    "disconnectGoogleCalendar",
    "syncGoogleCalendar",
    "getCloudConnectionStatus",
    "requestCloudOtp",
    "verifyCloudOtp",
    "disconnectCloud",
    "processCloudOutbox"
  ]) {
    assert.match(source, new RegExp(`export async function ${name}`));
  }
  const appSource = await readFile(new URL("../src/App.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(source, /accessToken/);
  assert.doesNotMatch(source, /refreshToken/);
  assert.doesNotMatch(source, /clientSecret/);
  assert.doesNotMatch(source, /publishableKey/);
  assert.doesNotMatch(source, /fetch\(/);
  assert.doesNotMatch(source, /httpRequest/);
  assert.doesNotMatch(appSource, /accessToken/);
  assert.doesNotMatch(appSource, /refreshToken/);
  assert.doesNotMatch(appSource, /clientSecret/);
  assert.doesNotMatch(appSource, /publishableKey/);
  assert.doesNotMatch(appSource, /fetch\(/);
});
