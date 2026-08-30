import { invoke } from "@tauri-apps/api/core";

export interface AppHealth {
  ok: boolean;
  appVersion: string;
  schemaVersion: number;
  databasePath: string;
  message: string;
}

export interface AppointmentSummary {
  id: string;
  customerId: string;
  staffId: string;
  startAtUtc: string;
  endAtUtc: string;
  totalDurationMinutes: number;
  status: string;
  note: string | null;
  customerName: string;
  customerPhone: string;
  staffName: string;
  serviceNames: string[];
  updatedAt: string;
}

export interface CustomerInput {
  firstName: string;
  lastName: string;
  phone: string;
  email?: string | null;
  notes?: string | null;
  whatsappReminderEnabled?: boolean;
  whatsappConsentConfirmed?: boolean;
}

export interface StaffInput {
  firstName: string;
  lastName?: string | null;
  phone?: string | null;
  specialtyNote?: string | null;
  colorKey: string;
  isActive?: boolean;
}

export interface CategoryInput {
  name: string;
  isActive?: boolean;
}

export interface ServiceInput {
  categoryId: string;
  name: string;
  durationMinutes?: number | null;
  isActive?: boolean;
}

export interface AppointmentInput {
  customerId: string;
  staffId: string;
  localDate: string;
  localStartTime: string;
  serviceIds: string[];
  status?: string;
  note?: string | null;
}

export interface GoogleConnectionStatus {
  configured: boolean;
  connected: boolean;
  pendingSyncCount: number;
  blockedSyncCount: number;
}

export interface CloudConnectionStatus {
  configured: boolean;
  sessionPresent: boolean;
}

export interface DispatcherStatus {
  paused: boolean;
  pauseReason: string | null;
}

export interface GoogleConnectResult {
  connected: boolean;
  calendarId: string;
}

export interface CloudSyncStatus {
  processed: number;
  blocked: number;
  unauthorized: boolean;
}

export async function getAppHealth(): Promise<AppHealth> {
  return invoke<AppHealth>("app_health");
}

export async function listAppointmentsByDate(localDate: string): Promise<AppointmentSummary[]> {
  return invoke<AppointmentSummary[]>("appointment_list_by_date", { localDate, limit: 20 });
}

export async function getGoogleCalendarStatus(): Promise<GoogleConnectionStatus> {
  return invoke<GoogleConnectionStatus>("google_calendar_status");
}

export async function connectGoogleCalendar(): Promise<GoogleConnectResult> {
  return invoke<GoogleConnectResult>("google_calendar_connect");
}

export async function disconnectGoogleCalendar(): Promise<boolean> {
  return invoke<boolean>("google_calendar_disconnect");
}

export async function syncGoogleCalendar(): Promise<number> {
  return invoke<number>("google_calendar_sync");
}

export async function getCloudConnectionStatus(): Promise<CloudConnectionStatus> {
  return invoke<CloudConnectionStatus>("cloud_connection_status");
}

export async function getDispatcherStatus(): Promise<DispatcherStatus> {
  return invoke<DispatcherStatus>("dispatcher_status");
}

export async function requestCloudOtp(email: string): Promise<boolean> {
  return invoke<boolean>("cloud_request_otp", { email });
}

export async function verifyCloudOtp(email: string, otp: string): Promise<CloudConnectionStatus> {
  return invoke<CloudConnectionStatus>("cloud_verify_otp", { email, otp });
}

export async function disconnectCloud(): Promise<boolean> {
  return invoke<boolean>("cloud_disconnect");
}

export async function processCloudOutbox(): Promise<CloudSyncStatus> {
  return invoke<CloudSyncStatus>("cloud_process_outbox");
}

export const coreDataCommands = [
  "customer_create",
  "customer_update",
  "customer_set_active",
  "customer_search",
  "staff_create",
  "staff_update",
  "staff_set_active",
  "staff_list",
  "staff_set_services",
  "service_category_create",
  "service_create",
  "service_update",
  "service_set_active",
  "service_list",
  "appointment_create",
  "appointment_update",
  "appointment_list_by_date",
  "google_calendar_status",
  "google_calendar_connect",
  "google_calendar_disconnect",
  "google_calendar_sync",
  "cloud_connection_status",
  "cloud_status",
  "dispatcher_status",
  "cloud_request_otp",
  "otp_request",
  "cloud_verify_otp",
  "otp_verify",
  "cloud_disconnect",
  "cloud_process_outbox"
] as const;
