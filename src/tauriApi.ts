import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

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
  whatsappReminderEnabled: boolean;
  whatsappReminderEffective: boolean;
  customerName: string;
  customerPhone: string | null;
  staffName: string;
  serviceNames: string[];
  services: AppointmentServiceSnapshot[];
  updatedAt: string;
}

export interface AppointmentServiceSnapshot {
  serviceId: string;
  serviceNameSnapshot: string;
  durationMinutesSnapshot: number;
  listedPriceSnapshotMinor: number;
  chargedPriceMinor: number;
  sortOrder: number;
}

export interface CustomerInput {
  firstName: string;
  lastName: string;
  phone?: string | null;
  email?: string | null;
  notes?: string | null;
  whatsappReminderEnabled?: boolean;
  whatsappConsentConfirmed?: boolean;
}

export interface Customer {
  id: string;
  firstName: string;
  lastName: string;
  phone: string | null;
  isActive: boolean;
}

export interface CustomerHistoryCustomer {
  customerId: string;
  name: string;
  phone: string | null;
  isActive: boolean;
}

export interface CustomerHistoryAppointment {
  appointmentId: string;
  localDate: string;
  localTime: string;
  status: string;
  staffId: string;
  staffDisplayName: string;
  services: AppointmentServiceSnapshot[];
  totalChargedMinor: number;
}

export interface CustomerHistoryPage {
  customer: CustomerHistoryCustomer;
  appointments: CustomerHistoryAppointment[];
  limit: number;
  offset: number;
}

export interface RepeatBookingSeedService {
  serviceId: string;
  serviceNameSnapshot: string;
  isEligible: boolean;
  unavailableReason: string | null;
}

export interface RepeatBookingSeed {
  customerId: string;
  customerDisplayName: string;
  staffId: string | null;
  services: RepeatBookingSeedService[];
}

export interface Staff {
  id: string;
  firstName: string;
  lastName: string | null;
  phone: string | null;
  specialtyNote: string | null;
  colorKey: string;
  sortOrder: number;
  isActive: boolean;
}

export interface ServiceItem {
  id: string;
  categoryId: string;
  categoryName: string;
  name: string;
  durationMinutes: number | null;
  defaultPriceMinor: number;
  sortOrder: number;
  isActive: boolean;
  availabilityStatus: string;
}

export interface ServiceStatistic {
  serviceId: string;
  serviceNameSnapshot: string;
  completedCount: number;
  revenueMinor: number;
}

export interface ServiceCategory {
  id: string;
  name: string;
  sortOrder: number;
  isActive: boolean;
}

export interface ServiceCategoryInput {
  name: string;
  isActive?: boolean;
}
export interface StaffWorkingHour {
  staffId: string;
  weekday: number;
  startMinute: number;
  endMinute: number;
}
export interface StaffTimeOff {
  id: string;
  staffId: string;
  localDate: string;
  fullDay: boolean;
  startMinute: number | null;
  endMinute: number | null;
}
export interface StaffTimeOffInput {
  localDate: string;
  fullDay: boolean;
  startMinute: number | null;
  endMinute: number | null;
}
export interface BusinessProfile {
  businessName: string;
  phone: string | null;
  email: string | null;
  address: string | null;
  currencyCode: string;
  themeKey: string;
  logoData: number[] | null;
  logoMimeType: string | null;
  updatedAt: string;
}

export interface OnboardingState {
  needsOnboarding: boolean;
  nextStep: number;
}

export interface BusinessProfileInput {
  businessName: string;
  phone: string | null;
  email: string | null;
  address: string | null;
  currencyCode: string;
  themeKey: string;
  logoData: number[] | null;
  logoMimeType: string | null;
}

export interface DatabaseBackupSummary {
  databasePath: string;
  backupPath: string;
  integrity: string;
  machineBoundSecretsRemoved: boolean;
  createdAtUtc: string;
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
  defaultPriceMinor?: number;
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
  whatsappReminderEnabled?: boolean;
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
  validationState?: "valid" | "disconnected" | "unavailable";
}

