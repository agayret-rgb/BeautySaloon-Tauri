import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

test("React shell exposes exactly the six normal-user workspaces and real booking action", async () => {
  const source = await readFile(new URL("../src/App.tsx", import.meta.url), "utf8");
  for (const label of ["Bugün", "Takvim", "Müşteriler", "Hizmetler", "Personel", "Ayarlar"]) assert.match(source, new RegExp(`"${label}"`));
  assert.match(source, /const navigationItems = \[/);
  assert.match(source, /\+ Yeni Randevu/);
  for (const label of ["Müşteri adı", "Telefon", "Hizmet", "Personel", "Tarih", "Saat", "Randevuyu Kaydet"]) assert.match(source, new RegExp(label));
  assert.match(source, /createCustomer/);
  assert.match(source, /createAppointment/);
  assert.doesNotMatch(source, /Supabase|OAuth|dispatcher|accessToken|clientSecret|publishableKey/);
});

test("Frontend booking API uses bounded typed reads and named mutations", async () => {
  const source = await readFile(new URL("../src/tauriApi.ts", import.meta.url), "utf8");
  for (const name of ["searchCustomers", "createCustomer", "listActiveStaff", "listActiveServices", "createAppointment"]) assert.match(source, new RegExp(`export async function ${name}`));
  assert.match(
    source,
    /customer_search",\s*\{\s*search,\s*status:\s*"active",\s*limit:\s*12,?\s*\}/,
  );
  assert.doesNotMatch(source, /executeSql|execute_sql|fetch\(/);
});

test("Settings exposes only normal-user business and backup actions", async () => {
  const source = await readFile(new URL("../src/App.tsx", import.meta.url), "utf8");
  for (const section of ["İşletme", "Veri ve Yedekleme", "Yedek Oluştur", "Yedekten Geri Yükle", "Veri Klasörünü Aç"]) assert.match(source, new RegExp(section));
  assert.doesNotMatch(source, /Google Calendar|Bulut|Supabase|OAuth|dispatcher|raw SQL|service_role|token/i);
});

test("Tauri Rust boundary remains named and does not expose generic SQL", async () => {
  const source = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  for (const command of ["app_health", "appointment_create", "database_create_backup", "database_restore_backup"]) assert.match(source, new RegExp(command));
  assert.doesNotMatch(source, /execute_sql/);
  assert.doesNotMatch(source, /executeSql/);
});
