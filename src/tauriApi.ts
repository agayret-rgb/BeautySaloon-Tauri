import { invoke } from "@tauri-apps/api/core";

export interface AppHealth {
  ok: boolean;
  appVersion: string;
  schemaVersion: number;
  databasePath: string;
  message: string;
}

export interface FoundationNote {
  id: number;
  title: string;
  createdAtUtc: string;
}

export async function getAppHealth(): Promise<AppHealth> {
  return invoke<AppHealth>("app_health");
}

export async function createFoundationNote(title: string): Promise<FoundationNote> {
  return invoke<FoundationNote>("create_foundation_note", { title });
}

export async function listFoundationNotes(limit = 10): Promise<FoundationNote[]> {
  return invoke<FoundationNote[]>("list_foundation_notes", { limit });
}