export interface ReminderReadinessStatus {
  state: "disconnected" | "connected_not_ready" | "ready";
  automaticEnabled: boolean;
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

export async function getBusinessProfile(): Promise<BusinessProfile> {
  return invoke<BusinessProfile>("get_business_profile");
}

export async function getOnboardingState(): Promise<OnboardingState> {
  return invoke<OnboardingState>("get_onboarding_state");
}

export async function updateBusinessProfile(
  input: BusinessProfileInput,
): Promise<BusinessProfile> {
  return invoke<BusinessProfile>("update_business_profile", { input });
}

export async function createManualBackup(): Promise<DatabaseBackupSummary> {
  return invoke<DatabaseBackupSummary>("database_create_backup");
}

export async function restoreDatabaseBackup(backupPath: string): Promise<void> {
  return invoke<void>("database_restore_backup", { backupPath });
}

export async function openDataFolder(): Promise<void> {
  return invoke<void>("open_data_folder");
}

export async function selectBackupFile(): Promise<string | null> {
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "BeautySaloon yedeği", extensions: ["sqlite", "db"] }],
  });
  return typeof selected === "string" ? selected : null;
}

export async function listAppointmentsByDate(
  localDate: string,
): Promise<AppointmentSummary[]> {
  return invoke<AppointmentSummary[]>("appointment_list_by_date", {
    localDate,
    limit: 20,
  });
}

export async function listAppointmentsByDateRange(
  startDate: string,
  endDate: string,
): Promise<AppointmentSummary[]> {
  return invoke<AppointmentSummary[]>("appointment_list_by_date_range", {
    startDate,
    endDate,
    limit: 500,
  });
}

export async function searchCustomers(search: string): Promise<Customer[]> {
  return invoke<Customer[]>("customer_search", {
    search,
    status: "active",
    limit: 12,
  });
}

export async function searchCustomersByStatus(
  search: string,
  status: "active" | "inactive" | "all",
): Promise<Customer[]> {
  return invoke<Customer[]>("customer_search", { search, status, limit: 50 });
}

export async function getCustomerHistory(
  customerId: string,
  limit = 20,
  offset = 0,
): Promise<CustomerHistoryPage> {
  return invoke<CustomerHistoryPage>("customer_history", {
    customerId,
    limit,
    offset,
  });
}

export async function getRepeatBookingSeed(
  customerId: string,
  sourceAppointmentId: string,
): Promise<RepeatBookingSeed> {
  return invoke<RepeatBookingSeed>("get_repeat_booking_seed", {
    customerId,
    sourceAppointmentId,
  });
}

export async function archiveCustomer(id: string): Promise<Customer> {
  return invoke<Customer>("customer_archive", { id });
}

export async function reactivateCustomer(id: string): Promise<Customer> {
  return invoke<Customer>("customer_reactivate", { id });
}

export async function createCustomer(input: CustomerInput): Promise<Customer> {
  return invoke<Customer>("customer_create", { input });
}

export async function listActiveStaff(): Promise<Staff[]> {
  return invoke<Staff[]>("staff_list", { status: "active", limit: 100 });
}

export async function listActiveServices(): Promise<ServiceItem[]> {
  return invoke<ServiceItem[]>("service_list", { status: "active" });
}

export async function listServices(
  status: "active" | "inactive" | "all" = "active",
): Promise<ServiceItem[]> {
  return invoke<ServiceItem[]>("service_list", { status });
}
export async function listServiceCategories(): Promise<ServiceCategory[]> {
  return invoke<ServiceCategory[]>("service_category_list", {
    status: "active",
  });
}
export async function createServiceCategory(
  input: ServiceCategoryInput,
): Promise<ServiceCategory> {
  return invoke<ServiceCategory>("service_category_create", { input });
}
export async function createService(input: ServiceInput): Promise<ServiceItem> {
  return invoke<ServiceItem>("service_create", { input });
}
export async function updateService(
  id: string,
  input: ServiceInput,
): Promise<ServiceItem> {
  return invoke<ServiceItem>("service_update", { id, input });
}
export async function deactivateService(id: string): Promise<ServiceItem> {
  return invoke<ServiceItem>("service_deactivate", { id });
}
export async function reactivateService(id: string): Promise<ServiceItem> {
  return invoke<ServiceItem>("service_reactivate", { id });
}

export async function serviceStatistics(
  startDate: string,
  endDate: string,
): Promise<ServiceStatistic[]> {
  return invoke<ServiceStatistic[]>("service_statistics", {
    startDate,
    endDate,
  });
}

