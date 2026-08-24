import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

test("React shell keeps the required navigation and appointment action", async () => {
  const source = await readFile(new URL("../src/App.tsx", import.meta.url), "utf8");

  for (const label of ["Bugun", "Takvim", "Musteriler", "Hizmetler", "Personel", "Ayarlar"]) {
    assert.match(source, new RegExp(`"${label}"`));
  }
  assert.match(source, /\+ Yeni Randevu/);
});

test("Tauri Rust boundary exposes only named foundation commands", async () => {
  const source = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  assert.match(source, /app_health/);
  assert.match(source, /create_foundation_note/);
  assert.match(source, /list_foundation_notes/);
  assert.doesNotMatch(source, /read_file/);
  assert.doesNotMatch(source, /execute_sql/);
});