export async function listStaff(
  status: "active" | "inactive" | "all" = "active",
): Promise<Staff[]> {
  return invoke<Staff[]>("staff_list", { status, limit: 100 });
}
export async function createStaff(input: StaffInput): Promise<Staff> {
  return invoke<Staff>("staff_create", { input });
}
export async function updateStaff(
  id: string,
  input: StaffInput,
): Promise<Staff> {
  return invoke<Staff>("staff_update", { id, input });
}
export async function deactivateStaff(id: string): Promise<Staff> {
  return invoke<Staff>("staff_deactivate", { id });
}
export async function reactivateStaff(id: string): Promise<Staff> {
  return invoke<Staff>("staff_reactivate", { id });
}
export async function listStaffServiceIds(staffId: string): Promise<string[]> {
  return invoke<string[]>("staff_services_list", { staffId });
}
export async function setStaffServices(
  staffId: string,
  serviceIds: string[],
): Promise<string[]> {
  return invoke<string[]>("staff_set_services", { staffId, serviceIds });
}
export async function listStaffWorkingHours(
  staffId: string,
): Promise<StaffWorkingHour[]> {
  return invoke<StaffWorkingHour[]>("staff_working_hours_list", { staffId });
}
export async function setStaffWorkingHours(
  staffId: string,
  intervals: StaffWorkingHour[],
): Promise<void> {
  return invoke<void>("staff_working_hours_set", { staffId, intervals });
}
export async function isStaffScheduleUnrestricted(
  staffId: string,
): Promise<boolean> {
  return invoke<boolean>("staff_schedule_is_unrestricted", { staffId });
}
export async function listStaffTimeOff(
  staffId: string,
): Promise<StaffTimeOff[]> {
  return invoke<StaffTimeOff[]>("staff_time_off_list", { staffId });
}
export async function addStaffTimeOff(
  staffId: string,
  input: StaffTimeOffInput,
): Promise<StaffTimeOff> {
  return invoke<StaffTimeOff>("staff_time_off_add", { staffId, input });
}
export async function updateStaffTimeOff(
  staffId: string,
  timeOffId: string,
  input: StaffTimeOffInput,
): Promise<StaffTimeOff> {
  return invoke<StaffTimeOff>("staff_time_off_update", {
    staffId,
    timeOffId,
    input,
  });
}
export async function removeStaffTimeOff(
  staffId: string,
  timeOffId: string,
): Promise<void> {
  return invoke<void>("staff_time_off_remove", { staffId, timeOffId });
}

export async function createAppointment(
  input: AppointmentInput,
): Promise<AppointmentSummary> {
  return invoke<AppointmentSummary>("appointment_create", { input });
}

export async function updateAppointment(
  id: string,
  input: AppointmentInput,
): Promise<AppointmentSummary> {
  return invoke<AppointmentSummary>("appointment_update", { id, input });
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

export async function getReminderReadiness(): Promise<ReminderReadinessStatus> {
  return invoke<ReminderReadinessStatus>("reminder_readiness");
}

export async function setReminderAutomaticEnabled(
  enabled: boolean,
): Promise<ReminderReadinessStatus> {
  return invoke<ReminderReadinessStatus>("reminder_set_automatic_enabled", {
    enabled,
  });
}

export async function getDispatcherStatus(): Promise<DispatcherStatus> {
  return invoke<DispatcherStatus>("dispatcher_status");
}

export async function requestCloudOtp(email: string): Promise<boolean> {
  return invoke<boolean>("cloud_request_otp", { email });
}

export async function verifyCloudOtp(
  email: string,
  otp: string,
): Promise<CloudConnectionStatus> {
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
  "appointment_list_by_date_range",
  "google_calendar_status",
  "google_calendar_connect",
  "google_calendar_disconnect",
  "google_calendar_sync",
  "cloud_connection_status",
  "reminder_readiness",
  "reminder_set_automatic_enabled",
  "cloud_status",
  "dispatcher_status",
  "cloud_request_otp",
  "otp_request",
  "cloud_verify_otp",
  "otp_verify",
  "cloud_disconnect",
  "cloud_process_outbox",
] as const;
